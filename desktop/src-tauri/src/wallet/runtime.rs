//! Per-community wallet session: facade + confirm-handle registry.

use crate::wallet::error::map_wallet_error;
use buzz_core_pkg::payment::{Amount, PaymentRequest, PaymentTarget};
use buzz_wallet_pkg::{
    decode_bolt11, AttemptKey, Bolt11, Clock, ConfirmHandle, IncomingStatus, LnurlResolver,
    PaymentStore, PersistedPaymentState, ProfilePublisher, ReceiveMode, SecretStore, SendOutcome,
    SendTarget, SettledAttempt, Wallet, WalletConnector, WalletTimeouts,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Driver-level error when no NWC secret is stored for the active community.
pub const NOT_LINKED: &str = "not_linked";

/// Webview-safe wallet status — structurally cannot carry an NWC URI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletStatusView {
    /// Whether a secret is stored for this community.
    pub linked: bool,
    /// Capability method names (e.g. `pay_invoice`).
    pub capabilities: Vec<String>,
    /// Receive presentation mode.
    pub receive_mode: String,
    /// Static Lightning Address when available.
    pub lud16: Option<String>,
    /// Balance in msat when the linked wallet exposes `get_balance`.
    pub balance_msat: Option<u64>,
}

/// Quote returned from prepare_send — handle id, not a bypassable pay token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrepareSendQuote {
    /// Opaque id for confirm/cancel (native registry only).
    pub handle_id: String,
    /// Amount bound at prepare (msat).
    pub amount_msat: u64,
    /// Bolt11 for display / QR — confirm still requires the stored handle.
    pub bolt11: String,
    /// Hex payment hash.
    pub payment_hash: String,
    /// Invoice expiry (unix seconds).
    pub expires_at_unix: u64,
    /// Human-readable target (lud16) when applicable.
    pub target_description: Option<String>,
}

/// Minted receive invoice — bolt11 plus mint-time hash and expiry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiveInvoice {
    /// Opaque bolt11 for QR / copy.
    pub bolt11: String,
    /// Hex payment hash (for kind-40009 payment-request events).
    pub payment_hash: String,
    /// Invoice expiry (unix seconds).
    pub expires_at_unix: u64,
}

/// Serializable confirm outcome for the webview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SendConfirmOutcome {
    /// Settled with preimage.
    Settled {
        /// Hex preimage.
        preimage: String,
    },
    /// Definitive failure.
    Failed {
        /// Mapped error code.
        reason: String,
    },
    /// Unknown — reconcile later.
    Unknown,
    /// Already latched.
    AlreadyClaimed {
        /// Persisted state name.
        state: String,
    },
}

/// Local settlement check for an incoming payment request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IncomingCheckOutcome {
    /// Own wallet reports the request's payment_hash as settled.
    Paid,
    /// Request carries a bolt11 whose hash is not settled in this wallet.
    Unpaid,
    /// `lud16`-only request — no hash the payee can look up.
    Unconfirmable,
}

/// Minimal payment-request target for [`Wallet::check_incoming`].
#[derive(Debug, Clone, Deserialize)]
pub struct CheckIncomingDto {
    /// Optional bolt11 from the kind-40009 request.
    pub bolt11: Option<String>,
    /// Optional Lightning Address from the kind-40009 request.
    pub lud16: Option<String>,
}

/// A pay-request attempt that newly settled during reconcile (receipt-relevant).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettledPayRequestDto {
    /// Kind-40009 request event id.
    pub request_event_id: String,
    /// Hex payment hash.
    pub payment_hash: String,
    /// Preimage when known from lookup / settle.
    pub preimage: Option<String>,
    /// Amount in millisatoshis.
    pub amount_msat: u64,
}

/// Send target from the webview.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SendTargetDto {
    /// Lightning Address.
    Lud16 {
        /// Address string.
        address: String,
    },
    /// Raw bolt11.
    Bolt11 {
        /// Invoice string.
        invoice: String,
    },
}

/// Attempt key from the webview.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttemptKeyDto {
    /// Fresh standalone send.
    Standalone,
    /// Pay-card keyed by request event id.
    PayRequest {
        /// Kind 40009 event id.
        event_id: String,
    },
}

impl From<SendTargetDto> for SendTarget {
    fn from(value: SendTargetDto) -> Self {
        match value {
            SendTargetDto::Lud16 { address } => SendTarget::Lud16(address),
            SendTargetDto::Bolt11 { invoice } => SendTarget::Bolt11(Bolt11::new(invoice)),
        }
    }
}

impl From<AttemptKeyDto> for AttemptKey {
    fn from(value: AttemptKeyDto) -> Self {
        match value {
            AttemptKeyDto::Standalone => AttemptKey::Standalone,
            AttemptKeyDto::PayRequest { event_id } => AttemptKey::PayRequest { event_id },
        }
    }
}

fn receive_mode_label(mode: &ReceiveMode) -> String {
    match mode {
        ReceiveMode::StaticAddress(_) => "static_address".to_string(),
        ReceiveMode::Interactive => "interactive".to_string(),
        ReceiveMode::Unavailable => "unavailable".to_string(),
    }
}

fn persisted_state_label(state: PersistedPaymentState) -> String {
    match state {
        PersistedPaymentState::Paying => "paying".to_string(),
        PersistedPaymentState::Settled => "settled".to_string(),
        PersistedPaymentState::Failed => "failed".to_string(),
        PersistedPaymentState::Unknown => "unknown".to_string(),
    }
}

/// Injected ports retained so unlink can rebuild a fresh unconnected facade.
pub struct WalletPorts {
    /// NWC connector (production or fake).
    pub connector: Arc<dyn WalletConnector>,
    /// Community-scoped secret store.
    pub secrets: Arc<dyn SecretStore>,
    /// Kind:0 lud16 publisher.
    pub profiles: Arc<dyn ProfilePublisher>,
    /// LNURL resolver.
    pub resolver: Arc<dyn LnurlResolver>,
    /// Durable payment store.
    pub store: Arc<dyn PaymentStore>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// Connect / pay-response timeouts.
    pub timeouts: WalletTimeouts,
}

impl WalletPorts {
    /// Construct a facade with no live [`WalletService`](buzz_wallet_pkg::WalletService).
    pub fn build_wallet(&self) -> Wallet {
        Wallet::new(
            Arc::clone(&self.connector),
            Arc::clone(&self.secrets),
            Arc::clone(&self.profiles),
            Arc::clone(&self.resolver),
            Arc::clone(&self.store),
            Arc::clone(&self.clock),
            self.timeouts,
        )
    }
}

/// One community's live wallet + confirm-handle registry.
pub struct WalletRuntime {
    community_id: String,
    ports: WalletPorts,
    /// Swappable facade slot — unlink replaces this with a fresh unconnected wallet.
    wallet: Mutex<Arc<Wallet>>,
    /// Opaque handle id → bound confirm handle. One map — not scattered Options.
    handles: Mutex<HashMap<String, ConfirmHandle>>,
    /// True after a successful `link` in this process session.
    connected: AtomicBool,
}

impl WalletRuntime {
    /// Build a runtime for `community_id` over injectable ports.
    pub fn new(community_id: String, ports: WalletPorts) -> Self {
        let wallet = Arc::new(ports.build_wallet());
        Self {
            community_id,
            ports,
            wallet: Mutex::new(wallet),
            handles: Mutex::new(HashMap::new()),
            connected: AtomicBool::new(false),
        }
    }

    /// Community this session is bound to.
    pub fn community_id(&self) -> &str {
        &self.community_id
    }

    /// Durable payment store (test inspection).
    #[cfg(test)]
    pub fn store(&self) -> &Arc<dyn PaymentStore> {
        &self.ports.store
    }

    fn wallet(&self) -> Arc<Wallet> {
        Arc::clone(&self.wallet.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// One gate for pay/receive: stored-secret presence, not the `connected` flag.
    async fn require_linked(&self) -> Result<(), String> {
        match self.ports.secrets.load().await.map_err(map_wallet_error)? {
            Some(_) => Ok(()),
            None => Err(NOT_LINKED.to_string()),
        }
    }

    /// Link from an NWC URI. Returns status — never the URI.
    pub async fn link(&self, uri: &str) -> Result<WalletStatusView, String> {
        self.wallet().link(uri).await.map_err(map_wallet_error)?;
        self.connected.store(true, Ordering::Release);
        self.status().await
    }

    /// Clear the secret, drop handles, and replace the facade with a fresh
    /// unconnected [`Wallet`] so no stale `WalletService` can survive.
    pub async fn unlink(&self) -> Result<(), String> {
        self.ports.secrets.clear().await.map_err(map_wallet_error)?;
        self.connected.store(false, Ordering::Release);
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Structural teardown — stale live service is unrepresentable after this.
        *self.wallet.lock().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(self.ports.build_wallet());
        Ok(())
    }

    /// Reconnect from the stored secret when present (no-op if unlinked).
    ///
    /// Skips the network round-trip when this session already linked.
    pub async fn ensure_connected(&self) -> Result<(), String> {
        if self.connected.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(secret) = self.ports.secrets.load().await.map_err(map_wallet_error)? else {
            return Ok(());
        };
        // `link` is the only public path that installs a live WalletService.
        self.wallet()
            .link(&secret.uri)
            .await
            .map_err(map_wallet_error)?;
        self.connected.store(true, Ordering::Release);
        Ok(())
    }

    /// Status view — never includes `nostr+walletconnect` material.
    pub async fn status(&self) -> Result<WalletStatusView, String> {
        let secret = self.ports.secrets.load().await.map_err(map_wallet_error)?;
        let Some(secret) = secret else {
            return Ok(WalletStatusView {
                linked: false,
                capabilities: Vec::new(),
                receive_mode: "unavailable".to_string(),
                lud16: None,
                balance_msat: None,
            });
        };

        let capabilities: Vec<String> = secret
            .capabilities
            .iter()
            .map(|m| m.as_str().to_string())
            .collect();
        // receive_mode is offline — decided from the persisted secret only.
        let receive_mode = self
            .wallet()
            .receive_mode()
            .await
            .map_err(map_wallet_error)?;
        let lud16 = match &receive_mode {
            ReceiveMode::StaticAddress(addr) => Some(addr.clone()),
            _ => secret.lud16.clone(),
        };

        // Best-effort balance: soft-fail to None when reconnect/capability fails.
        let balance_msat = {
            let _ = self.ensure_connected().await;
            match self.wallet().balance().await {
                Ok(amount) => amount.map(|a| a.as_msat()),
                Err(_) => None,
            }
        };

        Ok(WalletStatusView {
            linked: true,
            capabilities,
            receive_mode: receive_mode_label(&receive_mode),
            lud16,
            balance_msat,
        })
    }

    /// Interactive receive → bolt11 + payment_hash + expiry.
    pub async fn receive(
        &self,
        amount_msat: u64,
        description: Option<&str>,
    ) -> Result<ReceiveInvoice, String> {
        self.require_linked().await?;
        self.ensure_connected().await?;
        let bolt11 = self
            .wallet()
            .receive(Amount::from_msat(amount_msat), description)
            .await
            .map_err(map_wallet_error)?;
        let decoded = decode_bolt11(&bolt11).map_err(map_wallet_error)?;
        Ok(ReceiveInvoice {
            bolt11: bolt11.into_inner(),
            payment_hash: decoded.payment_hash_hex,
            expires_at_unix: decoded.expires_at_unix,
        })
    }

    /// Prepare send and stash the confirm handle under an opaque id.
    pub async fn prepare_send(
        &self,
        target: SendTargetDto,
        amount_msat: u64,
        attempt: AttemptKeyDto,
        memo: Option<&str>,
    ) -> Result<PrepareSendQuote, String> {
        self.require_linked().await?;
        self.ensure_connected().await?;
        let target_description = match &target {
            SendTargetDto::Lud16 { address } => Some(address.clone()),
            SendTargetDto::Bolt11 { .. } => None,
        };
        let handle = self
            .wallet()
            .prepare_send(
                attempt.into(),
                target.into(),
                Amount::from_msat(amount_msat),
                memo,
            )
            .await
            .map_err(map_wallet_error)?;

        let handle_id = Uuid::new_v4().to_string();
        let quote = PrepareSendQuote {
            handle_id: handle_id.clone(),
            amount_msat: handle.amount().as_msat(),
            bolt11: handle.bolt11().as_str().to_string(),
            payment_hash: handle.payment_hash().to_string(),
            expires_at_unix: handle.expires_at_unix(),
            target_description,
        };
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle_id, handle);
        Ok(quote)
    }

    /// Confirm by opaque handle id.
    pub async fn confirm(&self, handle_id: &str) -> Result<SendConfirmOutcome, String> {
        self.require_linked().await?;
        {
            let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            if !handles.contains_key(handle_id) {
                return Err("unknown_handle".to_string());
            }
        }
        self.ensure_connected().await?;
        let handle = {
            let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            handles
                .remove(handle_id)
                .ok_or_else(|| "unknown_handle".to_string())?
        };
        let result = self.wallet().confirm(&handle).await;
        // Keep the handle so a second confirm can hit AlreadyClaimed.
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle_id.to_string(), handle);
        let outcome = result.map_err(map_wallet_error)?;
        Ok(match outcome {
            SendOutcome::Settled { preimage } => SendConfirmOutcome::Settled { preimage },
            SendOutcome::Failed { reason } => SendConfirmOutcome::Failed {
                reason: map_wallet_error(reason),
            },
            SendOutcome::Unknown => SendConfirmOutcome::Unknown,
            SendOutcome::AlreadyClaimed { state } => SendConfirmOutcome::AlreadyClaimed {
                state: persisted_state_label(state),
            },
        })
    }

    /// Cancel by opaque handle id — removes the handle.
    pub fn cancel(&self, handle_id: &str) -> Result<(), String> {
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        let Some(handle) = handles.remove(handle_id) else {
            return Err("unknown_handle".to_string());
        };
        self.wallet().cancel(handle);
        Ok(())
    }

    /// Drive [`Wallet::reconcile`]. No-op when unlinked.
    ///
    /// Returns newly settled **pay_request** attempts (standalone sends omitted).
    pub async fn reconcile(&self) -> Result<Vec<SettledPayRequestDto>, String> {
        if self
            .ports
            .secrets
            .load()
            .await
            .map_err(map_wallet_error)?
            .is_none()
        {
            return Ok(Vec::new());
        }
        self.ensure_connected().await?;
        let settled = self.wallet().reconcile().await.map_err(map_wallet_error)?;
        Ok(settled
            .into_iter()
            .filter_map(settled_pay_request_dto)
            .collect())
    }

    /// Local check that an incoming payment request settled in this wallet.
    pub async fn check_incoming(
        &self,
        dto: CheckIncomingDto,
    ) -> Result<IncomingCheckOutcome, String> {
        self.require_linked().await?;
        self.ensure_connected().await?;
        let request = payment_request_from_check_dto(dto)?;
        let status = self
            .wallet()
            .check_incoming(&request)
            .await
            .map_err(map_wallet_error)?;
        Ok(match status {
            IncomingStatus::Paid => IncomingCheckOutcome::Paid,
            IncomingStatus::Unpaid => IncomingCheckOutcome::Unpaid,
            IncomingStatus::Unconfirmable => IncomingCheckOutcome::Unconfirmable,
        })
    }
}

fn settled_pay_request_dto(attempt: SettledAttempt) -> Option<SettledPayRequestDto> {
    match attempt.attempt {
        AttemptKey::PayRequest { event_id } => Some(SettledPayRequestDto {
            request_event_id: event_id,
            payment_hash: attempt.payment_hash,
            preimage: attempt.preimage,
            amount_msat: attempt.amount_msat,
        }),
        AttemptKey::Standalone => None,
    }
}

/// Build the minimal [`PaymentRequest`] `check_incoming` needs from IPC fields.
fn payment_request_from_check_dto(dto: CheckIncomingDto) -> Result<PaymentRequest, String> {
    let target = match (dto.bolt11, dto.lud16) {
        (Some(bolt11), Some(lud16)) => PaymentTarget::Both { bolt11, lud16 },
        (Some(bolt11), None) => PaymentTarget::Bolt11(bolt11),
        (None, Some(lud16)) => PaymentTarget::Lud16(lud16),
        (None, None) => return Err("missing_payment_target".to_string()),
    };
    Ok(PaymentRequest {
        // Amount / channel / payee are unused by check_incoming — only target matters.
        amount: Amount::from_msat(1),
        memo: None,
        target,
        channel_id: String::new(),
        payee_pubkey: String::new(),
        expiry: None,
    })
}

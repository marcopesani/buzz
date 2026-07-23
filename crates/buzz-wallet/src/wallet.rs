//! Wallet use-cases: link, receive, send, reconcile, check_incoming.

use crate::bolt11::{payment_hash_hex, validate_payable_bolt11};
use crate::error::WalletError;
use crate::ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
use crate::types::{
    AttemptKey, Bolt11, Capabilities, ClaimOutcome, ConfirmHandle, IncomingStatus, InvoiceStatus,
    Kind0Fields, PersistedPaymentState, ReceiveMode, SendOutcome, SendTarget, StoredSecret,
    WalletHandle, WalletMethod, WalletTimeouts,
};
use buzz_core::payment::{Amount, PaymentRequest, PaymentTarget};
use std::sync::{Arc, Mutex};
use tokio::time::timeout;

/// Injected-port facade for wallet use-cases.
///
/// Holds connector / secret / profile / resolver / store / clock ports plus
/// the live [`WalletService`] after a successful [`link`](Self::link).
pub struct Wallet {
    connector: Arc<dyn WalletConnector>,
    secrets: Arc<dyn SecretStore>,
    profiles: Arc<dyn ProfilePublisher>,
    resolver: Arc<dyn LnurlResolver>,
    store: Arc<dyn PaymentStore>,
    clock: Arc<dyn Clock>,
    timeouts: WalletTimeouts,
    service: Mutex<Option<Arc<dyn WalletService>>>,
}

impl Wallet {
    /// Build a wallet facade with injectable ports and timeouts.
    ///
    /// Tests pass short timeouts and use `tokio::time::pause` so unreachable
    /// / never-respond scenarios advance without sleeping wall time.
    pub fn new(
        connector: Arc<dyn WalletConnector>,
        secrets: Arc<dyn SecretStore>,
        profiles: Arc<dyn ProfilePublisher>,
        resolver: Arc<dyn LnurlResolver>,
        store: Arc<dyn PaymentStore>,
        clock: Arc<dyn Clock>,
        timeouts: WalletTimeouts,
    ) -> Self {
        Self {
            connector,
            secrets,
            profiles,
            resolver,
            store,
            clock,
            timeouts,
            service: Mutex::new(None),
        }
    }

    /// Link a wallet from an NWC URI.
    ///
    /// Connect (with timeout) → persist secret + capabilities (+ lud16) →
    /// optionally merge-publish the connector-reported address into kind:0.
    /// Failure before storage leaves secure storage unchanged.
    pub async fn link(&self, uri: &str) -> Result<WalletHandle, WalletError> {
        let (service, capabilities, lud16) = self.connect_with_timeout(uri).await?;

        let secret = StoredSecret {
            uri: uri.to_string(),
            capabilities: capabilities.clone(),
            lud16: lud16.clone(),
        };
        self.secrets.store(&secret).await?;

        if let Some(ref address) = lud16 {
            self.maybe_publish_lud16(address).await?;
        }

        *self.service.lock().unwrap_or_else(|e| e.into_inner()) = Some(service);

        Ok(WalletHandle {
            capabilities,
            lud16,
        })
    }

    /// Decide Receive presentation from persisted state only (no network).
    ///
    /// Priority: static address → interactive (`make_invoice`) → unavailable.
    pub async fn receive_mode(&self) -> Result<ReceiveMode, WalletError> {
        let secret = self
            .secrets
            .load()
            .await?
            .ok_or(WalletError::SecretUnavailable)?;
        Ok(receive_mode_from_secret(&secret))
    }

    /// Fetch balance (msat) from the linked wallet service.
    ///
    /// Returns [`WalletError::SecretUnavailable`] when nothing is linked
    /// (NotLinked at the driver boundary). `Ok(None)` means the wallet hid
    /// its balance or lacks `get_balance`.
    pub async fn balance(&self) -> Result<Option<Amount>, WalletError> {
        let _secret = self
            .secrets
            .load()
            .await?
            .ok_or(WalletError::SecretUnavailable)?;
        let service = self.live_service()?;
        service.get_balance().await
    }

    /// Interactive receive: mint one bolt11 via `make_invoice`.
    ///
    /// Returns [`WalletError::Unsupported`] when the capability is absent.
    /// Static-address wallets never call this — the UI uses [`receive_mode`].
    pub async fn receive(&self, amount: Amount, memo: Option<&str>) -> Result<Bolt11, WalletError> {
        let secret = self
            .secrets
            .load()
            .await?
            .ok_or(WalletError::SecretUnavailable)?;
        if !secret.capabilities.contains(WalletMethod::MakeInvoice) {
            return Err(WalletError::Unsupported);
        }
        let service = self.live_service()?;
        service.make_invoice(amount, memo).await
    }

    /// Resolve + validate a send target into a confirmation-bound handle.
    ///
    /// Lud16 is resolved to a bolt11 first; every path then shares one
    /// decode/validate step. Nothing is persisted and nothing is paid.
    ///
    /// `attempt` is the latch identity: [`AttemptKey::PayRequest`] shares one
    /// id across taps on the same card; [`AttemptKey::Standalone`] is unique.
    pub async fn prepare_send(
        &self,
        attempt: AttemptKey,
        target: SendTarget,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<ConfirmHandle, WalletError> {
        let bolt11 = self.resolve_to_bolt11(target, amount, memo).await?;
        let validated = validate_payable_bolt11(&bolt11, amount, self.clock.as_ref())?;
        Ok(ConfirmHandle {
            attempt_id: attempt.to_attempt_id(),
            bolt11: validated.bolt11,
            payment_hash: validated.payment_hash_hex,
            amount: validated.amount,
            expires_at_unix: validated.expires_at_unix,
        })
    }

    /// Persist-then-pay the bound invoice exactly once per attempt.
    ///
    /// `claim_paying` latches the attempt before `pay_invoice`. A second
    /// confirm on the same attempt is a no-op at the wallet (`AlreadyClaimed`).
    /// A pay-response timeout persists [`PersistedPaymentState::Unknown`] —
    /// never Failed — and never re-fires `pay_invoice`.
    pub async fn confirm(&self, handle: &ConfirmHandle) -> Result<SendOutcome, WalletError> {
        if self.clock.now_unix() >= handle.expires_at_unix {
            return Err(WalletError::ResolveRejected);
        }

        let claim = self
            .store
            .claim_paying(
                &handle.attempt_id,
                &handle.payment_hash,
                &handle.bolt11,
                handle.amount,
                handle.expires_at_unix,
            )
            .await?;

        match claim {
            ClaimOutcome::AlreadyClaimed(record) => {
                return Ok(SendOutcome::AlreadyClaimed {
                    state: record.state,
                });
            }
            ClaimOutcome::Claimed(_) => {}
        }

        let service = self.live_service()?;
        match timeout(
            self.timeouts.pay_response,
            service.pay_invoice(&handle.bolt11),
        )
        .await
        {
            Ok(Ok(preimage)) => {
                self.store
                    .update_state(&handle.attempt_id, PersistedPaymentState::Settled)
                    .await?;
                Ok(SendOutcome::Settled { preimage })
            }
            Ok(Err(err)) if is_definitive_pay_error(&err) => {
                self.store
                    .update_state(&handle.attempt_id, PersistedPaymentState::Failed)
                    .await?;
                Ok(SendOutcome::Failed { reason: err })
            }
            Ok(Err(WalletError::Unknown)) | Err(_) => {
                self.store
                    .update_state(&handle.attempt_id, PersistedPaymentState::Unknown)
                    .await?;
                Ok(SendOutcome::Unknown)
            }
            Ok(Err(err)) => Err(err),
        }
    }

    /// Drop a prepared send. Consumes the handle so confirm-after-cancel
    /// cannot be expressed. Nothing was persisted; nothing is paid.
    pub fn cancel(&self, handle: ConfirmHandle) {
        drop(handle);
    }

    /// Drain every `Paying` / `Unknown` record via `lookup_invoice`.
    ///
    /// The only exit from Unknown. Never calls `pay_invoice`.
    pub async fn reconcile(&self) -> Result<(), WalletError> {
        let pending = self
            .store
            .list_by_states(&[
                PersistedPaymentState::Paying,
                PersistedPaymentState::Unknown,
            ])
            .await?;
        let service = self.live_service()?;
        let now = self.clock.now_unix();
        for record in pending {
            let status = service.lookup_invoice(&record.payment_hash).await?;
            let next = reconcile_next_state(status, now, record.expires_at_unix);
            if next != record.state {
                self.store.update_state(&record.attempt_id, next).await?;
            }
        }
        Ok(())
    }

    /// Confirm an incoming payment against **this** wallet only.
    ///
    /// Takes a [`PaymentRequest`] — no receipt parameter exists, so a forged
    /// receipt cannot influence the answer by construction.
    pub async fn check_incoming(
        &self,
        request: &PaymentRequest,
    ) -> Result<IncomingStatus, WalletError> {
        let bolt11 = match &request.target {
            PaymentTarget::Bolt11(b) | PaymentTarget::Both { bolt11: b, .. } => {
                Bolt11::new(b.clone())
            }
            PaymentTarget::Lud16(_) => return Ok(IncomingStatus::Unconfirmable),
        };
        let hash = payment_hash_hex(&bolt11)?;
        let service = self.live_service()?;
        match service.lookup_invoice(&hash).await? {
            InvoiceStatus::Settled => Ok(IncomingStatus::Paid),
            InvoiceStatus::Pending
            | InvoiceStatus::Failed
            | InvoiceStatus::Expired
            | InvoiceStatus::NotFound => Ok(IncomingStatus::Unpaid),
        }
    }

    async fn resolve_to_bolt11(
        &self,
        target: SendTarget,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<Bolt11, WalletError> {
        match target {
            SendTarget::Bolt11(bolt11) => Ok(bolt11),
            SendTarget::Lud16(lud16) => {
                let resolved = self.resolver.resolve(&lud16, amount, memo).await?;
                Ok(resolved.bolt11)
            }
        }
    }

    fn live_service(&self) -> Result<Arc<dyn WalletService>, WalletError> {
        self.service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(WalletError::Unreachable)
    }

    async fn connect_with_timeout(
        &self,
        uri: &str,
    ) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError> {
        match timeout(self.timeouts.connect, self.connector.connect(uri)).await {
            Ok(result) => result,
            Err(_) => Err(WalletError::Unreachable),
        }
    }

    /// Publish `lud16` unless the user already set a *different* address.
    async fn maybe_publish_lud16(&self, lud16: &str) -> Result<(), WalletError> {
        match self.profiles.current_lud16().await? {
            Some(existing) if existing != lud16 => Ok(()),
            _ => {
                self.profiles
                    .merge_publish(Kind0Fields {
                        lud16: Some(lud16.to_string()),
                    })
                    .await
            }
        }
    }
}

/// Pure mapping from persisted secret → [`ReceiveMode`].
///
/// Isolated so the decision has no I/O and is obviously complete.
fn receive_mode_from_secret(secret: &StoredSecret) -> ReceiveMode {
    match &secret.lud16 {
        Some(address) if !address.is_empty() => ReceiveMode::StaticAddress(address.clone()),
        _ if secret.capabilities.contains(WalletMethod::MakeInvoice) => ReceiveMode::Interactive,
        _ => ReceiveMode::Unavailable,
    }
}

fn is_definitive_pay_error(err: &WalletError) -> bool {
    matches!(
        err,
        WalletError::PaymentFailed | WalletError::InsufficientBalance | WalletError::QuotaExceeded
    )
}

/// One match: `lookup_invoice` status → next persisted state.
///
/// Pending / NotFound-before-expiry land on Unknown (re-poll). NotFound past
/// invoice expiry is Failed — early fail invites a double pay on retry.
fn reconcile_next_state(
    status: InvoiceStatus,
    now_unix: u64,
    expires_at_unix: u64,
) -> PersistedPaymentState {
    match status {
        InvoiceStatus::Settled => PersistedPaymentState::Settled,
        InvoiceStatus::Failed | InvoiceStatus::Expired => PersistedPaymentState::Failed,
        InvoiceStatus::Pending => PersistedPaymentState::Unknown,
        InvoiceStatus::NotFound if now_unix >= expires_at_unix => PersistedPaymentState::Failed,
        InvoiceStatus::NotFound => PersistedPaymentState::Unknown,
    }
}

#[cfg(test)]
mod tests;

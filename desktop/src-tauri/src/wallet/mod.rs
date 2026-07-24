//! Desktop Lightning wallet driver — Tauri-side wiring for `buzz-wallet`.
//!
//! Owns community-scoped session state, the durable payment store, the
//! keyring-backed NWC secret adapter, and the confirm-handle registry.
//! Commands in [`crate::commands::wallet`] are a thin IPC skin over this
//! module; payment decisions stay in `buzz-wallet`.

mod agent_nwc;
mod error;
mod payment_store;
mod profile;
mod runtime;
mod secret_store;

pub use agent_nwc::{
    agent_nwc_blob_key, agent_nwc_is_provisioned, agent_nwc_os_backend, agent_nwc_redaction_extras,
    agent_nwc_redaction_extras_for_pubkey, agent_nwc_spawn_uri, agent_nwc_timeouts,
    agent_wallet_status_for, load_agent_nwc_uri, observe_agent_nwc_for_spawn,
    observe_agent_nwc_uri, provision_agent_nwc, unprovision_agent_nwc, validate_agent_receive_only,
    AdvertisementProbe, AgentNwcOpGates, AgentNwcSpawnObservation, AgentWalletStatus,
    AGENT_SPEND_METHODS,
};
pub use error::map_wallet_error;
pub use payment_store::JsonPaymentStore;
pub use profile::RelayProfilePublisher;
#[cfg(test)]
pub use runtime::NOT_LINKED;
pub use runtime::{
    AttemptKeyDto, CheckIncomingDto, IncomingCheckOutcome, PrepareSendQuote, ReceiveInvoice,
    SendConfirmOutcome, SendTargetDto, SettledPayRequestDto, WalletPorts, WalletRuntime,
    WalletStatusView,
};
#[cfg(test)]
pub use secret_store::nwc_blob_key;
pub use secret_store::{BlobBackend, KeyringNwcSecretStore, MapBlobBackend};
pub use secret_store::{BlobBackend as WalletBlobBackend, OsBlobBackend};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use buzz_wallet_pkg::{HttpLnurlResolver, NwcWalletConnector, SystemClock, WalletTimeouts};

/// Default RPC timeouts for the production NWC connector.
fn production_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(30),
        pay_response: Duration::from_secs(60),
    }
}

/// Reject path-traversal / empty community ids before touching the filesystem.
pub fn sanitize_community_id(community_id: &str) -> Result<String, String> {
    let trimmed = community_id.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.contains('\0')
    {
        return Err("invalid_community_id".to_string());
    }
    Ok(trimmed.to_string())
}

/// Durable payments file for one community under the app data dir.
pub fn payments_path(data_dir: &Path, community_id: &str) -> PathBuf {
    data_dir
        .join("wallet")
        .join(community_id)
        .join("payments.json")
}

/// Managed per-community wallet state for the Tauri backend.
pub struct WalletManager {
    inner: Mutex<WalletManagerInner>,
}

struct WalletManagerInner {
    data_dir: Option<PathBuf>,
    app: Option<AppHandle>,
    /// Active community session. Dropped (handles gone) on switch; durable
    /// store files survive on disk.
    session: Option<WalletRuntime>,
}

impl Default for WalletManager {
    fn default() -> Self {
        Self {
            inner: Mutex::new(WalletManagerInner {
                data_dir: None,
                app: None,
                session: None,
            }),
        }
    }
}

impl WalletManager {
    /// Bind the app data directory and handle (called once from setup).
    pub async fn init(&self, data_dir: PathBuf, app: AppHandle) {
        let mut guard = self.inner.lock().await;
        guard.data_dir = Some(data_dir);
        guard.app = Some(app);
    }

    /// Switch (or first-activate) the live session to `community_id`.
    ///
    /// Drops the previous community's in-flight confirm handles. Does **not**
    /// delete that community's durable payment records.
    pub async fn activate_community(&self, community_id: &str) -> Result<(), String> {
        let community_id = sanitize_community_id(community_id)?;
        let mut guard = self.inner.lock().await;
        let data_dir = guard
            .data_dir
            .clone()
            .ok_or_else(|| "wallet_not_initialized".to_string())?;
        let app = guard
            .app
            .clone()
            .ok_or_else(|| "wallet_not_initialized".to_string())?;

        if guard
            .session
            .as_ref()
            .is_some_and(|s| s.community_id() == community_id)
        {
            return Ok(());
        }

        let runtime = build_production_runtime(&community_id, &data_dir, &app)?;
        guard.session = Some(runtime);
        Ok(())
    }

    /// Run ensure-connected + reconcile for the active community.
    ///
    /// Safe to call when unlinked (no-op). Never returns secret material.
    /// Newly settled pay-request DTOs are dropped here — V3 consumes them via
    /// the explicit `wallet_reconcile` command.
    pub async fn reconcile_active(&self) -> Result<(), String> {
        let guard = self.inner.lock().await;
        let Some(session) = guard.session.as_ref() else {
            return Ok(());
        };
        session.ensure_connected().await?;
        let _settled = session.reconcile().await?;
        Ok(())
    }

    /// Activate `community_id` then reconcile — used after `apply_workspace`.
    pub async fn activate_and_reconcile(&self, community_id: &str) -> Result<(), String> {
        self.activate_community(community_id).await?;
        self.reconcile_active().await
    }

    /// Link a wallet for the active community.
    pub async fn link_wallet(&self, uri: String) -> Result<WalletStatusView, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.link(&uri).await
    }

    /// Unlink the active community's wallet and clear kind:0 `lud16`.
    pub async fn unlink_wallet(&self) -> Result<(), String> {
        let (app, unlink_result) = {
            let guard = self.inner.lock().await;
            let session = guard
                .session
                .as_ref()
                .ok_or_else(|| "wallet_no_community".to_string())?;
            let app = guard.app.clone();
            (app, session.unlink().await)
        };
        unlink_result?;
        // Best-effort profile clear — secret is already gone.
        if let Some(app) = app {
            let publisher = RelayProfilePublisher::new(app);
            if let Err(err) = publisher.clear_lud16().await {
                eprintln!(
                    "buzz-desktop: wallet unlink cleared secret; lud16 clear failed: {}",
                    map_wallet_error(err)
                );
            }
        }
        Ok(())
    }

    /// Status for the active community (never includes the NWC URI).
    pub async fn wallet_status(&self) -> Result<WalletStatusView, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.status().await
    }

    /// Mint a receive invoice.
    pub async fn wallet_receive(
        &self,
        amount_msat: u64,
        description: Option<String>,
    ) -> Result<ReceiveInvoice, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.receive(amount_msat, description.as_deref()).await
    }

    /// Prepare a send; returns a quote + opaque handle id.
    pub async fn wallet_prepare_send(
        &self,
        target: SendTargetDto,
        amount_msat: u64,
        attempt: AttemptKeyDto,
        memo: Option<String>,
    ) -> Result<PrepareSendQuote, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session
            .prepare_send(target, amount_msat, attempt, memo.as_deref())
            .await
    }

    /// Confirm a prepared send by opaque handle id.
    pub async fn wallet_confirm(&self, handle_id: String) -> Result<SendConfirmOutcome, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.confirm(&handle_id).await
    }

    /// Cancel a prepared send by opaque handle id.
    pub async fn wallet_cancel(&self, handle_id: String) -> Result<(), String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.cancel(&handle_id)
    }

    /// Explicit reconcile command — returns newly settled pay-request attempts.
    pub async fn wallet_reconcile(&self) -> Result<Vec<SettledPayRequestDto>, String> {
        let guard = self.inner.lock().await;
        let Some(session) = guard.session.as_ref() else {
            return Ok(Vec::new());
        };
        session.ensure_connected().await?;
        session.reconcile().await
    }

    /// Local settlement check for an incoming payment request.
    pub async fn wallet_check_incoming(
        &self,
        dto: CheckIncomingDto,
    ) -> Result<IncomingCheckOutcome, String> {
        let guard = self.inner.lock().await;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "wallet_no_community".to_string())?;
        session.check_incoming(dto).await
    }
}

fn build_production_runtime(
    community_id: &str,
    data_dir: &Path,
    app: &AppHandle,
) -> Result<WalletRuntime, String> {
    let timeouts = production_timeouts();
    let store_path = payments_path(data_dir, community_id);
    let ports = WalletPorts {
        connector: Arc::new(NwcWalletConnector::new(timeouts)),
        secrets: Arc::new(KeyringNwcSecretStore::new(
            community_id.to_string(),
            OsBlobBackend,
        )),
        profiles: Arc::new(RelayProfilePublisher::new(app.clone())),
        resolver: Arc::new(HttpLnurlResolver::new()),
        store: Arc::new(JsonPaymentStore::open(store_path)?),
        clock: Arc::new(SystemClock),
        timeouts,
    };
    Ok(WalletRuntime::new(community_id.to_string(), ports))
}

/// Spawn a non-blocking reconcile after workspace apply. Failures are logged
/// redacted and never fail the apply itself.
pub fn spawn_reconcile_after_apply(app: AppHandle, community_id: Option<String>) {
    let Some(community_id) = community_id.filter(|id| !id.trim().is_empty()) else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let Some(manager) = app.try_state::<WalletManager>() else {
            return;
        };
        if let Err(error) = manager.activate_and_reconcile(&community_id).await {
            // Enumerated / redacted — never the NWC URI.
            eprintln!("buzz-desktop: wallet reconcile after apply failed: {error}");
        }
    });
}

#[cfg(test)]
mod tests;

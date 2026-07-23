//! Wallet use-cases: link, receive_mode, receive.
//!
//! U4/U5 extend this struct with prepare_send / confirm / reconcile.

use crate::error::WalletError;
use crate::ports::{ProfilePublisher, SecretStore, WalletConnector, WalletService};
use crate::types::{
    Bolt11, Capabilities, Kind0Fields, ReceiveMode, StoredSecret, WalletHandle, WalletMethod,
};
use buzz_core::payment::Amount;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;

/// Injected-port facade for wallet use-cases.
///
/// Holds connector / secret / profile ports plus the live [`WalletService`]
/// after a successful [`link`](Self::link). `receive_mode` reads only from
/// the secret store — never the network.
pub struct Wallet {
    connector: Arc<dyn WalletConnector>,
    secrets: Arc<dyn SecretStore>,
    profiles: Arc<dyn ProfilePublisher>,
    connect_timeout: Duration,
    service: Mutex<Option<Arc<dyn WalletService>>>,
}

impl Wallet {
    /// Build a wallet facade with injectable ports and connect timeout.
    ///
    /// Tests pass a short timeout and use `tokio::time::pause` so the
    /// unreachable-relay scenario advances without sleeping wall time.
    pub fn new(
        connector: Arc<dyn WalletConnector>,
        secrets: Arc<dyn SecretStore>,
        profiles: Arc<dyn ProfilePublisher>,
        connect_timeout: Duration,
    ) -> Self {
        Self {
            connector,
            secrets,
            profiles,
            connect_timeout,
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
        let service = self
            .service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(WalletError::Unreachable)?;
        service.make_invoice(amount, memo).await
    }

    async fn connect_with_timeout(
        &self,
        uri: &str,
    ) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError> {
        match timeout(self.connect_timeout, self.connector.connect(uri)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(WalletError::Unreachable),
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

#[cfg(test)]
mod tests;

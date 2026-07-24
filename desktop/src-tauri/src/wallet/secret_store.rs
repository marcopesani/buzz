//! Keyring-backed [`SecretStore`](buzz_wallet_pkg::SecretStore) for NWC URIs.
//!
//! Blob key convention: `nwc:{community_id}` — community-scoped, never on an
//! env-read path. The URI is stored only in the OS keyring blob; it is never
//! written to managed files under the app data dir.

use async_trait::async_trait;
use buzz_wallet_pkg::{Capabilities, SecretStore, StoredSecret, WalletError, WalletMethod};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Keyring blob key for a community's NWC secret.
pub fn nwc_blob_key(community_id: &str) -> String {
    format!("nwc:{community_id}")
}

/// Minimal get/set/delete surface so tests (and agent-NWC helpers) can inject
/// a map without the OS keyring.
pub trait BlobBackend: Send + Sync {
    /// Load a blob value by key.
    fn load(&self, key: &str) -> Result<Option<String>, String>;
    /// Store a blob value by key.
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
    /// Delete a blob value by key (missing is ok).
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Production backend — the process-global OS keyring secret blob.
pub struct OsBlobBackend;

impl BlobBackend for OsBlobBackend {
    fn load(&self, key: &str) -> Result<Option<String>, String> {
        crate::secret_store::SecretStore::shared("buzz-desktop").load(key)
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        crate::secret_store::SecretStore::shared("buzz-desktop").store(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        crate::secret_store::SecretStore::shared("buzz-desktop").delete(key)
    }
}

/// In-memory blob backend for unit tests and the wallet E2E harness.
#[derive(Debug, Default)]
pub struct MapBlobBackend {
    map: Mutex<HashMap<String, String>>,
}

impl MapBlobBackend {
    /// Empty map backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of stored keys (test inspection).
    pub fn keys(&self) -> Vec<String> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Raw value for `key`, if any.
    pub fn get_raw(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }
}

impl BlobBackend for MapBlobBackend {
    fn load(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned())
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct SecretBlob {
    uri: String,
    capabilities: Vec<String>,
    #[serde(default)]
    lud16: Option<String>,
}

/// Community-scoped NWC [`SecretStore`] over a [`BlobBackend`].
pub struct KeyringNwcSecretStore<B: BlobBackend> {
    community_id: String,
    backend: B,
}

impl<B: BlobBackend> KeyringNwcSecretStore<B> {
    /// Build a store for `community_id` using `backend`.
    pub fn new(community_id: impl Into<String>, backend: B) -> Self {
        Self {
            community_id: community_id.into(),
            backend,
        }
    }

    fn key(&self) -> String {
        nwc_blob_key(&self.community_id)
    }
}

fn encode_secret(secret: &StoredSecret) -> Result<String, WalletError> {
    let blob = SecretBlob {
        uri: secret.uri.clone(),
        capabilities: secret
            .capabilities
            .iter()
            .map(WalletMethod::as_str)
            .map(str::to_string)
            .collect(),
        lud16: secret.lud16.clone(),
    };
    serde_json::to_string(&blob).map_err(|_| WalletError::SecretUnavailable)
}

fn decode_secret(raw: &str) -> Result<StoredSecret, WalletError> {
    let blob: SecretBlob = serde_json::from_str(raw).map_err(|_| WalletError::SecretUnavailable)?;
    Ok(StoredSecret {
        uri: blob.uri,
        capabilities: Capabilities::parse(blob.capabilities),
        lud16: blob.lud16,
    })
}

#[async_trait]
impl<B: BlobBackend + 'static> SecretStore for KeyringNwcSecretStore<B> {
    async fn store(&self, secret: &StoredSecret) -> Result<(), WalletError> {
        let encoded = encode_secret(secret)?;
        self.backend
            .store(&self.key(), &encoded)
            .map_err(|_| WalletError::SecretUnavailable)
    }

    async fn load(&self) -> Result<Option<StoredSecret>, WalletError> {
        match self.backend.load(&self.key()) {
            Ok(None) => Ok(None),
            Ok(Some(raw)) => Ok(Some(decode_secret(&raw)?)),
            Err(_) => Err(WalletError::SecretUnavailable),
        }
    }

    async fn clear(&self) -> Result<(), WalletError> {
        self.backend
            .delete(&self.key())
            .map_err(|_| WalletError::SecretUnavailable)
    }
}

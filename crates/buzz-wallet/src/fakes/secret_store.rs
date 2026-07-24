//! In-memory [`SecretStore`](crate::ports::SecretStore) with snapshot asserts.

use crate::error::WalletError;
use crate::ports::SecretStore;
use crate::types::StoredSecret;
use async_trait::async_trait;
use std::sync::Mutex;

/// Community-scoped in-memory secret store.
#[derive(Debug)]
pub struct InMemorySecretStore {
    community_id: String,
    secret: Mutex<Option<StoredSecret>>,
}

impl InMemorySecretStore {
    /// Create a store for `community_id`.
    pub fn new(community_id: impl Into<String>) -> Self {
        Self {
            community_id: community_id.into(),
            secret: Mutex::new(None),
        }
    }

    /// Community this store is scoped to.
    pub fn community_id(&self) -> &str {
        &self.community_id
    }

    /// Snapshot the current secret (for "unchanged since" assertions).
    pub fn snapshot(&self) -> Option<StoredSecret> {
        self.secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Panic if the store differs from `before`.
    ///
    /// Link-failure scenarios use this to prove secure storage was untouched.
    pub fn assert_unchanged_since(&self, before: &Option<StoredSecret>) {
        let now = self.snapshot();
        assert_eq!(
            &now, before,
            "secure storage changed for community {}",
            self.community_id
        );
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn store(&self, secret: &StoredSecret) -> Result<(), WalletError> {
        *self.secret.lock().unwrap_or_else(|e| e.into_inner()) = Some(secret.clone());
        Ok(())
    }

    async fn load(&self) -> Result<Option<StoredSecret>, WalletError> {
        Ok(self
            .secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    async fn clear(&self) -> Result<(), WalletError> {
        *self.secret.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

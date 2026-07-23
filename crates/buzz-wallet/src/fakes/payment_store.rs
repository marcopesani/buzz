//! In-memory [`PaymentStore`](crate::ports::PaymentStore) with atomic claim.

use crate::error::WalletError;
use crate::ports::PaymentStore;
use crate::types::{AttemptId, Bolt11, ClaimOutcome, PaymentRecord, PersistedPaymentState};
use async_trait::async_trait;
use buzz_core::payment::Amount;
use std::collections::HashMap;
use std::sync::Mutex;

/// Community-scoped in-memory payment store.
///
/// `claim_paying` uses a mutex so two concurrent claims of the same attempt
/// admit exactly one [`ClaimOutcome::Claimed`].
#[derive(Debug)]
pub struct InMemoryPaymentStore {
    community_id: String,
    records: Mutex<HashMap<String, PaymentRecord>>,
}

impl InMemoryPaymentStore {
    /// Create a store for `community_id`.
    pub fn new(community_id: impl Into<String>) -> Self {
        Self {
            community_id: community_id.into(),
            records: Mutex::new(HashMap::new()),
        }
    }

    /// Community this store is scoped to.
    pub fn community_id(&self) -> &str {
        &self.community_id
    }

    /// All records (test inspection).
    pub fn all(&self) -> Vec<PaymentRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

#[async_trait]
impl PaymentStore for InMemoryPaymentStore {
    async fn claim_paying(
        &self,
        attempt_id: &AttemptId,
        payment_hash: &str,
        bolt11: &Bolt11,
        amount: Amount,
        expires_at_unix: u64,
    ) -> Result<ClaimOutcome, WalletError> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let key = attempt_id.as_str().to_string();
        if let Some(existing) = records.get(&key) {
            return Ok(ClaimOutcome::AlreadyClaimed(existing.clone()));
        }
        let record = PaymentRecord {
            attempt_id: attempt_id.clone(),
            payment_hash: payment_hash.to_string(),
            bolt11: bolt11.clone(),
            amount,
            expires_at_unix,
            state: PersistedPaymentState::Paying,
        };
        records.insert(key, record.clone());
        Ok(ClaimOutcome::Claimed(record))
    }

    async fn get(&self, attempt_id: &AttemptId) -> Result<Option<PaymentRecord>, WalletError> {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        Ok(records.get(attempt_id.as_str()).cloned())
    }

    async fn update_state(
        &self,
        attempt_id: &AttemptId,
        state: PersistedPaymentState,
    ) -> Result<(), WalletError> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = records.get_mut(attempt_id.as_str()) else {
            return Err(WalletError::Unknown);
        };
        record.state = state;
        Ok(())
    }

    async fn list_by_states(
        &self,
        states: &[PersistedPaymentState],
    ) -> Result<Vec<PaymentRecord>, WalletError> {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        Ok(records
            .values()
            .filter(|r| states.contains(&r.state))
            .cloned()
            .collect())
    }
}

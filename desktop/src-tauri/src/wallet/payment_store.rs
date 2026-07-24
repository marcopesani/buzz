//! JSON-file [`PaymentStore`](buzz_wallet_pkg::PaymentStore) under the app data dir.
//!
//! Chosen over sqlite: the desktop app already uses atomic JSON writes for
//! managed-agent state, and a single per-community file is enough for the
//! payment-attempt latch. Atomicity of `claim_paying` is a process-local
//! mutex around load-modify-save (Buzz desktop is single-process for a given
//! identity); `expires_at_unix` is persisted so reconcile survives restart.

use async_trait::async_trait;
use buzz_core_pkg::payment::Amount;
use buzz_wallet_pkg::{
    AttemptId, Bolt11, ClaimOutcome, PaymentRecord, PaymentStore, PersistedPaymentState,
    WalletError,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredFile {
    records: Vec<StoredRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    attempt_id: String,
    payment_hash: String,
    bolt11: String,
    amount_msat: u64,
    expires_at_unix: u64,
    state: String,
    #[serde(default)]
    preimage: Option<String>,
}

impl StoredRecord {
    fn from_record(record: &PaymentRecord) -> Self {
        Self {
            attempt_id: record.attempt_id.as_str().to_string(),
            payment_hash: record.payment_hash.clone(),
            bolt11: record.bolt11.as_str().to_string(),
            amount_msat: record.amount.as_msat(),
            expires_at_unix: record.expires_at_unix,
            state: state_to_str(record.state).to_string(),
            preimage: record.preimage.clone(),
        }
    }

    fn into_record(self) -> Result<PaymentRecord, WalletError> {
        Ok(PaymentRecord {
            attempt_id: AttemptId::new(self.attempt_id),
            payment_hash: self.payment_hash,
            bolt11: Bolt11::new(self.bolt11),
            amount: Amount::from_msat(self.amount_msat),
            expires_at_unix: self.expires_at_unix,
            state: state_from_str(&self.state)?,
            preimage: self.preimage,
        })
    }
}

fn state_to_str(state: PersistedPaymentState) -> &'static str {
    match state {
        PersistedPaymentState::Paying => "paying",
        PersistedPaymentState::Settled => "settled",
        PersistedPaymentState::Failed => "failed",
        PersistedPaymentState::Unknown => "unknown",
    }
}

fn state_from_str(s: &str) -> Result<PersistedPaymentState, WalletError> {
    match s {
        "paying" => Ok(PersistedPaymentState::Paying),
        "settled" => Ok(PersistedPaymentState::Settled),
        "failed" => Ok(PersistedPaymentState::Failed),
        "unknown" => Ok(PersistedPaymentState::Unknown),
        _ => Err(WalletError::Unknown),
    }
}

/// Durable JSON payment store for one community.
pub struct JsonPaymentStore {
    path: PathBuf,
    /// Single-process latch: all mutations hold this mutex for load-modify-save.
    lock: Mutex<()>,
}

impl JsonPaymentStore {
    /// Open (or create) a store at `path`. Parent directories are created on first write.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        Ok(Self {
            path: path.into(),
            lock: Mutex::new(()),
        })
    }

    fn load_map(&self) -> Result<HashMap<String, PaymentRecord>, WalletError> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let bytes = std::fs::read(&self.path).map_err(|_| WalletError::Unknown)?;
        if bytes.is_empty() {
            return Ok(HashMap::new());
        }
        let file: StoredFile = serde_json::from_slice(&bytes).map_err(|_| WalletError::Unknown)?;
        let mut map = HashMap::new();
        for stored in file.records {
            let record = stored.into_record()?;
            map.insert(record.attempt_id.as_str().to_string(), record);
        }
        Ok(map)
    }

    fn save_map(&self, map: &HashMap<String, PaymentRecord>) -> Result<(), WalletError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| WalletError::Unknown)?;
        }
        let file = StoredFile {
            records: map.values().map(StoredRecord::from_record).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|_| WalletError::Unknown)?;
        // Atomic write via tmp+rename (same pattern as managed_agents storage).
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|_| WalletError::Unknown)?;
        std::fs::rename(&tmp, &self.path).map_err(|_| WalletError::Unknown)?;
        Ok(())
    }
}

#[async_trait]
impl PaymentStore for JsonPaymentStore {
    async fn claim_paying(
        &self,
        attempt_id: &AttemptId,
        payment_hash: &str,
        bolt11: &Bolt11,
        amount: Amount,
        expires_at_unix: u64,
    ) -> Result<ClaimOutcome, WalletError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load_map()?;
        let key = attempt_id.as_str().to_string();
        if let Some(existing) = map.get(&key) {
            return Ok(ClaimOutcome::AlreadyClaimed(existing.clone()));
        }
        let record = PaymentRecord {
            attempt_id: attempt_id.clone(),
            payment_hash: payment_hash.to_string(),
            bolt11: bolt11.clone(),
            amount,
            expires_at_unix,
            state: PersistedPaymentState::Paying,
            preimage: None,
        };
        map.insert(key, record.clone());
        self.save_map(&map)?;
        Ok(ClaimOutcome::Claimed(record))
    }

    async fn get(&self, attempt_id: &AttemptId) -> Result<Option<PaymentRecord>, WalletError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.load_map()?;
        Ok(map.get(attempt_id.as_str()).cloned())
    }

    async fn update_state(
        &self,
        attempt_id: &AttemptId,
        state: PersistedPaymentState,
        preimage: Option<String>,
    ) -> Result<(), WalletError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load_map()?;
        let Some(record) = map.get_mut(attempt_id.as_str()) else {
            return Err(WalletError::Unknown);
        };
        record.state = state;
        if let Some(preimage) = preimage {
            record.preimage = Some(preimage);
        }
        self.save_map(&map)?;
        Ok(())
    }

    async fn list_by_states(
        &self,
        states: &[PersistedPaymentState],
    ) -> Result<Vec<PaymentRecord>, WalletError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.load_map()?;
        Ok(map
            .values()
            .filter(|r| states.contains(&r.state))
            .cloned()
            .collect())
    }
}

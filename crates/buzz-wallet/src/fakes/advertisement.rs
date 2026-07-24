//! Scriptable NWC advertisement probe for agent receive-only tests.

use crate::error::WalletError;
use crate::types::WalletAdvertisement;
use std::sync::Mutex;

/// Fake probe — queues [`WalletAdvertisement`] (or errors) per call.
#[derive(Debug, Default)]
pub struct FakeAdvertisementProbe {
    scripts: Mutex<Vec<Result<WalletAdvertisement, WalletError>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeAdvertisementProbe {
    /// Empty probe — each `probe` must be scripted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful advertisement pair.
    pub fn script_ok(&self, ads: WalletAdvertisement) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Ok(ads));
    }

    /// Queue a probe failure.
    pub fn script_err(&self, err: WalletError) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Err(err));
    }

    /// URIs passed to `probe`, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Pop the next scripted outcome (records `uri` in [`Self::calls`]).
    pub async fn respond(&self, uri: &str) -> Result<WalletAdvertisement, WalletError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(uri.to_string());
        let mut scripts = self.scripts.lock().unwrap_or_else(|e| e.into_inner());
        if scripts.is_empty() {
            panic!("FakeAdvertisementProbe: respond called with no script queued");
        }
        scripts.remove(0)
    }
}

//! Scriptable [`LnurlResolver`](crate::ports::LnurlResolver).

use crate::error::WalletError;
use crate::ports::LnurlResolver;
use crate::types::ResolvedPay;
use async_trait::async_trait;
use buzz_core::payment::Amount;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Scripted outcome for one `resolve` call against a lud16.
#[derive(Debug, Clone)]
pub enum LnurlScript {
    /// Return this resolved pay.
    Ok(ResolvedPay),
    /// Fail with [`WalletError::ResolveRejected`] or [`WalletError::ResolveFailed`].
    Fail(WalletError),
}

/// Fake LNURL resolver — keyed by lud16, FIFO per address.
#[derive(Debug, Default)]
pub struct FakeLnurlResolver {
    scripts: Mutex<HashMap<String, VecDeque<LnurlScript>>>,
    calls: Mutex<Vec<(String, Amount, Option<String>)>>,
}

impl FakeLnurlResolver {
    /// Empty resolver — each resolve must be scripted for that lud16.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an outcome for `lud16` (FIFO).
    pub fn script(&self, lud16: impl Into<String>, script: LnurlScript) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(lud16.into())
            .or_default()
            .push_back(script);
    }

    /// Recorded `(lud16, amount, memo)` triples, in order.
    pub fn calls(&self) -> Vec<(String, Amount, Option<String>)> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl LnurlResolver for FakeLnurlResolver {
    async fn resolve(
        &self,
        lud16: &str,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<ResolvedPay, WalletError> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push((
            lud16.to_string(),
            amount,
            memo.map(str::to_string),
        ));

        let script = {
            let mut scripts = self.scripts.lock().unwrap_or_else(|e| e.into_inner());
            scripts
                .get_mut(lud16)
                .and_then(|q| q.pop_front())
                .unwrap_or_else(|| {
                    panic!("FakeLnurlResolver: resolve({lud16}) called with no script queued")
                })
        };

        match script {
            LnurlScript::Ok(pay) => Ok(pay),
            LnurlScript::Fail(err) => Err(err),
        }
    }
}

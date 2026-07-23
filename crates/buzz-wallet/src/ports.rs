//! Wallet ports — injectable boundaries for use-cases and drivers.

use crate::error::WalletError;
use crate::types::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, InvoiceStatus, Kind0Fields, PaymentRecord,
    PersistedPaymentState, ResolvedPay, StoredSecret, Tx,
};
use async_trait::async_trait;
use buzz_core::payment::Amount;
use std::sync::Arc;

/// Wall-clock source for expiry and timeout decisions.
///
/// Production uses the system clock; tests inject [`crate::fakes::FakeClock`]
/// and advance it — nothing sleeps for expiry scenarios.
pub trait Clock: Send + Sync {
    /// Current unix time in seconds.
    fn now_unix(&self) -> u64;
}

/// Links a wallet from an NWC URI **before** a [`WalletService`] exists.
///
/// Parses the URI, connects to the wallet relay, and reads
/// `13194 ∩ get_info.methods`. Link acceptance scenarios drive this port —
/// they cannot be covered by [`WalletService`] fakes alone.
#[async_trait]
pub trait WalletConnector: Send + Sync {
    /// Connect and return a live service plus link-time metadata.
    ///
    /// `lud16` comes from the connection-string query param when present
    /// (NIP-47); `get_info` / 13194 do not carry an address.
    async fn connect(
        &self,
        uri: &str,
    ) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError>;
}

/// Wallet transport: balance, invoices, pay, lookup, history.
///
/// Implemented by `NwcWalletService` (later) and [`crate::fakes::FakeWalletService`].
/// Capabilities gate optional methods (e.g. `get_balance`); balance is display
/// only — never a gate on send or receive.
#[async_trait]
pub trait WalletService: Send + Sync {
    /// Current balance in msat when the wallet exposes it.
    async fn get_balance(&self) -> Result<Option<Amount>, WalletError>;

    /// Mint a bolt11 invoice (receive).
    async fn make_invoice(&self, amount: Amount, memo: Option<&str>)
        -> Result<Bolt11, WalletError>;

    /// Pay a bolt11 once; returns the hex preimage on success.
    ///
    /// Callers must latch the attempt via [`PaymentStore::claim_paying`] first.
    /// A timeout is [`WalletError::Unknown`] — never re-fire for the same hash.
    async fn pay_invoice(&self, bolt11: &Bolt11) -> Result<String, WalletError>;

    /// Look up an invoice by payment hash.
    ///
    /// Lookup outcomes are [`InvoiceStatus`] variants. Transport failures
    /// still return [`WalletError`].
    async fn lookup_invoice(&self, payment_hash: &str) -> Result<InvoiceStatus, WalletError>;

    /// List recent wallet transactions.
    async fn list_transactions(&self) -> Result<Vec<Tx>, WalletError>;
}

/// Resolves a Lightning Address (LUD-16) to a payable bolt11.
///
/// Lives **beside** the wallet, not inside it. The pay path has one input:
/// bolt11. HTTPS / min / max bounds are enforced inside the real adapter.
#[async_trait]
pub trait LnurlResolver: Send + Sync {
    /// Resolve `lud16` for `amount` (msat) into a [`ResolvedPay`].
    async fn resolve(
        &self,
        lud16: &str,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<ResolvedPay, WalletError>;
}

/// Durable per-community payment attempt store.
///
/// The atomic [`claim_paying`](Self::claim_paying) latch is keyed by
/// [`AttemptId`], not payment hash alone.
#[async_trait]
pub trait PaymentStore: Send + Sync {
    /// Atomically persist `payment_hash` + [`PersistedPaymentState::Paying`]
    /// and latch the attempt.
    ///
    /// A second claim for the same attempt returns
    /// [`ClaimOutcome::AlreadyClaimed`] — not a second record.
    async fn claim_paying(
        &self,
        attempt_id: &AttemptId,
        payment_hash: &str,
        bolt11: &Bolt11,
        amount: Amount,
    ) -> Result<ClaimOutcome, WalletError>;

    /// Load a record by attempt id.
    async fn get(&self, attempt_id: &AttemptId) -> Result<Option<PaymentRecord>, WalletError>;

    /// Update the persisted state of an existing attempt.
    async fn update_state(
        &self,
        attempt_id: &AttemptId,
        state: PersistedPaymentState,
    ) -> Result<(), WalletError>;

    /// List records whose state is in `states` (for reconcile).
    async fn list_by_states(
        &self,
        states: &[PersistedPaymentState],
    ) -> Result<Vec<PaymentRecord>, WalletError>;
}

/// Secure storage for the NWC URI and capabilities (per community).
///
/// Drivers supply the impl: OS keyring (desktop), `0600` file (CLI).
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Persist URI + capabilities together.
    async fn store(&self, secret: &StoredSecret) -> Result<(), WalletError>;

    /// Load the stored secret, if any.
    async fn load(&self) -> Result<Option<StoredSecret>, WalletError>;

    /// Clear the stored secret.
    async fn clear(&self) -> Result<(), WalletError>;
}

/// Merge-publishes kind:0 fields (drivers own the Buzz relay transport).
#[async_trait]
pub trait ProfilePublisher: Send + Sync {
    /// Merge `fields` into the latest kind:0, preserving every other field.
    async fn merge_publish(&self, fields: Kind0Fields) -> Result<(), WalletError>;
}

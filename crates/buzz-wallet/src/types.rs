//! Shared wallet types — capabilities, invoice status, store records.

use buzz_core::payment::Amount;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

/// Opaque bolt11 invoice string.
///
/// Decoding (amount, payment_hash, expiry) is a later unit — this newtype
/// keeps the domain from treating the string as structured data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Bolt11(String);

impl Bolt11 {
    /// Wrap an opaque bolt11 string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the opaque string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for Bolt11 {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Bolt11 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Bolt11 {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Bolt11 {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Identity of one payment attempt (confirm-handle / request identity).
///
/// Distinct from [`payment_hash`](PaymentRecord::payment_hash): a double-tap
/// on a `lud16` card mints two invoices with two hashes, but two attempt ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttemptId(String);

impl AttemptId {
    /// Construct an attempt id.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AttemptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AttemptId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AttemptId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Known NWC method names (closed set).
///
/// Unknown method strings are ignored at parse — capabilities are queried,
/// never assumed from free-form wallet ads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WalletMethod {
    /// `pay_invoice`
    PayInvoice,
    /// `make_invoice`
    MakeInvoice,
    /// `lookup_invoice`
    LookupInvoice,
    /// `get_balance`
    GetBalance,
    /// `list_transactions`
    ListTransactions,
    /// `pay_keysend`
    PayKeysend,
    /// `multi_pay_invoice`
    MultiPayInvoice,
    /// `multi_pay_keysend`
    MultiPayKeysend,
    /// `notifications`
    Notifications,
}

impl WalletMethod {
    /// Canonical NWC method string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PayInvoice => "pay_invoice",
            Self::MakeInvoice => "make_invoice",
            Self::LookupInvoice => "lookup_invoice",
            Self::GetBalance => "get_balance",
            Self::ListTransactions => "list_transactions",
            Self::PayKeysend => "pay_keysend",
            Self::MultiPayInvoice => "multi_pay_invoice",
            Self::MultiPayKeysend => "multi_pay_keysend",
            Self::Notifications => "notifications",
        }
    }
}

impl FromStr for WalletMethod {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pay_invoice" => Ok(Self::PayInvoice),
            "make_invoice" => Ok(Self::MakeInvoice),
            "lookup_invoice" => Ok(Self::LookupInvoice),
            "get_balance" => Ok(Self::GetBalance),
            "list_transactions" => Ok(Self::ListTransactions),
            "pay_keysend" => Ok(Self::PayKeysend),
            "multi_pay_invoice" => Ok(Self::MultiPayInvoice),
            "multi_pay_keysend" => Ok(Self::MultiPayKeysend),
            "notifications" => Ok(Self::Notifications),
            _ => Err(()),
        }
    }
}

/// Set of NWC methods the linked wallet supports.
///
/// Built as `13194 ∩ get_info.methods` at link time and persisted beside the
/// secret so a restart never needs the network to know Receive availability.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    methods: BTreeSet<WalletMethod>,
}

impl Capabilities {
    /// Empty capability set.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from known methods (unknown names already filtered).
    pub fn from_methods(methods: impl IntoIterator<Item = WalletMethod>) -> Self {
        Self {
            methods: methods.into_iter().collect(),
        }
    }

    /// Parse method-name strings, ignoring unknowns.
    pub fn parse(names: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self::from_methods(names.into_iter().filter_map(|n| n.as_ref().parse().ok()))
    }

    /// Intersection of two capability sets (`self ∩ other`).
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            methods: self.methods.intersection(&other.methods).copied().collect(),
        }
    }

    /// Link-time advertisement: `13194 ∩ get_info.methods`.
    ///
    /// Unknown strings in either list are ignored before the intersect.
    pub fn from_link_advertisement(
        info_13194: impl IntoIterator<Item = impl AsRef<str>>,
        get_info_methods: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        Self::parse(info_13194).intersect(&Self::parse(get_info_methods))
    }

    /// Whether the given method is present.
    pub fn contains(&self, method: WalletMethod) -> bool {
        self.methods.contains(&method)
    }

    /// Iterate methods in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = WalletMethod> + '_ {
        self.methods.iter().copied()
    }

    /// Number of methods.
    pub fn len(&self) -> usize {
        self.methods.len()
    }

    /// True when no methods are advertised.
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }
}

/// Outcome of [`lookup_invoice`](crate::ports::WalletService::lookup_invoice).
///
/// Lookup outcomes are **not** [`WalletError`](crate::WalletError) variants —
/// the lifecycle maps each status explicitly. Transport failures still use
/// `WalletError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceStatus {
    /// Invoice paid; preimage known to the wallet.
    Settled,
    /// Payment in flight or invoice unpaid but live.
    Pending,
    /// Payment definitively failed.
    Failed,
    /// Invoice expired unpaid.
    Expired,
    /// Wallet has no record (may be temporary right after a crash).
    NotFound,
}

/// One wallet transaction row from `list_transactions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    /// Hex-encoded payment hash.
    pub payment_hash: String,
    /// Amount in millisatoshis.
    pub amount: Amount,
    /// Optional memo / description.
    pub memo: Option<String>,
    /// Unix-seconds settlement time, when known.
    pub settled_at: Option<u64>,
}

/// Result of resolving a Lightning Address to a payable bolt11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPay {
    /// Opaque bolt11 returned by the LNURL callback.
    pub bolt11: Bolt11,
    /// Hex-encoded payment hash (as reported by the resolver; re-checked later).
    pub payment_hash: String,
    /// Amount requested / embedded (msat).
    pub amount: Amount,
    /// LNURL `minSendable` (msat).
    pub min_sendable: Amount,
    /// LNURL `maxSendable` (msat).
    pub max_sendable: Amount,
}

/// Persisted payment states in [`PaymentStore`](crate::ports::PaymentStore).
///
/// Pre-claim states (`Resolving`, `ReadyToConfirm`) live in memory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersistedPaymentState {
    /// Human confirmed; `pay_invoice` may be in flight or about to fire.
    Paying,
    /// Preimage obtained (or lookup settled).
    Settled,
    /// Definitive failure.
    Failed,
    /// Timeout / disconnect — exit only via `lookup_invoice`.
    Unknown,
}

/// Durable payment attempt record (per community).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRecord {
    /// Attempt identity (confirm-handle / request) — latch key.
    pub attempt_id: AttemptId,
    /// Hex-encoded payment hash of the bound bolt11.
    pub payment_hash: String,
    /// Opaque bolt11 that was (or will be) paid.
    pub bolt11: Bolt11,
    /// Amount in millisatoshis.
    pub amount: Amount,
    /// Persisted lifecycle state.
    pub state: PersistedPaymentState,
}

/// Result of an atomic [`claim_paying`](crate::ports::PaymentStore::claim_paying).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// First claim — record persisted in [`PersistedPaymentState::Paying`].
    Claimed(PaymentRecord),
    /// Attempt already latched — second tap is a no-op at the store layer.
    AlreadyClaimed(PaymentRecord),
}

/// NWC URI + capabilities persisted together in secure storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSecret {
    /// Raw `nostr+walletconnect://…` URI (never logged).
    pub uri: String,
    /// Capabilities captured at link (or last refresh).
    pub capabilities: Capabilities,
}

/// Kind:0 fields a [`ProfilePublisher`](crate::ports::ProfilePublisher) may merge.
///
/// Drivers own the full profile merge (preserve every other field). This type
/// only names the payment-relevant fields the wallet use-cases set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Kind0Fields {
    /// Lightning Address to publish (or clear when `None` is intentional — drivers decide).
    pub lud16: Option<String>,
}

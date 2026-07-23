#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-wallet` — Lightning wallet ports, fakes, and (later) use-cases.
//!
//! Dependencies point inward: entities live in `buzz-core`; this crate owns
//! the injectable ports (`WalletService`, `LnurlResolver`, `WalletConnector`,
//! `PaymentStore`, `SecretStore`, `ProfilePublisher`) and scriptable fakes
//! so acceptance scenarios run network-free.
//!
//! Unit U2 ships ports + error enum + fakes only — no use-cases, no real
//! adapters, no bolt11 decoding.

/// Shared [`WalletError`] for every port.
pub mod error;
/// Scriptable fakes for dependent tests (not `cfg(test)`-gated).
pub mod fakes;
/// Async port traits.
pub mod ports;
/// Capabilities, invoice status, store records, opaque bolt11.
pub mod types;

pub use error::WalletError;
pub use ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
pub use types::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, InvoiceStatus, Kind0Fields, PaymentRecord,
    PersistedPaymentState, ResolvedPay, StoredSecret, Tx, WalletMethod,
};

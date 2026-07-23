#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-wallet` — Lightning wallet ports, use-cases, and fakes.
//!
//! Dependencies point inward: entities live in `buzz-core`; this crate owns
//! the injectable ports (`WalletService`, `LnurlResolver`, `WalletConnector`,
//! `PaymentStore`, `SecretStore`, `ProfilePublisher`), scriptable fakes, and
//! use-cases (`link`, `receive_mode`, `receive`) so acceptance scenarios run
//! network-free.

/// Shared [`WalletError`] for every port.
pub mod error;
/// Scriptable fakes for dependent tests (not `cfg(test)`-gated).
pub mod fakes;
/// Async port traits.
pub mod ports;
/// Capabilities, invoice status, store records, opaque bolt11.
pub mod types;
/// Use-cases: link, receive_mode, receive (U4/U5 extend send/reconcile).
pub mod wallet;

pub use error::WalletError;
pub use ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
pub use types::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, InvoiceStatus, Kind0Fields, PaymentRecord,
    PersistedPaymentState, ReceiveMode, ResolvedPay, StoredSecret, Tx, WalletHandle, WalletMethod,
};
pub use wallet::Wallet;

#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-wallet` — Lightning wallet ports, use-cases, and fakes.
//!
//! Dependencies point inward: entities live in `buzz-core`; this crate owns
//! the injectable ports (`WalletService`, `LnurlResolver`, `WalletConnector`,
//! `PaymentStore`, `SecretStore`, `ProfilePublisher`), scriptable fakes,
//! bolt11 validation, and use-cases (`link`, `receive_mode`, `receive`,
//! `prepare_send`, `confirm`, `cancel`) so acceptance scenarios run
//! network-free.

mod bolt11;
/// Shared [`WalletError`] for every port.
pub mod error;
/// Scriptable fakes for dependent tests (not `cfg(test)`-gated).
pub mod fakes;
/// Async port traits.
pub mod ports;
/// Production system clock.
pub mod system_clock;
/// Harness invoice minting (tests + future mock wallet).
pub mod test_support;
/// Capabilities, invoice status, store records, opaque bolt11.
pub mod types;
/// Use-cases: link, receive_mode, receive, prepare_send, confirm, cancel.
pub mod wallet;

pub use error::WalletError;
pub use ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
pub use system_clock::SystemClock;
pub use types::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, ConfirmHandle, InvoiceStatus, Kind0Fields,
    PaymentRecord, PersistedPaymentState, ReceiveMode, ResolvedPay, SendOutcome, SendTarget,
    StoredSecret, Tx, WalletHandle, WalletMethod,
};
pub use wallet::Wallet;

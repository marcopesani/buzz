#![deny(unsafe_code)]
#![warn(missing_docs)]
//! `buzz-wallet` — Lightning wallet ports, use-cases, and fakes.
//!
//! Dependencies point inward: entities live in `buzz-core`; this crate owns
//! the injectable ports (`WalletService`, `LnurlResolver`, `WalletConnector`,
//! `PaymentStore`, `SecretStore`, `ProfilePublisher`), scriptable fakes,
//! bolt11 validation, and use-cases (`link`, `receive`, `prepare_send`,
//! `confirm`, `cancel`, `reconcile`, `check_incoming`) so acceptance
//! scenarios run network-free.

/// Production NWC / LNURL adapters.
pub mod adapters;
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
/// Use-cases: link, receive, send, reconcile, check_incoming.
pub mod wallet;

pub use adapters::{
    map_nip47_error_code, parse_nwc_uri, HttpLnurlResolver, NwcWalletConnector, NwcWalletService,
    ParsedNwcUri,
};
pub use error::WalletError;
pub use ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
pub use system_clock::SystemClock;
pub use types::{
    AttemptId, AttemptKey, Bolt11, Capabilities, ClaimOutcome, ConfirmHandle, IncomingStatus,
    InvoiceStatus, Kind0Fields, PaymentRecord, PersistedPaymentState, ReceiveMode, ResolvedPay,
    SendOutcome, SendTarget, StoredSecret, Tx, WalletAdvertisement, WalletHandle, WalletMethod,
    WalletTimeouts,
};
pub use wallet::Wallet;

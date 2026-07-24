//! Scriptable fakes for every wallet port.
//!
//! Available to dependent crates (not gated on `cfg(test)`) so U3–U5
//! integration tests and `buzz-cli` tests can drive the same harness.

mod advertisement;
mod clock;
mod connector;
mod lnurl;
mod payment_store;
mod profile;
mod secret_store;
mod wallet;

pub use advertisement::FakeAdvertisementProbe;
pub use clock::FakeClock;
pub use connector::{ConnectorScript, FakeWalletConnector};
pub use lnurl::{FakeLnurlResolver, LnurlScript};
pub use payment_store::InMemoryPaymentStore;
pub use profile::{RecordingProfilePublisher, SeededKind0};
pub use secret_store::InMemorySecretStore;
pub use wallet::{FakeWalletService, InvoiceScript, MakeInvoiceScript, PayScript, WalletCall};

#[cfg(test)]
mod tests;

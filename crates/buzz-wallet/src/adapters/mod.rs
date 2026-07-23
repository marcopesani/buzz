//! Production adapters behind the wallet ports.
//!
//! Translators only: NWC / LNURL vocabulary → [`WalletError`](crate::WalletError)
//! and port types. Lifecycle decisions stay in [`crate::Wallet`].

pub mod lnurl;
pub mod nwc;

pub use lnurl::HttpLnurlResolver;
pub use nwc::{
    map_nip47_error_code, parse_nwc_uri, NwcWalletConnector, NwcWalletService, ParsedNwcUri,
};

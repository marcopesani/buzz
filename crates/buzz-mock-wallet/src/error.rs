//! Errors for the mock wallet daemon and relay.

use thiserror::Error;

/// Failures starting or operating the mock wallet.
#[derive(Debug, Error)]
pub enum MockWalletError {
    /// TCP bind or accept failed.
    #[error("network error: {0}")]
    Network(String),
    /// Nostr event build, sign, encrypt, or decrypt failed.
    #[error("nostr error: {0}")]
    Nostr(String),
    /// Bolt11 mint or decode failed.
    #[error("invoice error: {0}")]
    Invoice(String),
    /// Invalid configuration.
    #[error("config error: {0}")]
    Config(String),
    /// JSON parse/serialize failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

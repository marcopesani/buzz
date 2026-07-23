//! Map [`buzz_wallet_pkg::WalletError`] to stable, URI-free frontend strings.

use buzz_wallet_pkg::WalletError;

/// Closed set of error codes surfaced to the webview.
///
/// Never formats `Debug` of internal errors — those can embed the NWC URI.
pub fn map_wallet_error(err: WalletError) -> String {
    match err {
        WalletError::InvalidUri => "invalid_uri".to_string(),
        WalletError::Unreachable => "unreachable".to_string(),
        WalletError::Unauthorized => "unauthorized".to_string(),
        WalletError::Unsupported => "unsupported".to_string(),
        WalletError::InsufficientBalance => "insufficient_balance".to_string(),
        WalletError::QuotaExceeded => "quota_exceeded".to_string(),
        WalletError::PaymentFailed => "payment_failed".to_string(),
        WalletError::ResolveRejected => "resolve_rejected".to_string(),
        WalletError::ResolveFailed => "resolve_failed".to_string(),
        WalletError::SecretUnavailable => "secret_unavailable".to_string(),
        WalletError::Unknown => "unknown".to_string(),
    }
}

//! Single error enum for every wallet port.

use thiserror::Error;

/// Failure modes shared by all wallet ports.
///
/// Adapters map transport vocabulary (NWC codes, HTTP statuses) into these
/// variants at the boundary. Use-cases and UI never see NWC or HTTP terms.
///
/// Two classes matter to the payment state machine: **definitive** errors
/// ([`PaymentFailed`](Self::PaymentFailed), [`InsufficientBalance`](Self::InsufficientBalance),
/// [`QuotaExceeded`](Self::QuotaExceeded)) land in `Failed` and permit retry;
/// [`Unknown`](Self::Unknown) permits only reconciliation via `lookup_invoice`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WalletError {
    /// Malformed NWC connection string — nothing stored.
    #[error("invalid NWC URI")]
    InvalidUri,
    /// Wallet relay connect / info-event failure.
    #[error("wallet unreachable")]
    Unreachable,
    /// NWC UNAUTHORIZED or RESTRICTED — secret revoked; prompt relink.
    #[error("wallet unauthorized")]
    Unauthorized,
    /// NWC NOT_IMPLEMENTED — capability absent; feature hidden.
    #[error("wallet method unsupported")]
    Unsupported,
    /// NWC INSUFFICIENT_BALANCE — definitive.
    #[error("insufficient balance")]
    InsufficientBalance,
    /// NWC QUOTA_EXCEEDED or RATE_LIMITED — connection budget spent.
    #[error("wallet quota exceeded")]
    QuotaExceeded,
    /// NWC PAYMENT_FAILED — definitive; retry allowed (fresh invoice).
    #[error("payment failed")]
    PaymentFailed,
    /// LNURL response failed validation — never pay.
    #[error("LNURL resolve rejected")]
    ResolveRejected,
    /// LNURL transport error (DNS / TLS / HTTP / malformed JSON).
    #[error("LNURL resolve failed")]
    ResolveFailed,
    /// Secure storage locked or missing — no plaintext fallback.
    #[error("secret store unavailable")]
    SecretUnavailable,
    /// No response — outcome unresolved; reconcile only, never re-fire pay.
    #[error("wallet outcome unknown")]
    Unknown,
}

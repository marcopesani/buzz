//! Decode and validate bolt11 invoices for the send use-case.
//!
//! One validation path for every payable invoice — resolver-returned and
//! raw pay-card bolt11s both enter here. Lud16 concerns end at the resolve
//! boundary.

use crate::error::WalletError;
use crate::ports::Clock;
use crate::types::Bolt11;
use bitcoin::hashes::Hash;
use buzz_core::payment::Amount;
use lightning_invoice::Bolt11Invoice;
use std::str::FromStr;
use std::time::Duration;

/// Fields extracted from a decoded, amount-bearing bolt11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedInvoice {
    pub bolt11: Bolt11,
    pub payment_hash_hex: String,
    pub amount: Amount,
    /// Unix seconds at which the invoice expires.
    pub expires_at_unix: u64,
}

/// Decode `bolt11`, require amount == `requested`, and reject expired invoices.
pub(crate) fn validate_payable_bolt11(
    bolt11: &Bolt11,
    requested: Amount,
    clock: &dyn Clock,
) -> Result<ValidatedInvoice, WalletError> {
    let invoice =
        Bolt11Invoice::from_str(bolt11.as_str()).map_err(|_| WalletError::ResolveRejected)?;

    let amount_msat = invoice
        .amount_milli_satoshis()
        .ok_or(WalletError::ResolveRejected)?;
    if amount_msat != requested.as_msat() {
        return Err(WalletError::ResolveRejected);
    }

    let expires_at = invoice
        .expires_at()
        .ok_or(WalletError::ResolveRejected)?
        .as_secs();
    let now = Duration::from_secs(clock.now_unix());
    if invoice.would_expire(now) {
        return Err(WalletError::ResolveRejected);
    }

    Ok(ValidatedInvoice {
        bolt11: bolt11.clone(),
        payment_hash_hex: hex::encode(invoice.payment_hash().to_byte_array()),
        amount: requested,
        expires_at_unix: expires_at,
    })
}

/// Fields decoded from an opaque bolt11 (no amount / expiry validation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBolt11 {
    /// Hex-encoded payment hash.
    pub payment_hash_hex: String,
    /// Unix seconds when the invoice expires.
    pub expires_at_unix: u64,
    /// Embedded amount in msat when the invoice is amount-bearing.
    pub amount_msat: Option<u64>,
}

/// Decode payment hash, expiry, and optional amount from an opaque bolt11.
///
/// Used by receive IPC (mint-time hash + expiry for payment-request events).
pub fn decode_bolt11(bolt11: &Bolt11) -> Result<DecodedBolt11, WalletError> {
    let invoice =
        Bolt11Invoice::from_str(bolt11.as_str()).map_err(|_| WalletError::ResolveRejected)?;
    let expires_at_unix = invoice
        .expires_at()
        .ok_or(WalletError::ResolveRejected)?
        .as_secs();
    Ok(DecodedBolt11 {
        payment_hash_hex: hex::encode(invoice.payment_hash().to_byte_array()),
        expires_at_unix,
        amount_msat: invoice.amount_milli_satoshis(),
    })
}

/// Extract the payment hash from an opaque bolt11 (no amount / expiry checks).
///
/// Used by [`check_incoming`](crate::Wallet::check_incoming) — the payee only
/// needs the hash to query its own wallet — and by test harnesses that verify
/// `sha256(preimage) == payment_hash` for invoices they minted.
pub fn payment_hash_hex(bolt11: &Bolt11) -> Result<String, WalletError> {
    let invoice =
        Bolt11Invoice::from_str(bolt11.as_str()).map_err(|_| WalletError::ResolveRejected)?;
    Ok(hex::encode(invoice.payment_hash().to_byte_array()))
}

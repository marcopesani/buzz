//! Real bolt11 minting via `lightning-invoice` `InvoiceBuilder`.

use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{Secp256k1, SecretKey as BitcoinSecretKey};
use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret, SignedRawBolt11Invoice};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::MockWalletError;

/// A minted bolt11 plus the preimage/hash pair used to build it.
#[derive(Debug, Clone)]
pub struct MintedInvoice {
    /// Signed bolt11 string.
    pub bolt11: String,
    /// Hex-encoded payment hash (`SHA256(preimage)`).
    pub payment_hash_hex: String,
    /// Hex-encoded 32-byte preimage.
    pub preimage_hex: String,
    /// Amount in millisatoshis.
    pub amount_msat: u64,
    /// Description stored on the invoice.
    pub description: String,
    /// Creation time (unix seconds).
    pub created_at: u64,
    /// Expiry time (unix seconds).
    pub expires_at: u64,
}

/// Mint a signed bolt11 with a fresh preimage, signed by `signing_secret`.
///
/// `signing_secret` is 32 raw secp256k1 secret-key bytes (typically the wallet
/// nostr secret). Preimage/hash pairs pass [`buzz_core::payment::verify`].
pub fn mint_bolt11(
    amount_msat: u64,
    description: &str,
    expiry_secs: u64,
    signing_secret: &[u8; 32],
    preimage: [u8; 32],
) -> Result<MintedInvoice, MockWalletError> {
    let payment_hash = bitcoin::hashes::sha256::Hash::hash(&preimage);
    let payment_secret = PaymentSecret(preimage);
    let created_at = unix_now();
    let expires_at = created_at.saturating_add(expiry_secs);

    let builder = InvoiceBuilder::new(Currency::Bitcoin)
        .description(description.to_string())
        .payment_hash(payment_hash)
        .payment_secret(payment_secret)
        .duration_since_epoch(Duration::from_secs(created_at))
        .min_final_cltv_expiry_delta(144)
        .expiry_time(Duration::from_secs(expiry_secs))
        .amount_milli_satoshis(amount_msat);

    let secp = Secp256k1::new();
    let secret_key = BitcoinSecretKey::from_slice(signing_secret)
        .map_err(|e| MockWalletError::Invoice(format!("invalid signing key: {e}")))?;

    let invoice = builder
        .build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &secret_key))
        .map_err(|e| MockWalletError::Invoice(format!("bolt11 build failed: {e}")))?;

    let bolt11 = invoice.to_string();
    // Ensure the string round-trips as a signed invoice.
    let _: SignedRawBolt11Invoice = bolt11
        .parse()
        .map_err(|e| MockWalletError::Invoice(format!("minted bolt11 parse failed: {e}")))?;

    Ok(MintedInvoice {
        bolt11,
        payment_hash_hex: hex::encode(payment_hash.to_byte_array()),
        preimage_hex: hex::encode(preimage),
        amount_msat,
        description: description.to_string(),
        created_at,
        expires_at,
    })
}

/// Current unix time in seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decode amount_msat and payment_hash from a bolt11 string.
pub fn decode_bolt11(bolt11: &str) -> Result<(Option<u64>, String), MockWalletError> {
    use lightning_invoice::Bolt11Invoice;
    let invoice: Bolt11Invoice = bolt11
        .parse()
        .map_err(|e| MockWalletError::Invoice(format!("bolt11 decode failed: {e}")))?;
    let amount_msat = invoice.amount_milli_satoshis();
    let hash = hex::encode(invoice.payment_hash().to_byte_array());
    Ok((amount_msat, hash))
}

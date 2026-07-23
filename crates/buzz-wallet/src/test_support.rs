//! Harness helpers shared by unit tests and the future mock wallet (U7).
//!
//! Not on the production pay path — minting signed bolt11s is a test /
//! loopback concern. Lives outside `cfg(test)` so `buzz-mock-wallet` can
//! depend on the same factory without duplicating `InvoiceBuilder` wiring.

use crate::types::Bolt11;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::PrivateKey;
use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use std::time::Duration;

/// Inputs for [`mint_bolt11`].
#[derive(Debug, Clone)]
pub struct MintInvoiceParams {
    /// Amount in millisatoshis. `None` mints an amountless invoice.
    pub amount_msat: Option<u64>,
    /// 32-byte payment preimage; `payment_hash = SHA256(preimage)`.
    pub preimage: [u8; 32],
    /// Invoice creation time (unix seconds) — drives expiry with [`Self::expiry_secs`].
    pub timestamp_unix: u64,
    /// Seconds after `timestamp_unix` until the invoice expires.
    pub expiry_secs: u64,
    /// Optional description (defaults to a short placeholder).
    pub description: Option<&'static str>,
}

/// A minted bolt11 plus the hash / preimage pair used to sign it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedInvoice {
    /// Signed bolt11 string.
    pub bolt11: Bolt11,
    /// Hex-encoded payment hash (`SHA256(preimage)`).
    pub payment_hash_hex: String,
    /// Hex-encoded 32-byte preimage.
    pub preimage_hex: String,
}

/// Mint a valid, signed bolt11 for tests and the local mock wallet.
///
/// Uses a fixed signing key — deterministic enough for harness use, never a
/// production secret.
pub fn mint_bolt11(params: MintInvoiceParams) -> MintedInvoice {
    let payment_hash = bitcoin::hashes::sha256::Hash::hash(&params.preimage);
    let payment_secret = PaymentSecret([0x42; 32]);
    let description = params.description.unwrap_or("buzz-wallet test invoice");

    let mut builder = InvoiceBuilder::new(Currency::Bitcoin)
        .description(description.into())
        .payment_hash(payment_hash)
        .payment_secret(payment_secret)
        .duration_since_epoch(Duration::from_secs(params.timestamp_unix))
        .min_final_cltv_expiry_delta(144)
        .expiry_time(Duration::from_secs(params.expiry_secs));

    if let Some(msat) = params.amount_msat {
        builder = builder.amount_milli_satoshis(msat);
    }

    let secp = Secp256k1::new();
    // Fixed key: only for harness invoices, never production spend keys.
    let private_key = PrivateKey::from_slice(&[0x01; 32], bitcoin::Network::Bitcoin)
        .expect("fixed 32-byte key is valid for secp256k1");
    let secret_key = SecretKey::from_slice(&private_key.to_bytes())
        .expect("PrivateKey bytes are a valid SecretKey");

    let invoice = builder
        .build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &secret_key))
        .expect("InvoiceBuilder inputs satisfy BOLT11 invariants");

    MintedInvoice {
        bolt11: Bolt11::new(invoice.to_string()),
        payment_hash_hex: hex::encode(payment_hash.to_byte_array()),
        preimage_hex: hex::encode(params.preimage),
    }
}

/// Convenience: mint an invoice for `amount_msat` with a given preimage byte.
pub fn mint_bolt11_for_amount(
    amount_msat: u64,
    preimage_byte: u8,
    timestamp_unix: u64,
) -> MintedInvoice {
    mint_bolt11(MintInvoiceParams {
        amount_msat: Some(amount_msat),
        preimage: [preimage_byte; 32],
        timestamp_unix,
        expiry_secs: 3_600,
        description: None,
    })
}

//! Pure Lightning payment domain — amount, preimage verification, and
//! request/receipt tag validation.
//!
//! No network I/O and no bolt11 decoding. A bolt11 invoice is an opaque
//! [`String`] here; decoding lands in `buzz-wallet`.

use sha2::{Digest, Sha256};
use std::str::FromStr;
use thiserror::Error;

/// Millisatoshis. The domain speaks msat everywhere — event tags included.
///
/// Sats exist only at the UI boundary. No floating-point amounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(u64);

impl Amount {
    /// Construct from a millisatoshi count.
    pub fn from_msat(msat: u64) -> Self {
        Self(msat)
    }

    /// Millisatoshis represented by this amount.
    pub fn as_msat(self) -> u64 {
        self.0
    }

    /// Parse an amount tag value: ASCII decimal digits only.
    ///
    /// Rejects empty strings, signs (`+`/`-`), floats, and values that do not
    /// fit in `u64`. Leading zeros are accepted (`"001"` → 1 msat) because they
    /// are still decimal digits and parse to the same integer.
    pub fn parse_tag(s: &str) -> Result<Self, PaymentError> {
        if s.is_empty() {
            return Err(PaymentError::MalformedAmount);
        }
        if !s.chars().all(|c| c.is_ascii_digit()) {
            return Err(PaymentError::MalformedAmount);
        }
        let msat = u64::from_str(s).map_err(|_| PaymentError::MalformedAmount)?;
        Ok(Self(msat))
    }

    /// Render this amount as a decimal tag string (no leading zeros except `"0"`).
    pub fn to_tag_string(self) -> String {
        self.0.to_string()
    }
}

/// Returns `true` iff `SHA256(preimage) == payment_hash`.
///
/// Inputs are hex strings as they appear in event tags. Malformed hex or wrong
/// lengths (both must decode to exactly 32 bytes) yield `false` — verification
/// of untrusted data has one question and one answer.
pub fn verify(preimage: &str, payment_hash: &str) -> bool {
    let Ok(preimage_bytes) = hex::decode(preimage) else {
        return false;
    };
    let Ok(hash_bytes) = hex::decode(payment_hash) else {
        return false;
    };
    if preimage_bytes.len() != 32 || hash_bytes.len() != 32 {
        return false;
    }
    let digest = Sha256::digest(&preimage_bytes);
    digest.as_slice() == hash_bytes.as_slice()
}

/// How a payment request names its pay target.
///
/// After validation, "neither bolt11 nor lud16" is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentTarget {
    /// Pay a bolt11 invoice (opaque string — not decoded here).
    Bolt11(String),
    /// Pay a Lightning Address (LUD-16).
    Lud16(String),
    /// Both a bolt11 and a lud16 were present on the request.
    Both {
        /// Opaque bolt11 invoice string.
        bolt11: String,
        /// Lightning Address.
        lud16: String,
    },
}

/// A validated kind-40009 payment request (tag shape only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRequest {
    /// Amount in millisatoshis (must be > 0).
    pub amount: Amount,
    /// Optional human-readable memo.
    pub memo: Option<String>,
    /// Payment target — at least one of bolt11 / lud16.
    pub target: PaymentTarget,
    /// Channel id from the required `h` tag.
    pub channel_id: String,
    /// Payee pubkey from the required `p` tag.
    pub payee_pubkey: String,
    /// Optional unix-seconds expiry from the `expiry` tag (never `expiration`).
    pub expiry: Option<u64>,
}

/// A validated kind-40010 payment receipt (decorative shape only).
///
/// Shape validation does not imply the payment settled — trust stays local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentReceipt {
    /// Request event id from the bare `e` tag.
    pub request_id: String,
    /// Hex-encoded payment hash tag value.
    pub payment_hash: String,
    /// Hex-encoded preimage tag value.
    pub preimage: String,
    /// Amount claimed on the receipt (msat).
    pub amount: Amount,
}

/// Parse/validation failures for payment request and receipt tags.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PaymentError {
    /// A required tag name is absent or empty.
    #[error("missing required tag: {0}")]
    MissingTag(&'static str),
    /// A present tag value failed structural parsing.
    #[error("malformed tag: {0}")]
    MalformedTag(&'static str),
    /// An amount tag was not a strict decimal integer string.
    #[error("malformed amount")]
    MalformedAmount,
    /// Amount was present but zero — payment requests must ask for > 0 msat.
    #[error("amount must be greater than zero")]
    ZeroAmount,
    /// Neither `bolt11` nor `lud16` was present.
    #[error("payment target requires bolt11 or lud16")]
    MissingPaymentTarget,
}

impl PaymentRequest {
    /// Parse and validate a kind-40009 tag set.
    ///
    /// Required: non-zero `amount`, `h`, `p`, and at least one of `bolt11` /
    /// `lud16`. Optional: `memo`, `expiry` (tag spelling is `expiry` only).
    pub fn from_tags(tags: &[Vec<String>]) -> Result<Self, PaymentError> {
        let amount = match first_tag(tags, "amount") {
            None => return Err(PaymentError::MissingTag("amount")),
            Some(raw) => Amount::parse_tag(raw)?,
        };
        if amount.as_msat() == 0 {
            return Err(PaymentError::ZeroAmount);
        }

        let channel_id = required_nonempty(tags, "h")?.to_string();
        let payee_pubkey = required_nonempty(tags, "p")?.to_string();
        let target = payment_target(tags)?;
        let memo = optional_nonempty(tags, "memo").map(str::to_string);
        let expiry = match first_tag(tags, "expiry") {
            None => None,
            Some(raw) => Some(parse_unix_seconds(raw)?),
        };

        Ok(Self {
            amount,
            memo,
            target,
            channel_id,
            payee_pubkey,
            expiry,
        })
    }
}

impl PaymentReceipt {
    /// Parse and validate a kind-40010 tag set (shape only).
    ///
    /// Required: bare `e` (exactly `["e", "<id>"]`), `payment_hash`,
    /// `preimage`, and `amount`. Does not call [`verify`] — receipts are
    /// decorative; trust is local.
    pub fn from_tags(tags: &[Vec<String>]) -> Result<Self, PaymentError> {
        let request_id = bare_e_tag(tags)?.to_string();
        let payment_hash = required_nonempty(tags, "payment_hash")?.to_string();
        let preimage = required_nonempty(tags, "preimage")?.to_string();
        let amount = match first_tag(tags, "amount") {
            None => return Err(PaymentError::MissingTag("amount")),
            Some(raw) => Amount::parse_tag(raw)?,
        };

        Ok(Self {
            request_id,
            payment_hash,
            preimage,
            amount,
        })
    }
}

fn first_tag<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|t| t.first().map(|s| s.as_str()) == Some(name))
        .and_then(|t| t.get(1).map(|s| s.as_str()))
}

fn required_nonempty<'a>(
    tags: &'a [Vec<String>],
    name: &'static str,
) -> Result<&'a str, PaymentError> {
    match first_tag(tags, name) {
        Some(v) if !v.is_empty() => Ok(v),
        Some(_) => Err(PaymentError::MalformedTag(name)),
        None => Err(PaymentError::MissingTag(name)),
    }
}

fn optional_nonempty<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    first_tag(tags, name).filter(|v| !v.is_empty())
}

fn payment_target(tags: &[Vec<String>]) -> Result<PaymentTarget, PaymentError> {
    let bolt11 = optional_nonempty(tags, "bolt11").map(str::to_string);
    let lud16 = optional_nonempty(tags, "lud16").map(str::to_string);
    match (bolt11, lud16) {
        (Some(bolt11), Some(lud16)) => Ok(PaymentTarget::Both { bolt11, lud16 }),
        (Some(bolt11), None) => Ok(PaymentTarget::Bolt11(bolt11)),
        (None, Some(lud16)) => Ok(PaymentTarget::Lud16(lud16)),
        (None, None) => Err(PaymentError::MissingPaymentTarget),
    }
}

/// Bare `e` means exactly two fields: name + event id (no NIP-10 markers).
fn bare_e_tag(tags: &[Vec<String>]) -> Result<&str, PaymentError> {
    let tag = tags
        .iter()
        .find(|t| t.first().map(|s| s.as_str()) == Some("e"))
        .ok_or(PaymentError::MissingTag("e"))?;
    match tag.as_slice() {
        [_, id] if !id.is_empty() => Ok(id.as_str()),
        _ => Err(PaymentError::MalformedTag("e")),
    }
}

fn parse_unix_seconds(raw: &str) -> Result<u64, PaymentError> {
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return Err(PaymentError::MalformedTag("expiry"));
    }
    u64::from_str(raw).map_err(|_| PaymentError::MalformedTag("expiry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;
    use std::path::PathBuf;

    fn tags(pairs: &[(&str, &str)]) -> Vec<Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| vec![(*k).to_string(), (*v).to_string()])
            .collect()
    }

    fn request_base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("amount", "1000"),
            ("bolt11", "lnbc1opaque"),
            ("h", "channel-uuid"),
            ("p", "payee-pubkey-hex"),
        ]
    }

    // --- Amount -----------------------------------------------------------

    #[test]
    fn amount_parse_accepts_decimal_digits() {
        assert_eq!(Amount::parse_tag("0").unwrap().as_msat(), 0);
        assert_eq!(Amount::parse_tag("500000").unwrap().as_msat(), 500_000);
    }

    #[test]
    fn amount_parse_accepts_leading_zeros() {
        // Leading zeros are decimal digits and parse to the same msat value.
        assert_eq!(Amount::parse_tag("001").unwrap().as_msat(), 1);
        assert_eq!(Amount::parse_tag("000").unwrap().as_msat(), 0);
    }

    #[test]
    fn amount_parse_rejects_empty() {
        assert_eq!(Amount::parse_tag(""), Err(PaymentError::MalformedAmount));
    }

    #[test]
    fn amount_parse_rejects_float() {
        assert_eq!(
            Amount::parse_tag("12.5"),
            Err(PaymentError::MalformedAmount)
        );
    }

    #[test]
    fn amount_parse_rejects_negative() {
        assert_eq!(Amount::parse_tag("-1"), Err(PaymentError::MalformedAmount));
    }

    #[test]
    fn amount_parse_rejects_plus_sign() {
        assert_eq!(Amount::parse_tag("+1"), Err(PaymentError::MalformedAmount));
    }

    #[test]
    fn amount_parse_rejects_overflow() {
        assert_eq!(
            Amount::parse_tag("18446744073709551616"),
            Err(PaymentError::MalformedAmount)
        );
    }

    #[test]
    fn amount_to_tag_string_roundtrips() {
        let a = Amount::from_msat(42);
        assert_eq!(a.to_tag_string(), "42");
        assert_eq!(Amount::parse_tag(&a.to_tag_string()).unwrap(), a);
    }

    // --- verify + shared vectors ------------------------------------------

    #[derive(Debug, Deserialize)]
    struct VerifyVector {
        name: String,
        preimage: String,
        payment_hash: String,
        valid: bool,
    }

    #[test]
    fn preimage_verification_vectors() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/wallet/verify_vectors.json");
        let raw =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let vectors: Vec<VerifyVector> =
            serde_json::from_str(&raw).expect("verify_vectors.json must be valid");
        assert!(!vectors.is_empty(), "vector table must not be empty");

        for v in vectors {
            let got = verify(&v.preimage, &v.payment_hash);
            assert_eq!(
                got, v.valid,
                "vector {}: verify({}, {}) returned {}, expected {}",
                v.name, v.preimage, v.payment_hash, got, v.valid
            );
        }
    }

    // --- PaymentRequest ---------------------------------------------------

    #[test]
    fn request_bolt11_only_ok() {
        let t = tags(&request_base());
        let req = PaymentRequest::from_tags(&t).unwrap();
        assert_eq!(req.amount.as_msat(), 1000);
        assert_eq!(req.target, PaymentTarget::Bolt11("lnbc1opaque".into()));
        assert_eq!(req.channel_id, "channel-uuid");
        assert_eq!(req.payee_pubkey, "payee-pubkey-hex");
        assert_eq!(req.memo, None);
        assert_eq!(req.expiry, None);
    }

    #[test]
    fn request_lud16_only_ok() {
        let mut base = request_base();
        base.retain(|(k, _)| *k != "bolt11");
        base.push(("lud16", "alice@example.com"));
        let req = PaymentRequest::from_tags(&tags(&base)).unwrap();
        assert_eq!(req.target, PaymentTarget::Lud16("alice@example.com".into()));
    }

    #[test]
    fn request_both_targets_ok() {
        let mut base = request_base();
        base.push(("lud16", "alice@example.com"));
        let req = PaymentRequest::from_tags(&tags(&base)).unwrap();
        assert_eq!(
            req.target,
            PaymentTarget::Both {
                bolt11: "lnbc1opaque".into(),
                lud16: "alice@example.com".into(),
            }
        );
    }

    #[test]
    fn request_missing_amount() {
        let mut base = request_base();
        base.retain(|(k, _)| *k != "amount");
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::MissingTag("amount"))
        );
    }

    #[test]
    fn request_missing_h() {
        let mut base = request_base();
        base.retain(|(k, _)| *k != "h");
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::MissingTag("h"))
        );
    }

    #[test]
    fn request_missing_p() {
        let mut base = request_base();
        base.retain(|(k, _)| *k != "p");
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::MissingTag("p"))
        );
    }

    #[test]
    fn request_missing_payment_target() {
        let mut base = request_base();
        base.retain(|(k, _)| *k != "bolt11");
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::MissingPaymentTarget)
        );
    }

    #[test]
    fn request_zero_amount() {
        let mut base = request_base();
        for (k, v) in &mut base {
            if *k == "amount" {
                *v = "0";
            }
        }
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::ZeroAmount)
        );
    }

    #[test]
    fn request_expiry_ok() {
        let mut base = request_base();
        base.push(("expiry", "1700000000"));
        base.push(("memo", "coffee"));
        let req = PaymentRequest::from_tags(&tags(&base)).unwrap();
        assert_eq!(req.expiry, Some(1_700_000_000));
        assert_eq!(req.memo.as_deref(), Some("coffee"));
    }

    #[test]
    fn request_expiry_malformed() {
        let mut base = request_base();
        base.push(("expiry", "not-a-unix-ts"));
        assert_eq!(
            PaymentRequest::from_tags(&tags(&base)),
            Err(PaymentError::MalformedTag("expiry"))
        );
    }

    #[test]
    fn request_does_not_accept_expiration_as_expiry() {
        // Deliberate NIP-40 divergence: builders must not normalize the tag name.
        let mut base = request_base();
        base.push(("expiration", "1700000000"));
        let req = PaymentRequest::from_tags(&tags(&base)).unwrap();
        assert_eq!(req.expiry, None);
    }

    // --- PaymentReceipt ---------------------------------------------------

    fn receipt_tags() -> Vec<Vec<String>> {
        tags(&[
            ("e", "request-event-id"),
            (
                "payment_hash",
                "72cd6e8422c407fb6d098690f1130b7ded7ec2f7f5e1d30bd9d521f015363793",
            ),
            (
                "preimage",
                "0101010101010101010101010101010101010101010101010101010101010101",
            ),
            ("amount", "500000"),
        ])
    }

    #[test]
    fn receipt_shape_ok() {
        let r = PaymentReceipt::from_tags(&receipt_tags()).unwrap();
        assert_eq!(r.request_id, "request-event-id");
        assert_eq!(r.amount.as_msat(), 500_000);
    }

    #[test]
    fn receipt_missing_e() {
        let mut t = receipt_tags();
        t.retain(|tag| tag[0] != "e");
        assert_eq!(
            PaymentReceipt::from_tags(&t),
            Err(PaymentError::MissingTag("e"))
        );
    }

    #[test]
    fn receipt_rejects_marked_e() {
        let mut t = receipt_tags();
        for tag in &mut t {
            if tag[0] == "e" {
                tag.push("root".into());
            }
        }
        assert_eq!(
            PaymentReceipt::from_tags(&t),
            Err(PaymentError::MalformedTag("e"))
        );
    }

    #[test]
    fn receipt_missing_payment_hash() {
        let mut t = receipt_tags();
        t.retain(|tag| tag[0] != "payment_hash");
        assert_eq!(
            PaymentReceipt::from_tags(&t),
            Err(PaymentError::MissingTag("payment_hash"))
        );
    }

    #[test]
    fn receipt_missing_preimage() {
        let mut t = receipt_tags();
        t.retain(|tag| tag[0] != "preimage");
        assert_eq!(
            PaymentReceipt::from_tags(&t),
            Err(PaymentError::MissingTag("preimage"))
        );
    }

    #[test]
    fn receipt_missing_amount() {
        let mut t = receipt_tags();
        t.retain(|tag| tag[0] != "amount");
        assert_eq!(
            PaymentReceipt::from_tags(&t),
            Err(PaymentError::MissingTag("amount"))
        );
    }

    #[test]
    fn receipt_shape_does_not_require_matching_preimage() {
        // Decorative only — a forged preimage still parses.
        let mut t = receipt_tags();
        for tag in &mut t {
            if tag[0] == "preimage" {
                tag[1] = "ff".repeat(32);
            }
        }
        let r = PaymentReceipt::from_tags(&t).unwrap();
        assert!(!verify(&r.preimage, &r.payment_hash));
    }
}

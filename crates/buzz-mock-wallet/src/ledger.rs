//! In-memory msat ledger and invoice book — the single owner of balance state.

use crate::error::MockWalletError;
use crate::mint::{decode_bolt11, mint_bolt11, unix_now, MintedInvoice};
use nostr::Keys;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// Lifecycle of a minted invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceState {
    /// Awaiting payment.
    Pending,
    /// Paid (preimage revealed).
    Settled,
    /// Past expiry and still unpaid.
    Expired,
}

/// One invoice tracked by the ledger.
#[derive(Debug, Clone)]
pub struct TrackedInvoice {
    /// Minted bolt11 and secrets.
    pub minted: MintedInvoice,
    /// Current state.
    pub state: InvoiceState,
    /// Settlement time (unix seconds), if settled.
    pub settled_at: Option<u64>,
    /// When true, paying this invoice credits the ledger after debit (self-pay
    /// net-zero). When false, the debit sticks — payee is external.
    pub credit_on_settle: bool,
}

/// Scriptable failure / timing knobs — data, not code paths in handlers.
#[derive(Debug, Clone, Default)]
pub struct Script {
    /// If set, the next `pay_invoice` returns this NIP-47 error code then clears.
    pub fail_next_pay_code: Option<String>,
    /// Fixed delay before every RPC response.
    pub response_delay: std::time::Duration,
    /// When true, the daemon swallows requests (no 23195).
    pub swallow_requests: bool,
    /// Extra method names appended to the `get_info` response only.
    ///
    /// Mirrors real wallets (e.g. Alby: `sign_message`, `get_budget`) whose
    /// extension methods are unknown to strict NIP-47 client enums and make
    /// the whole `get_info` response undeserializable for them.
    pub get_info_extra_methods: Vec<String>,
}

/// Shared msat ledger.
#[derive(Debug)]
pub struct Ledger {
    inner: Mutex<LedgerInner>,
    signing_secret: [u8; 32],
}

#[derive(Debug)]
struct LedgerInner {
    balance_msat: u64,
    invoices: HashMap<String, TrackedInvoice>,
    script: Script,
}

impl Ledger {
    /// Create a ledger with `starting_balance_msat` and the wallet signing key.
    pub fn new(starting_balance_msat: u64, wallet_keys: &Keys, script: Script) -> Arc<Self> {
        let mut signing_secret = [0u8; 32];
        signing_secret.copy_from_slice(wallet_keys.secret_key().as_secret_bytes());
        Arc::new(Self {
            inner: Mutex::new(LedgerInner {
                balance_msat: starting_balance_msat,
                invoices: HashMap::new(),
                script,
            }),
            signing_secret,
        })
    }

    /// Current balance in millisatoshis.
    pub fn balance_msat(&self) -> u64 {
        self.inner.lock().balance_msat
    }

    /// Snapshot of script knobs.
    pub fn script(&self) -> Script {
        self.inner.lock().script.clone()
    }

    /// Replace script knobs (tests / CLI).
    pub fn set_script(&self, script: Script) {
        self.inner.lock().script = script;
    }

    /// Take and clear `fail_next_pay_code` if present.
    pub fn take_fail_next_pay_code(&self) -> Option<String> {
        self.inner.lock().script.fail_next_pay_code.take()
    }

    /// Mint and track a new pending invoice.
    pub fn make_invoice(
        &self,
        amount_msat: u64,
        description: &str,
        expiry_secs: u64,
    ) -> Result<MintedInvoice, MockWalletError> {
        let preimage = random_32();
        let minted = mint_bolt11(
            amount_msat,
            description,
            expiry_secs,
            &self.signing_secret,
            preimage,
        )?;
        let tracked = TrackedInvoice {
            minted: minted.clone(),
            state: InvoiceState::Pending,
            settled_at: None,
            credit_on_settle: true,
        };
        self.inner
            .lock()
            .invoices
            .insert(minted.payment_hash_hex.clone(), tracked);
        Ok(minted)
    }

    /// Mint a payee invoice: pay returns a verifying preimage and leaves the
    /// debit in place (no self-pay credit).
    pub fn make_payee_invoice(
        &self,
        amount_msat: u64,
        description: &str,
        expiry_secs: u64,
    ) -> Result<MintedInvoice, MockWalletError> {
        let preimage = random_32();
        // Sign with a fresh key so the invoice is not the wallet's receive key,
        // while still tracking the preimage for a verifying settle.
        let foreign = Keys::generate();
        let mut signing = [0u8; 32];
        signing.copy_from_slice(foreign.secret_key().as_secret_bytes());
        let minted = mint_bolt11(amount_msat, description, expiry_secs, &signing, preimage)?;
        let tracked = TrackedInvoice {
            minted: minted.clone(),
            state: InvoiceState::Pending,
            settled_at: None,
            credit_on_settle: false,
        };
        self.inner
            .lock()
            .invoices
            .insert(minted.payment_hash_hex.clone(), tracked);
        Ok(minted)
    }

    /// Look up by payment_hash hex. Refreshes expired state.
    pub fn lookup(&self, payment_hash_hex: &str) -> Option<TrackedInvoice> {
        let mut inner = self.inner.lock();
        let inv = inner.invoices.get_mut(payment_hash_hex)?;
        if inv.state == InvoiceState::Pending && unix_now() >= inv.minted.expires_at {
            inv.state = InvoiceState::Expired;
        }
        Some(inv.clone())
    }

    /// Pay a bolt11: debit balance; settle if it is one of ours.
    ///
    /// Paying our own invoice debits then credits (net zero) and marks settled so
    /// `lookup_invoice` / 23196 see a real receive. Paying an unknown invoice only
    /// debits (synthetic settle for client timeout / pay-path tests).
    pub fn pay_invoice(&self, bolt11: &str) -> Result<PayOutcome, PayError> {
        let (amount_opt, hash) =
            decode_bolt11(bolt11).map_err(|e| PayError::Other(e.to_string()))?;
        let amount_msat = amount_opt.ok_or_else(|| {
            PayError::Other("amountless invoices are not supported by the mock".into())
        })?;

        let mut inner = self.inner.lock();

        if let Some(inv) = inner.invoices.get(&hash) {
            if inv.state == InvoiceState::Settled {
                return Ok(PayOutcome {
                    preimage_hex: inv.minted.preimage_hex.clone(),
                    payment_hash_hex: hash,
                    amount_msat,
                    settled_ours: true,
                    bolt11: inv.minted.bolt11.clone(),
                    description: inv.minted.description.clone(),
                    created_at: inv.minted.created_at,
                    expires_at: inv.minted.expires_at,
                    settled_at: inv.settled_at.unwrap_or_else(unix_now),
                });
            }
            if inv.state == InvoiceState::Expired
                || (inv.state == InvoiceState::Pending && unix_now() >= inv.minted.expires_at)
            {
                if let Some(inv) = inner.invoices.get_mut(&hash) {
                    inv.state = InvoiceState::Expired;
                }
                return Err(PayError::Other("invoice expired".into()));
            }
        }

        if inner.balance_msat < amount_msat {
            return Err(PayError::InsufficientBalance {
                balance_msat: inner.balance_msat,
                needed_msat: amount_msat,
            });
        }
        inner.balance_msat -= amount_msat;

        if let Some(inv) = inner.invoices.get_mut(&hash) {
            let now = unix_now();
            inv.state = InvoiceState::Settled;
            inv.settled_at = Some(now);
            let credit = inv.credit_on_settle;
            let outcome = PayOutcome {
                preimage_hex: inv.minted.preimage_hex.clone(),
                payment_hash_hex: hash,
                amount_msat,
                settled_ours: credit,
                bolt11: inv.minted.bolt11.clone(),
                description: inv.minted.description.clone(),
                created_at: inv.minted.created_at,
                expires_at: inv.minted.expires_at,
                settled_at: now,
            };
            drop(inner);
            if credit {
                // Self-pay: credit inbound settle (nets to zero).
                let mut inner = self.inner.lock();
                inner.balance_msat = inner.balance_msat.saturating_add(amount_msat);
            }
            return Ok(outcome);
        }

        // Unknown invoice: settle synthetically with a fresh preimage (balance stays debited).
        let preimage = random_32();
        Ok(PayOutcome {
            preimage_hex: hex::encode(preimage),
            payment_hash_hex: hash,
            amount_msat,
            settled_ours: false,
            bolt11: bolt11.to_string(),
            description: String::new(),
            created_at: unix_now(),
            expires_at: unix_now(),
            settled_at: unix_now(),
        })
    }
}

/// Successful `pay_invoice` result for the daemon to package into NIP-47 + 23196.
#[derive(Debug, Clone)]
pub struct PayOutcome {
    /// Hex preimage returned to the payer.
    pub preimage_hex: String,
    /// Payment hash hex.
    pub payment_hash_hex: String,
    /// Amount paid in msat.
    pub amount_msat: u64,
    /// True when the bolt11 was minted by this ledger.
    pub settled_ours: bool,
    /// Bolt11 string.
    pub bolt11: String,
    /// Invoice description.
    pub description: String,
    /// Creation unix seconds.
    pub created_at: u64,
    /// Expiry unix seconds.
    pub expires_at: u64,
    /// Settlement unix seconds.
    pub settled_at: u64,
}

/// Pay-path errors mapped to NIP-47 codes by the daemon.
#[derive(Debug, Clone)]
pub enum PayError {
    /// Ledger short.
    InsufficientBalance {
        /// Current balance.
        balance_msat: u64,
        /// Required amount.
        needed_msat: u64,
    },
    /// Other pay failure.
    Other(String),
}

fn random_32() -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(Keys::generate().secret_key().as_secret_bytes());
    out
}

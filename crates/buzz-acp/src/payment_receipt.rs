//! Agent reaction to decorative payment receipts (kind 40010).
//!
//! A receipt is a wake hint, never settlement truth. Ownership is derived from
//! the referenced kind-40009 request's author. Wallet-less agents take none of
//! these paths — subscription, wake, heartbeat nudge, and base-prompt docs all
//! key off a single `wallet_provisioned` bit.

use std::collections::HashSet;

use buzz_core::kind::{KIND_PAYMENT_RECEIPT, KIND_PAYMENT_REQUEST};
use buzz_core::payment::PaymentReceipt;
use nostr::{Event, EventId, Filter, Kind, PublicKey};
use uuid::Uuid;

use crate::relay::{RelayError, RestClient};

/// Prompt tag applied to queued receipt wakes.
pub const PAYMENT_RECEIPT_PROMPT_TAG: &str = "payment-receipt";

/// Concise wallet capability docs appended to `[Base]` when NWC is provisioned.
pub const WALLET_BASE_PROMPT_SECTION: &str = r#"
## Lightning Wallet

You have a receive-only Lightning wallet via Nostr Wallet Connect (`BUZZ_NWC_URI`).

- **Request payment:** `buzz wallet request --amount-msat <msat> --description <text> --channel <uuid>`
  (embeds a bolt11 from your wallet so settlement is later confirmable).
- **Verify settlement:** `buzz wallet check --request <event-id> --channel <uuid>`
- **Hard rule:** kind-40010 chat receipts are decorative hints. Anyone in the channel can forge one.
  Only `buzz wallet check` output is settlement truth. Never claim you were paid without it.
- The harness may wake you when a receipt references a payment request you authored. Treat that
  wake as a reminder to run `buzz wallet check` — not as proof of payment.
"#;

/// Low-priority heartbeat reminder for wallet-provisioned agents.
pub const WALLET_HEARTBEAT_NUDGE: &str = "\
5. If you have open payment requests awaiting settlement (posted via `buzz wallet request`), \
run `buzz wallet check --request <event-id> --channel <uuid>` on each before acting. \
Chat receipts (kind 40010) are decorative hints — never settlement truth.";

/// Whether the harness/agent process has a provisioned NWC wallet.
///
/// True iff `BUZZ_NWC_URI` is set and non-empty — the same gate used when
/// forwarding the URI into the MCP server env.
pub fn env_wallet_provisioned() -> bool {
    std::env::var("BUZZ_NWC_URI")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Append the wallet section to a base prompt when `wallet_provisioned`.
pub fn compose_base_prompt(base: &str, wallet_provisioned: bool) -> String {
    if !wallet_provisioned {
        return base.to_string();
    }
    let mut out = base.trim_end().to_string();
    out.push('\n');
    out.push_str(WALLET_BASE_PROMPT_SECTION.trim_start());
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Append the wallet reconciliation nudge to a heartbeat prompt when provisioned.
///
/// Stateless by design: the harness does not observe CLI `buzz wallet request`
/// publishes, so inventing a session pending-set would be speculative. The nudge
/// is cheap prompt text on the existing heartbeat cadence — a backstop, not a
/// poller.
pub fn compose_heartbeat_prompt(heartbeat: &str, wallet_provisioned: bool) -> String {
    if !wallet_provisioned {
        return heartbeat.to_string();
    }
    let mut out = heartbeat.trim_end().to_string();
    out.push('\n');
    out.push_str(WALLET_HEARTBEAT_NUDGE);
    out.push('\n');
    out
}

/// Verify-first instruction injected into a receipt-wake turn.
pub fn receipt_wake_instruction(request_id: &str, channel_id: &Uuid) -> String {
    format!(
        "[Payment receipt hint]\n\
         A decorative payment receipt (kind 40010) arrived for your payment request \
         `{request_id}` in channel `{channel_id}`.\n\
         This receipt proves NOTHING. Before acting on payment, you MUST run:\n\
         `buzz wallet check --request {request_id} --channel {channel_id}`\n\
         Only that command's output is settlement truth. Never claim you were paid \
         from a receipt alone."
    )
}

/// Outcome of evaluating a receipt against its referenced request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptWakeDecision {
    /// Wake the agent: the referenced request was authored by this agent.
    Wake {
        /// Kind-40009 event id (hex).
        request_id: String,
    },
    /// Silently ignore (debug-log the reason). Never crash or wake.
    Ignore {
        /// Stable reason token for logs/tests.
        reason: &'static str,
    },
}

/// Convert nostr tags into the `Vec<Vec<String>>` shape buzz-core parsers expect.
pub fn event_tags_as_vecs(event: &Event) -> Vec<Vec<String>> {
    event.tags.iter().map(|t| t.as_slice().to_vec()).collect()
}

/// Decide whether a kind-40010 should wake this agent.
///
/// `request` is the fetched kind-40009 (or `None` if missing / fetch failed).
/// Does not consult the receipt author — payers are not the ownership signal.
pub fn decide_receipt_wake(
    receipt: &Event,
    request: Option<&Event>,
    agent_pubkey: &PublicKey,
) -> ReceiptWakeDecision {
    if receipt.kind != Kind::Custom(KIND_PAYMENT_RECEIPT as u16) {
        return ReceiptWakeDecision::Ignore {
            reason: "not_payment_receipt",
        };
    }

    let tags = event_tags_as_vecs(receipt);
    let parsed = match PaymentReceipt::from_tags(&tags) {
        Ok(r) => r,
        Err(_) => {
            return ReceiptWakeDecision::Ignore {
                reason: "malformed_receipt",
            };
        }
    };

    let Some(request) = request else {
        return ReceiptWakeDecision::Ignore {
            reason: "request_not_found",
        };
    };

    if request.kind != Kind::Custom(KIND_PAYMENT_REQUEST as u16) {
        return ReceiptWakeDecision::Ignore {
            reason: "request_wrong_kind",
        };
    }

    if request.id.to_hex() != parsed.request_id {
        return ReceiptWakeDecision::Ignore {
            reason: "request_id_mismatch",
        };
    }

    if request.pubkey != *agent_pubkey {
        return ReceiptWakeDecision::Ignore {
            reason: "not_own_request",
        };
    }

    ReceiptWakeDecision::Wake {
        request_id: parsed.request_id,
    }
}

/// Fetch a kind-40009 payment request by event id (kinds filter required).
pub async fn fetch_payment_request(
    rest: &RestClient,
    request_id_hex: &str,
) -> Result<Option<Event>, RelayError> {
    let event_id = EventId::from_hex(request_id_hex)
        .map_err(|e| RelayError::Http(format!("invalid request id: {e}")))?;
    let filter = Filter::new()
        .id(event_id)
        .kind(Kind::Custom(KIND_PAYMENT_REQUEST as u16))
        .limit(1);
    let value = rest.query(&[filter]).await?;
    let Some(arr) = value.as_array() else {
        return Err(RelayError::Http(
            "expected JSON array from /query (payment request)".into(),
        ));
    };
    let Some(raw) = arr.first() else {
        return Ok(None);
    };
    match serde_json::from_value::<Event>(raw.clone()) {
        Ok(event) => Ok(Some(event)),
        Err(e) => {
            tracing::debug!("payment request JSON parse failed: {e}");
            Ok(None)
        }
    }
}

/// Bounded two-generation seen-set for idempotent receipt wakes.
///
/// Same rotation strategy as the relay layer's event-id dedupe: when `current`
/// reaches `limit / 2`, it rotates into `previous`. At most one wake per
/// receipt event id while the id remains remembered.
#[derive(Debug, Default)]
pub struct ReceiptWakeDedupe {
    current: HashSet<String>,
    previous: HashSet<String>,
    limit: usize,
}

impl ReceiptWakeDedupe {
    /// Create a dedupe set that remembers up to `limit` recent receipt ids.
    pub fn new(limit: usize) -> Self {
        Self {
            current: HashSet::new(),
            previous: HashSet::new(),
            limit: limit.max(2),
        }
    }

    /// Record `receipt_id`. Returns `true` if this is the first time (wake).
    pub fn insert_if_new(&mut self, receipt_id: String) -> bool {
        if self.current.contains(&receipt_id) || self.previous.contains(&receipt_id) {
            return false;
        }
        self.current.insert(receipt_id);
        if self.current.len() >= self.limit / 2 {
            self.previous = std::mem::take(&mut self.current);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_sdk::builders::{build_payment_receipt, build_payment_request};
    use nostr::{EventBuilder, Keys, Tag};

    fn sign(builder: EventBuilder, keys: &Keys) -> Event {
        builder.sign_with_keys(keys).expect("sign")
    }

    fn agent_keys() -> Keys {
        Keys::generate()
    }

    fn own_request(agent: &Keys, channel: Uuid) -> Event {
        let builder = build_payment_request(
            channel,
            50_000,
            &agent.public_key().to_hex(),
            Some("lnbc1opaque"),
            None,
            Some("lunch"),
            None,
        )
        .expect("build request");
        sign(builder, agent)
    }

    fn receipt_for(request: &Event, channel: Uuid, payer: &Keys) -> Event {
        let hash = "a".repeat(64);
        let preimage = "b".repeat(64);
        let builder =
            build_payment_receipt(channel, request.id, &hash, &preimage, 50_000).expect("receipt");
        sign(builder, payer)
    }

    #[test]
    fn compose_base_prompt_appends_wallet_section_iff_provisioned() {
        let base = "You are an agent.\n";
        let without = compose_base_prompt(base, false);
        assert_eq!(without, base);
        assert!(!without.contains("Lightning Wallet"));

        let with = compose_base_prompt(base, true);
        assert!(with.contains("You are an agent."));
        assert!(with.contains("## Lightning Wallet"));
        assert!(with.contains("buzz wallet request"));
        assert!(with.contains("buzz wallet check"));
        assert!(with.contains("decorative hints"));
    }

    #[test]
    fn compose_heartbeat_prompt_nudges_only_when_wallet_provisioned() {
        let hb = "[System: Heartbeat]\ncheck feed\n";
        assert_eq!(compose_heartbeat_prompt(hb, false), hb);
        let with = compose_heartbeat_prompt(hb, true);
        assert!(with.contains("check feed"));
        assert!(with.contains("buzz wallet check"));
        assert!(with.contains("never settlement truth"));
    }

    #[test]
    fn own_request_receipt_wakes_once() {
        let agent = agent_keys();
        let payer = Keys::generate();
        let channel = Uuid::new_v4();
        let request = own_request(&agent, channel);
        let receipt = receipt_for(&request, channel, &payer);

        let decision = decide_receipt_wake(&receipt, Some(&request), &agent.public_key());
        assert_eq!(
            decision,
            ReceiptWakeDecision::Wake {
                request_id: request.id.to_hex(),
            }
        );

        let instruction = receipt_wake_instruction(&request.id.to_hex(), &channel);
        assert!(instruction.contains("proves NOTHING"));
        assert!(instruction.contains(&format!(
            "buzz wallet check --request {} --channel {}",
            request.id.to_hex(),
            channel
        )));
    }

    #[test]
    fn foreign_request_receipt_does_not_wake() {
        let agent = agent_keys();
        let other = Keys::generate();
        let payer = Keys::generate();
        let channel = Uuid::new_v4();
        let request = own_request(&other, channel);
        let receipt = receipt_for(&request, channel, &payer);

        assert_eq!(
            decide_receipt_wake(&receipt, Some(&request), &agent.public_key()),
            ReceiptWakeDecision::Ignore {
                reason: "not_own_request",
            }
        );
    }

    #[test]
    fn missing_request_does_not_wake() {
        let agent = agent_keys();
        let payer = Keys::generate();
        let channel = Uuid::new_v4();
        let request = own_request(&agent, channel);
        let receipt = receipt_for(&request, channel, &payer);

        assert_eq!(
            decide_receipt_wake(&receipt, None, &agent.public_key()),
            ReceiptWakeDecision::Ignore {
                reason: "request_not_found",
            }
        );
    }

    #[test]
    fn malformed_receipt_missing_e_does_not_wake() {
        let agent = agent_keys();
        let payer = Keys::generate();
        let channel = Uuid::new_v4();
        // Build a 40010-shaped event without a bare e tag.
        let builder = EventBuilder::new(Kind::Custom(KIND_PAYMENT_RECEIPT as u16), "").tags([
            Tag::parse(["h", &channel.to_string()]).expect("h"),
            Tag::parse(["payment_hash", &"a".repeat(64)]).expect("hash"),
            Tag::parse(["preimage", &"b".repeat(64)]).expect("preimage"),
            Tag::parse(["amount", "1000"]).expect("amount"),
        ]);
        let receipt = sign(builder, &payer);

        assert_eq!(
            decide_receipt_wake(&receipt, None, &agent.public_key()),
            ReceiptWakeDecision::Ignore {
                reason: "malformed_receipt",
            }
        );
    }

    #[test]
    fn receipt_wake_dedupe_is_idempotent() {
        let mut dedupe = ReceiptWakeDedupe::new(64);
        assert!(dedupe.insert_if_new("abc".into()));
        assert!(!dedupe.insert_if_new("abc".into()));
        assert!(dedupe.insert_if_new("def".into()));
    }

    #[test]
    fn env_wallet_provisioned_reads_nwc_uri() {
        // Isolate from ambient env: set then clear around the assertion.
        let prev = std::env::var("BUZZ_NWC_URI").ok();
        std::env::remove_var("BUZZ_NWC_URI");
        assert!(!env_wallet_provisioned());
        std::env::set_var("BUZZ_NWC_URI", "");
        assert!(!env_wallet_provisioned());
        std::env::set_var(
            "BUZZ_NWC_URI",
            "nostr+walletconnect://pk?relay=wss://r.example&secret=abcd",
        );
        assert!(env_wallet_provisioned());
        match prev {
            Some(v) => std::env::set_var("BUZZ_NWC_URI", v),
            None => std::env::remove_var("BUZZ_NWC_URI"),
        }
    }
}

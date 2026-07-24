//! NWC wallet daemon: publishes 13194, answers 23194 with NIP-04 23195, emits 23196.
//!
//! Encryption is **NIP-04** — verified against rust-nostr `nwc` / `nostr` 0.44
//! (`Request::to_event` / `Response::from_event` both call `nip04::encrypt` /
//! `nip04::decrypt`). Newer NIP-47 prefers NIP-44; nwc 0.44 has not migrated.

use crate::error::MockWalletError;
use crate::ledger::{InvoiceState, Ledger, PayError, PayOutcome};
use crate::mint::unix_now;
use crate::relay::{inject_event, Relay};
use futures_util::{SinkExt, StreamExt};
use nostr::nips::nip04;
use nostr::nips::nip47::{
    LookupInvoiceRequest, MakeInvoiceRequest, Method, PayInvoiceRequest, Request, RequestParams,
};
use nostr::{Event, EventBuilder, EventId, JsonUtil, Keys, Kind, PublicKey, Tag};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

type WsStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = futures_util::stream::SplitStream<WsStream>;

/// NIP-47 info kind (13194).
pub const KIND_INFO: Kind = Kind::Custom(13194);
/// NWC request kind (23194).
pub const KIND_REQUEST: Kind = Kind::Custom(23194);
/// NWC response kind (23195).
pub const KIND_RESPONSE: Kind = Kind::Custom(23195);
/// NWC notification kind (23196).
pub const KIND_NOTIFICATION: Kind = Kind::Custom(23196);

/// Supported method names for a full wallet.
const FULL_METHODS: &[&str] = &[
    "get_info",
    "get_balance",
    "make_invoice",
    "lookup_invoice",
    "pay_invoice",
];

/// Supported method names for receive-only mode (no pay variants).
const RECEIVE_ONLY_METHODS: &[&str] =
    &["get_info", "get_balance", "make_invoice", "lookup_invoice"];

/// Result of dispatching one NIP-47 method.
struct DispatchResult {
    response_json: String,
    /// When set, daemon emits a kind-23196 `payment_received` after the response.
    payment_received: Option<PayOutcome>,
}

/// Run the wallet daemon against an in-process relay URL.
pub async fn run_daemon(
    relay: Arc<Relay>,
    relay_ws_url: &str,
    wallet_keys: Keys,
    client_pubkey: PublicKey,
    ledger: Arc<Ledger>,
    receive_only: bool,
    cancel: CancellationToken,
) -> Result<(), MockWalletError> {
    let methods: &'static [&'static str] = if receive_only {
        RECEIVE_ONLY_METHODS
    } else {
        FULL_METHODS
    };

    let info_event = build_info_event(&wallet_keys, methods)?;
    inject_event(&relay, event_to_json(&info_event)?);

    let (mut sink, mut stream) = connect_ws(relay_ws_url).await?;
    publish_event(&mut sink, &info_event).await?;

    let wallet_pk = wallet_keys.public_key().to_hex();
    let sub_id = "nwc-req";
    let req = json!(["REQ", sub_id, { "kinds": [23194u64], "#p": [wallet_pk] }]);
    sink.send(Message::Text(req.to_string().into()))
        .await
        .map_err(|e| MockWalletError::Network(e.to_string()))?;

    info!(
        wallet = %wallet_keys.public_key(),
        receive_only,
        "mock NWC daemon listening for requests"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.map_err(|e| MockWalletError::Network(e.to_string()))?;
                let Message::Text(text) = msg else { continue };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                let Some(arr) = value.as_array() else { continue };
                if arr.first().and_then(|v| v.as_str()) != Some("EVENT") {
                    continue;
                }
                let Some(event_v) = arr.get(2) else { continue };
                let event: Event = match serde_json::from_value(event_v.clone()) {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("bad request event: {e}");
                        continue;
                    }
                };
                if event.kind != KIND_REQUEST {
                    continue;
                }
                if let Err(e) = handle_request(
                    &mut sink,
                    &relay,
                    &wallet_keys,
                    client_pubkey,
                    &ledger,
                    receive_only,
                    methods,
                    &event,
                )
                .await
                {
                    warn!("request handling failed: {e}");
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    sink: &mut WsSink,
    relay: &Relay,
    wallet_keys: &Keys,
    client_pubkey: PublicKey,
    ledger: &Ledger,
    receive_only: bool,
    methods: &[&str],
    event: &Event,
) -> Result<(), MockWalletError> {
    if event.pubkey != client_pubkey {
        debug!("ignoring request from unexpected pubkey {}", event.pubkey);
        return Ok(());
    }

    let script = ledger.script();
    if script.response_delay > Duration::ZERO {
        tokio::time::sleep(script.response_delay).await;
    }
    if script.swallow_requests {
        debug!("swallowing request {} (scripted)", event.id);
        return Ok(());
    }

    let plaintext = nip04::decrypt(wallet_keys.secret_key(), &event.pubkey, &event.content)
        .map_err(|e| MockWalletError::Nostr(format!("nip04 decrypt: {e}")))?;
    let request = Request::from_json(&plaintext)
        .map_err(|e| MockWalletError::Nostr(format!("nip47 request parse: {e}")))?;

    let DispatchResult {
        response_json,
        payment_received,
    } = dispatch(ledger, receive_only, methods, request)?;

    let response_event =
        build_encrypted_response(wallet_keys, &event.pubkey, event.id, &response_json)?;
    inject_event(relay, event_to_json(&response_event)?);
    publish_event(sink, &response_event).await?;

    if let Some(outcome) = payment_received {
        if outcome.settled_ours {
            let notif = build_payment_received(wallet_keys, &event.pubkey, &outcome)?;
            inject_event(relay, event_to_json(&notif)?);
            publish_event(sink, &notif).await?;
        }
    }

    Ok(())
}

fn dispatch(
    ledger: &Ledger,
    receive_only: bool,
    methods: &[&str],
    request: Request,
) -> Result<DispatchResult, MockWalletError> {
    let method = request.method;
    let method_str = method.as_str();
    info!(method = method_str, "nwc rpc");

    if method == Method::PayInvoice {
        if let Some(code) = ledger.take_fail_next_pay_code() {
            return Ok(DispatchResult {
                response_json: error_envelope(method_str, &code, "scripted pay failure"),
                payment_received: None,
            });
        }
        if receive_only || !methods.contains(&"pay_invoice") {
            return Ok(DispatchResult {
                response_json: error_envelope(
                    method_str,
                    "RESTRICTED",
                    "receive-only wallet: pay_invoice not allowed",
                ),
                payment_received: None,
            });
        }
    }

    match request.params {
        RequestParams::GetInfo => Ok(DispatchResult {
            response_json: ok_envelope(
                "get_info",
                json!({
                    "alias": "buzz-mock-wallet",
                    "methods": methods,
                    "notifications": ["payment_received"],
                }),
            ),
            payment_received: None,
        }),
        RequestParams::GetBalance => Ok(DispatchResult {
            response_json: ok_envelope("get_balance", json!({ "balance": ledger.balance_msat() })),
            payment_received: None,
        }),
        RequestParams::MakeInvoice(MakeInvoiceRequest {
            amount,
            description,
            expiry,
            ..
        }) => {
            let desc = description.unwrap_or_default();
            let expiry_secs = expiry.unwrap_or(3600);
            let minted = ledger.make_invoice(amount, &desc, expiry_secs)?;
            Ok(DispatchResult {
                response_json: ok_envelope(
                    "make_invoice",
                    json!({
                        "invoice": minted.bolt11,
                        "payment_hash": minted.payment_hash_hex,
                        "amount": minted.amount_msat,
                        "description": minted.description,
                        "created_at": minted.created_at,
                        "expires_at": minted.expires_at,
                    }),
                ),
                payment_received: None,
            })
        }
        RequestParams::LookupInvoice(LookupInvoiceRequest {
            payment_hash,
            invoice,
        }) => lookup_invoice(ledger, payment_hash, invoice),
        RequestParams::PayInvoice(PayInvoiceRequest { invoice, .. }) => {
            match ledger.pay_invoice(&invoice) {
                Ok(outcome) => Ok(DispatchResult {
                    response_json: ok_envelope(
                        "pay_invoice",
                        json!({ "preimage": outcome.preimage_hex }),
                    ),
                    payment_received: Some(outcome),
                }),
                Err(PayError::InsufficientBalance {
                    balance_msat,
                    needed_msat,
                }) => Ok(DispatchResult {
                    response_json: error_envelope(
                        "pay_invoice",
                        "INSUFFICIENT_BALANCE",
                        &format!("balance {balance_msat} msat < needed {needed_msat} msat"),
                    ),
                    payment_received: None,
                }),
                Err(PayError::Other(msg)) => Ok(DispatchResult {
                    response_json: error_envelope("pay_invoice", "PAYMENT_FAILED", &msg),
                    payment_received: None,
                }),
            }
        }
        other => {
            let name = match &other {
                RequestParams::PayKeysend(_) => "pay_keysend",
                RequestParams::MultiPayInvoice(_) => "multi_pay_invoice",
                RequestParams::MultiPayKeysend(_) => "multi_pay_keysend",
                _ => method_str,
            };
            let restricted = receive_only
                && (name == "pay_keysend"
                    || name == "multi_pay_invoice"
                    || name == "multi_pay_keysend"
                    || name == "pay_invoice");
            if restricted {
                return Ok(DispatchResult {
                    response_json: error_envelope(
                        name,
                        "RESTRICTED",
                        "receive-only wallet: pay methods not allowed",
                    ),
                    payment_received: None,
                });
            }
            Ok(DispatchResult {
                response_json: error_envelope(
                    method_str,
                    "NOT_IMPLEMENTED",
                    &format!("method not implemented: {method_str}"),
                ),
                payment_received: None,
            })
        }
    }
}

fn lookup_invoice(
    ledger: &Ledger,
    payment_hash: Option<String>,
    invoice: Option<String>,
) -> Result<DispatchResult, MockWalletError> {
    let hash = match (payment_hash, invoice) {
        (Some(h), _) => h,
        (None, Some(bolt11)) => crate::mint::decode_bolt11(&bolt11)
            .map(|(_, h)| h)
            .map_err(|e| MockWalletError::Invoice(e.to_string()))?,
        (None, None) => {
            return Ok(DispatchResult {
                response_json: error_envelope(
                    "lookup_invoice",
                    "OTHER",
                    "payment_hash or invoice required",
                ),
                payment_received: None,
            });
        }
    };
    match ledger.lookup(&hash) {
        None => Ok(DispatchResult {
            response_json: error_envelope("lookup_invoice", "NOT_FOUND", "invoice not found"),
            payment_received: None,
        }),
        Some(inv) => {
            let state = match inv.state {
                InvoiceState::Pending => "pending",
                InvoiceState::Settled => "settled",
                InvoiceState::Expired => "expired",
            };
            let mut result = json!({
                "type": "incoming",
                "state": state,
                "invoice": inv.minted.bolt11,
                "description": inv.minted.description,
                "payment_hash": inv.minted.payment_hash_hex,
                "amount": inv.minted.amount_msat,
                "fees_paid": 0u64,
                "created_at": inv.minted.created_at,
                "expires_at": inv.minted.expires_at,
            });
            if inv.state == InvoiceState::Settled {
                result["preimage"] = json!(inv.minted.preimage_hex);
                result["settled_at"] = json!(inv.settled_at.unwrap_or_else(unix_now));
            }
            Ok(DispatchResult {
                response_json: ok_envelope("lookup_invoice", result),
                payment_received: None,
            })
        }
    }
}

fn build_info_event(keys: &Keys, methods: &[&str]) -> Result<Event, MockWalletError> {
    let content = methods.join(" ");
    // Omit `encryption` tag → NIP-47 defaults to nip04 (matches nwc 0.44).
    EventBuilder::new(KIND_INFO, content)
        .tag(Tag::custom(
            nostr::TagKind::custom("notifications"),
            ["payment_received"],
        ))
        .sign_with_keys(keys)
        .map_err(|e| MockWalletError::Nostr(e.to_string()))
}

fn build_encrypted_response(
    wallet_keys: &Keys,
    client_pubkey: &PublicKey,
    request_id: EventId,
    plaintext: &str,
) -> Result<Event, MockWalletError> {
    let encrypted = nip04::encrypt(wallet_keys.secret_key(), client_pubkey, plaintext)
        .map_err(|e| MockWalletError::Nostr(format!("nip04 encrypt: {e}")))?;
    EventBuilder::new(KIND_RESPONSE, encrypted)
        .tag(Tag::event(request_id))
        .tag(Tag::public_key(*client_pubkey))
        .sign_with_keys(wallet_keys)
        .map_err(|e| MockWalletError::Nostr(e.to_string()))
}

fn build_payment_received(
    wallet_keys: &Keys,
    client_pubkey: &PublicKey,
    outcome: &PayOutcome,
) -> Result<Event, MockWalletError> {
    let plaintext = json!({
        "notification_type": "payment_received",
        "notification": {
            "type": "incoming",
            "state": "settled",
            "invoice": outcome.bolt11,
            "description": outcome.description,
            "preimage": outcome.preimage_hex,
            "payment_hash": outcome.payment_hash_hex,
            "amount": outcome.amount_msat,
            "fees_paid": 0u64,
            "created_at": outcome.created_at,
            "expires_at": outcome.expires_at,
            "settled_at": outcome.settled_at,
        }
    })
    .to_string();
    let encrypted = nip04::encrypt(wallet_keys.secret_key(), client_pubkey, &plaintext)
        .map_err(|e| MockWalletError::Nostr(format!("nip04 encrypt: {e}")))?;
    EventBuilder::new(KIND_NOTIFICATION, encrypted)
        .tag(Tag::public_key(*client_pubkey))
        .sign_with_keys(wallet_keys)
        .map_err(|e| MockWalletError::Nostr(e.to_string()))
}

fn ok_envelope(result_type: &str, result: Value) -> String {
    json!({
        "result_type": result_type,
        "result": result,
    })
    .to_string()
}

fn error_envelope(result_type: &str, code: &str, message: &str) -> String {
    json!({
        "result_type": result_type,
        "error": {
            "code": code,
            "message": message,
        }
    })
    .to_string()
}

fn event_to_json(event: &Event) -> Result<Value, MockWalletError> {
    serde_json::to_value(event).map_err(MockWalletError::Json)
}

async fn connect_ws(url: &str) -> Result<(WsSink, WsRead), MockWalletError> {
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| MockWalletError::Network(e.to_string()))?;
    Ok(ws.split())
}

async fn publish_event(sink: &mut WsSink, event: &Event) -> Result<(), MockWalletError> {
    let msg = json!(["EVENT", event]);
    sink.send(Message::Text(msg.to_string().into()))
        .await
        .map_err(|e| MockWalletError::Network(e.to_string()))?;
    Ok(())
}

/// Method list advertised for the given mode (tests / info content).
pub fn advertised_methods(receive_only: bool) -> &'static [&'static str] {
    if receive_only {
        RECEIVE_ONLY_METHODS
    } else {
        FULL_METHODS
    }
}

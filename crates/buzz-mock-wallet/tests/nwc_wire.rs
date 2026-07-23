//! Hermetic NWC wire tests against the in-process mock wallet.
//!
//! Drives the daemon with a raw nostr 0.44 client (NIP-04 encrypted 23194/23195),
//! matching what `nwc` 0.44 sends — not via `buzz-wallet`.

use bitcoin::hashes::Hash;
use buzz_mock_wallet::{advertised_methods, MockWallet, MockWalletConfig, Script};
use futures_util::{SinkExt, StreamExt};
use nostr::nips::nip04;
use nostr::nips::nip47::{
    GetBalanceResponse, GetInfoResponse, LookupInvoiceRequest, MakeInvoiceRequest,
    MakeInvoiceResponse, NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse, Request,
    Response,
};
use nostr::Keys;
use nostr::{Event, Kind};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const KIND_INFO: u64 = 13194;
const KIND_REQUEST: Kind = Kind::Custom(23194);
const KIND_RESPONSE: Kind = Kind::Custom(23195);
const KIND_NOTIFICATION: Kind = Kind::Custom(23196);

type TestWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct WireClient {
    uri: NostrWalletConnectURI,
    client_keys: Keys,
    sink: futures_util::stream::SplitSink<TestWs, Message>,
    stream: futures_util::stream::SplitStream<TestWs>,
}

impl WireClient {
    async fn connect(nwc_uri: &str) -> Self {
        let uri = NostrWalletConnectURI::parse(nwc_uri).expect("valid nwc uri");
        let relay = uri.relays[0].to_string();
        // RelayUrl Display may not include ws:// — rebuild from mock.
        let relay_url = if relay.starts_with("ws") {
            relay
        } else {
            format!("ws://{relay}")
        };
        // Prefer the relay query from the raw URI for loopback.
        let relay_url = extract_relay(nwc_uri).unwrap_or(relay_url);
        let (ws, _) = tokio_tungstenite::connect_async(&relay_url)
            .await
            .expect("connect relay");
        let (sink, stream) = ws.split();
        let client_keys = Keys::new(uri.secret.clone());
        Self {
            uri,
            client_keys,
            sink,
            stream,
        }
    }

    async fn fetch_info(&mut self) -> Event {
        let sub = "info";
        let req = json!([
            "REQ",
            sub,
            {
                "kinds": [KIND_INFO],
                "authors": [self.uri.public_key.to_hex()],
            }
        ]);
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
        let event = self
            .wait_event(sub, Duration::from_secs(2))
            .await
            .expect("info event");
        let close = json!(["CLOSE", sub]);
        let _ = self
            .sink
            .send(Message::Text(close.to_string().into()))
            .await;
        event
    }

    async fn subscribe_notifications(&mut self, sub: &str) {
        let filter = json!({
            "kinds": [23196u64],
            "authors": [self.uri.public_key.to_hex()],
            "#p": [self.client_keys.public_key().to_hex()],
        });
        let req = json!(["REQ", sub, filter]);
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
        let _ = self.wait_raw(Duration::from_millis(200)).await;
    }

    async fn call(&mut self, request: Request) -> Response {
        let event = request
            .to_event(&self.uri)
            .expect("request to_event (nip04)");
        assert_eq!(event.kind, KIND_REQUEST);

        let sub = format!("resp-{}", &event.id.to_hex()[..8]);
        let filter = json!({
            "kinds": [23195u64],
            "authors": [self.uri.public_key.to_hex()],
            "#e": [event.id.to_hex()],
        });
        let req = json!(["REQ", sub, filter]);
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
        let _ = self.wait_raw(Duration::from_millis(100)).await;

        let msg = json!(["EVENT", event]);
        self.sink
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();

        let resp_event = self
            .wait_event(&sub, Duration::from_secs(3))
            .await
            .expect("response event");
        assert_eq!(resp_event.kind, KIND_RESPONSE);
        Response::from_event(&self.uri, &resp_event).expect("decrypt nip04 response")
    }

    async fn call_no_response(&mut self, request: Request, bound: Duration) -> bool {
        let event = request.to_event(&self.uri).expect("request to_event");
        let sub = "timeout-sub";
        let filter = json!({
            "kinds": [23195u64],
            "authors": [self.uri.public_key.to_hex()],
            "#e": [event.id.to_hex()],
        });
        let req = json!(["REQ", sub, filter]);
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
        let msg = json!(["EVENT", event]);
        self.sink
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();
        self.wait_event(sub, bound).await.is_none()
    }

    async fn wait_notification(&mut self, sub: &str, bound: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + bound;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let raw = timeout(remaining, self.stream.next()).await.ok()??;
            let Message::Text(text) = raw.ok()? else {
                continue;
            };
            let value: Value = serde_json::from_str(&text).ok()?;
            let arr = value.as_array()?;
            if arr.first().and_then(|v| v.as_str()) != Some("EVENT") {
                continue;
            }
            if arr.get(1).and_then(|v| v.as_str()) != Some(sub) {
                continue;
            }
            let event: Event = serde_json::from_value(arr.get(2)?.clone()).ok()?;
            if event.kind != KIND_NOTIFICATION {
                continue;
            }
            let plaintext =
                nip04::decrypt(self.client_keys.secret_key(), &event.pubkey, &event.content)
                    .ok()?;
            return serde_json::from_str(&plaintext).ok();
        }
        None
    }

    async fn wait_event(&mut self, sub: &str, bound: Duration) -> Option<Event> {
        let deadline = tokio::time::Instant::now() + bound;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let raw = timeout(remaining, self.stream.next()).await.ok()??;
            let Message::Text(text) = raw.ok()? else {
                continue;
            };
            let value: Value = serde_json::from_str(&text).ok()?;
            let arr = value.as_array()?;
            if arr.first().and_then(|v| v.as_str()) != Some("EVENT") {
                continue;
            }
            if arr.get(1).and_then(|v| v.as_str()) != Some(sub) {
                continue;
            }
            return serde_json::from_value(arr.get(2)?.clone()).ok();
        }
        None
    }

    async fn wait_raw(&mut self, bound: Duration) -> Option<String> {
        let msg = timeout(bound, self.stream.next()).await.ok()??;
        match msg.ok()? {
            Message::Text(t) => Some(t.to_string()),
            _ => None,
        }
    }
}

fn extract_relay(uri: &str) -> Option<String> {
    let q = uri.split('?').nth(1)?;
    for part in q.split('&') {
        let (k, v) = part.split_once('=')?;
        if k == "relay" {
            return Some(urlencoding_decode(v));
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn start_wallet(config: MockWalletConfig) -> MockWallet {
    MockWallet::start(config).await.expect("start mock wallet")
}

#[tokio::test]
async fn info_event_lists_configured_methods() {
    let wallet = start_wallet(MockWalletConfig::default()).await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let info = client.fetch_info().await;
    assert_eq!(info.kind, Kind::Custom(KIND_INFO as u16));
    let methods: Vec<&str> = info.content.split_whitespace().collect();
    for expected in advertised_methods(false) {
        assert!(
            methods.contains(expected),
            "missing method {expected} in {}",
            info.content
        );
    }
    assert!(methods.contains(&"pay_invoice"));
    wallet.shutdown();
}

#[tokio::test]
async fn receive_only_info_omits_pay_methods() {
    let wallet = start_wallet(MockWalletConfig {
        receive_only: true,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let info = client.fetch_info().await;
    let methods: Vec<&str> = info.content.split_whitespace().collect();
    assert!(!methods.iter().any(|m| m.contains("pay")));
    for expected in advertised_methods(true) {
        assert!(methods.contains(expected));
    }

    let resp = client.call(Request::get_info()).await;
    let info: GetInfoResponse = resp.to_get_info().expect("get_info ok");
    assert!(!info.methods.iter().any(|m| m.as_str().contains("pay")));
    wallet.shutdown();
}

#[tokio::test]
async fn make_invoice_real_bolt11_and_verify_after_settle() {
    let _ = tracing_subscriber::fmt::try_init();
    let wallet = start_wallet(MockWalletConfig {
        balance_msat: 5_000_000,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    client.subscribe_notifications("notif").await;

    let amount = 2_500u64;
    let resp = client
        .call(Request::make_invoice(MakeInvoiceRequest {
            amount,
            description: Some("wire-test".into()),
            description_hash: None,
            expiry: Some(600),
        }))
        .await;
    let made: MakeInvoiceResponse = resp.to_make_invoice().expect("make_invoice");
    let bolt11 = made.invoice;
    assert!(
        bolt11.starts_with("lnbc") || bolt11.starts_with("lntb") || bolt11.starts_with("lnbcrt")
    );

    let invoice: lightning_invoice::Bolt11Invoice = bolt11.parse().expect("decode bolt11");
    assert_eq!(invoice.amount_milli_satoshis(), Some(amount));
    let payment_hash = hex::encode(invoice.payment_hash().to_byte_array());
    assert_eq!(made.payment_hash.as_deref(), Some(payment_hash.as_str()));

    let pay = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: bolt11.clone(),
            amount: None,
        }))
        .await;
    let paid: PayInvoiceResponse = pay.to_pay_invoice().expect("pay_invoice");
    assert!(
        buzz_core::payment::verify(&paid.preimage, &payment_hash),
        "preimage/hash must pass buzz_core::payment::verify"
    );

    let notif = client
        .wait_notification("notif", Duration::from_secs(2))
        .await
        .expect("23196 payment_received");
    assert_eq!(
        notif.get("notification_type").and_then(|v| v.as_str()),
        Some("payment_received")
    );

    let lookup = client
        .call(Request::lookup_invoice(LookupInvoiceRequest {
            payment_hash: Some(payment_hash),
            invoice: None,
        }))
        .await;
    let looked = lookup.to_lookup_invoice().expect("lookup settled");
    assert_eq!(
        looked.state,
        Some(nostr::nips::nip47::TransactionState::Settled)
    );
    assert_eq!(looked.preimage.as_deref(), Some(paid.preimage.as_str()));

    tracing::info!(
        uri = wallet.uri(),
        %bolt11,
        preimage = %paid.preimage,
        "live make_invoice/pay handshake ok"
    );
    wallet.shutdown();
}

#[tokio::test]
async fn pay_unknown_invoice_decrements_balance() {
    let wallet = start_wallet(MockWalletConfig {
        balance_msat: 100_000,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;

    // Mint via a second ephemeral mint path: ask the wallet for an invoice, then
    // we'll pay a *copy* — actually for unknown we need a foreign bolt11.
    // Build one with a different signing key in-test.
    let foreign = mint_foreign(1_000);
    let before = wallet.balance_msat();
    let pay = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: foreign,
            amount: None,
        }))
        .await;
    let _ = pay.to_pay_invoice().expect("synthetic pay ok");
    assert_eq!(wallet.balance_msat(), before - 1_000);
    wallet.shutdown();
}

#[tokio::test]
async fn insufficient_balance_returns_nip47_error() {
    let wallet = start_wallet(MockWalletConfig {
        balance_msat: 100,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let foreign = mint_foreign(50_000);
    let resp = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: foreign,
            amount: None,
        }))
        .await;
    let err = resp.to_pay_invoice().expect_err("should fail");
    match err {
        nostr::nips::nip47::Error::ErrorCode(e) => {
            assert_eq!(e.code, nostr::nips::nip47::ErrorCode::InsufficientBalance);
        }
        other => panic!("unexpected error: {other}"),
    }
    wallet.shutdown();
}

#[tokio::test]
async fn scripted_error_code_honored() {
    let wallet = start_wallet(MockWalletConfig {
        balance_msat: 1_000_000,
        script: Script {
            fail_next_pay_code: Some("PAYMENT_FAILED".into()),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let foreign = mint_foreign(100);
    let resp = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: foreign,
            amount: None,
        }))
        .await;
    let err = resp.to_pay_invoice().expect_err("scripted fail");
    match err {
        nostr::nips::nip47::Error::ErrorCode(e) => {
            assert_eq!(e.code, nostr::nips::nip47::ErrorCode::PaymentFailed);
        }
        other => panic!("unexpected: {other}"),
    }
    wallet.shutdown();
}

#[tokio::test]
async fn scripted_no_response_times_out() {
    let wallet = start_wallet(MockWalletConfig {
        script: Script {
            swallow_requests: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let timed_out = client
        .call_no_response(Request::get_balance(), Duration::from_millis(400))
        .await;
    assert!(timed_out, "client should observe no 23195 within bound");
    wallet.shutdown();
}

#[tokio::test]
async fn lookup_pending_settled_and_unknown() {
    let wallet = start_wallet(MockWalletConfig::default()).await;
    let mut client = WireClient::connect(wallet.uri()).await;

    let made = client
        .call(Request::make_invoice(MakeInvoiceRequest {
            amount: 777,
            description: Some("lookup".into()),
            description_hash: None,
            expiry: Some(3600),
        }))
        .await
        .to_make_invoice()
        .unwrap();
    let hash = made.payment_hash.expect("hash");

    let pending = client
        .call(Request::lookup_invoice(LookupInvoiceRequest {
            payment_hash: Some(hash.clone()),
            invoice: None,
        }))
        .await
        .to_lookup_invoice()
        .unwrap();
    assert_eq!(
        pending.state,
        Some(nostr::nips::nip47::TransactionState::Pending)
    );

    let _ = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: made.invoice,
            amount: None,
        }))
        .await
        .to_pay_invoice()
        .unwrap();

    let settled = client
        .call(Request::lookup_invoice(LookupInvoiceRequest {
            payment_hash: Some(hash),
            invoice: None,
        }))
        .await
        .to_lookup_invoice()
        .unwrap();
    assert_eq!(
        settled.state,
        Some(nostr::nips::nip47::TransactionState::Settled)
    );

    let missing = client
        .call(Request::lookup_invoice(LookupInvoiceRequest {
            payment_hash: Some("aa".repeat(32)),
            invoice: None,
        }))
        .await;
    match missing.to_lookup_invoice() {
        Err(nostr::nips::nip47::Error::ErrorCode(e)) => {
            assert_eq!(e.code, nostr::nips::nip47::ErrorCode::NotFound);
        }
        other => panic!("expected NOT_FOUND, got {other:?}"),
    }
    wallet.shutdown();
}

#[tokio::test]
async fn receive_only_pay_invoice_restricted() {
    let wallet = start_wallet(MockWalletConfig {
        receive_only: true,
        balance_msat: 1_000_000,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let foreign = mint_foreign(100);
    let resp = client
        .call(Request::pay_invoice(PayInvoiceRequest {
            id: None,
            invoice: foreign,
            amount: None,
        }))
        .await;
    match resp.to_pay_invoice() {
        Err(nostr::nips::nip47::Error::ErrorCode(e)) => {
            assert_eq!(e.code, nostr::nips::nip47::ErrorCode::Restricted);
        }
        other => panic!("expected RESTRICTED, got {other:?}"),
    }
    wallet.shutdown();
}

#[tokio::test]
async fn get_balance_matches_ledger() {
    let wallet = start_wallet(MockWalletConfig {
        balance_msat: 42_000,
        ..Default::default()
    })
    .await;
    let mut client = WireClient::connect(wallet.uri()).await;
    let bal: GetBalanceResponse = client
        .call(Request::get_balance())
        .await
        .to_get_balance()
        .unwrap();
    assert_eq!(bal.balance, 42_000);
    wallet.shutdown();
}

fn mint_foreign(amount_msat: u64) -> String {
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let preimage = [0xAB; 32];
    let payment_hash = bitcoin::hashes::sha256::Hash::hash(&preimage);
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let builder = InvoiceBuilder::new(Currency::Bitcoin)
        .description("foreign".into())
        .payment_hash(payment_hash)
        .payment_secret(PaymentSecret([0x11; 32]))
        .duration_since_epoch(Duration::from_secs(created))
        .min_final_cltv_expiry_delta(144)
        .expiry_time(Duration::from_secs(3600))
        .amount_milli_satoshis(amount_msat);
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[0x02; 32]).unwrap();
    builder
        .build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &sk))
        .unwrap()
        .to_string()
}

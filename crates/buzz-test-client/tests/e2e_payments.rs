//! End-to-end tests for payment-kind ingest (kinds 40009 / 40010).
//!
//! Contract: `docs/lightning-wallet-nwc.md` → Feature: Relay ingest of payment kinds.
//! The relay is a dumb transport — h-scoped fan-out and messaging rate limits only.
//!
//! These tests require a running relay (+ Postgres/Redis). Marked `#[ignore]` so
//! `cargo test` does not fail when infrastructure is absent.
//!
//! # Running
//!
//! ```text
//! cargo test -p buzz-test-client --test e2e_payments -- --ignored --nocapture
//! ```
//!
//! Override the shared relay with `RELAY_URL` (default `ws://localhost:3000`).
//! Scenario 3 boots its own relay on port 3100 with a low message quota.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use buzz_core::kind::{KIND_PAYMENT_RECEIPT, KIND_PAYMENT_REQUEST};
use buzz_sdk::builders::{build_payment_receipt, build_payment_request};
use buzz_test_client::{BuzzTestClient, RelayMessage};
use nostr::{Alphabet, Event, EventBuilder, EventId, Filter, Keys, Kind, SingleLetterTag, Tag};
use uuid::Uuid;

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_string())
}

fn sub_id(name: &str) -> String {
    format!("pay-e2e-{name}-{}", Uuid::new_v4())
}

fn host_from_relay_url(url: &str) -> String {
    url.trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .trim_end_matches('/')
        .to_string()
}

async fn e2e_db_pool() -> sqlx::Pool<sqlx::Postgres> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string());
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to e2e Postgres")
}

async fn ensure_test_community(host: &str) -> Uuid {
    let pool = e2e_db_pool().await;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO communities (id, host) \
         VALUES ($1, $2) \
         ON CONFLICT (lower(host)) DO NOTHING",
    )
    .bind(id)
    .bind(host)
    .execute(&pool)
    .await
    .unwrap_or_else(|e| panic!("seed community {host}: {e}"));

    sqlx::query_scalar("SELECT id FROM communities WHERE lower(host) = lower($1)")
        .bind(host)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("lookup community {host}: {e}"))
}

/// Create a private channel over WebSocket and return the channel UUID string.
async fn create_private_channel_ws(client: &mut BuzzTestClient, keys: &Keys) -> String {
    let channel_uuid = Uuid::new_v4().to_string();
    let channel_name = format!("pay-e2e-private-{}", channel_uuid);

    let event = EventBuilder::new(Kind::Custom(9007), "")
        .tags(vec![
            Tag::parse(["h", &channel_uuid]).expect("h tag"),
            Tag::parse(["name", &channel_name]).expect("name tag"),
            Tag::parse(["channel_type", "stream"]).expect("channel_type tag"),
            Tag::parse(["visibility", "private"]).expect("visibility tag"),
        ])
        .sign_with_keys(keys)
        .expect("sign create-channel");

    let ok = client
        .send_event(event)
        .await
        .expect("create private channel");
    assert!(
        ok.accepted,
        "private channel creation failed: {}",
        ok.message
    );
    channel_uuid
}

/// Submit a kind:9000 PUT_USER invite over WebSocket.
async fn add_member_ws(
    client: &mut BuzzTestClient,
    channel_id: &str,
    target_pubkey_hex: &str,
    signer: &Keys,
) {
    let event = EventBuilder::new(Kind::Custom(9000), "")
        .tags([
            Tag::parse(["h", channel_id]).expect("h tag"),
            Tag::parse(["p", target_pubkey_hex]).expect("p tag"),
        ])
        .sign_with_keys(signer)
        .expect("sign PUT_USER");

    let ok = client.send_event(event).await.expect("send PUT_USER");
    assert!(ok.accepted, "PUT_USER rejected: {}", ok.message);
}

fn channel_kind_filter(channel_id: &str, kind: u16) -> Filter {
    Filter::new()
        .kind(Kind::Custom(kind))
        .custom_tags(SingleLetterTag::lowercase(Alphabet::H), [channel_id])
}

/// Drain until CLOSED for `sid`, matching the suite's private-scope assertions.
async fn expect_subscribe_closed(client: &mut BuzzTestClient, sid: &str) {
    loop {
        let msg = client
            .recv_event(Duration::from_secs(5))
            .await
            .expect("recv CLOSED (or timeout)");
        match msg {
            RelayMessage::Closed {
                subscription_id,
                message,
            } => {
                assert_eq!(
                    subscription_id, sid,
                    "CLOSED for wrong subscription: {subscription_id}"
                );
                assert!(
                    message.to_lowercase().contains("restricted"),
                    "expected 'restricted' in CLOSED message, got: {message}"
                );
                return;
            }
            RelayMessage::Event { .. } | RelayMessage::Eose { .. } => {
                panic!("non-member subscription must be rejected at subscribe time, got {msg:?}");
            }
            _ => {}
        }
    }
}

fn event_tag_rows(event: &Event) -> Vec<Vec<String>> {
    event
        .tags
        .iter()
        .map(|t| t.as_slice().iter().map(|s| s.to_string()).collect())
        .collect()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn relay_binary() -> PathBuf {
    let root = workspace_root();
    let debug = root.join("target/debug/buzz-relay");
    if debug.exists() {
        return debug;
    }
    let release = root.join("target/release/buzz-relay");
    if release.exists() {
        return release;
    }
    panic!(
        "buzz-relay binary not found at {} or {}; run `cargo build -p buzz-relay` first",
        debug.display(),
        release.display()
    );
}

/// Dedicated relay child process; always killed on drop.
struct DedicatedRelay {
    child: Child,
    ws_url: String,
    http_url: String,
}

impl DedicatedRelay {
    fn spawn(port: u16, messages_per_min: u64) -> Self {
        let bind = format!("0.0.0.0:{port}");
        let ws_url = format!("ws://localhost:{port}");
        let http_url = format!("http://localhost:{port}");
        let health_port = port + 80;
        let metrics_port = port + 100;

        let mut cmd = Command::new(relay_binary());
        cmd.current_dir(workspace_root())
            .env("BUZZ_BIND_ADDR", &bind)
            .env("RELAY_URL", &ws_url)
            .env("BUZZ_HEALTH_PORT", health_port.to_string())
            .env("BUZZ_METRICS_PORT", metrics_port.to_string())
            .env(
                "BUZZ_RATE_LIMIT_HUMAN_MESSAGES_PER_MIN",
                messages_per_min.to_string(),
            )
            // Keep the per-second WS budget clear of the message-tier assertion.
            .env("BUZZ_RATE_LIMIT_HUMAN_WS_EVENTS_PER_SEC", "100")
            .env(
                "DATABASE_URL",
                std::env::var("DATABASE_URL")
                    .unwrap_or_else(|_| "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string()),
            )
            .env(
                "REDIS_URL",
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("spawn dedicated buzz-relay on {port}: {e}"));

        Self {
            child,
            ws_url,
            http_url,
        }
    }

    async fn wait_ready(&self) {
        let client = reqwest::Client::new();
        let health = format!("{}/health", self.http_url);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "dedicated relay at {} not healthy within 60s",
                    self.http_url
                );
            }
            match client.get(&health).send().await {
                Ok(resp) if resp.status().is_success() => return,
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}

impl Drop for DedicatedRelay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drain until a NOTICE containing the standard rate-limit text (not an OK).
async fn expect_rate_limit_notice(client: &mut BuzzTestClient) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        assert!(
            !remaining.is_zero(),
            "timed out waiting for rate-limit NOTICE"
        );
        match client
            .recv_event(remaining)
            .await
            .expect("recv rate-limit response")
        {
            RelayMessage::Notice { message } => {
                assert!(
                    message.contains("rate-limited: quota exceeded"),
                    "expected rate-limit NOTICE text, got: {message}"
                );
                return;
            }
            RelayMessage::Ok(ok) => {
                panic!(
                    "rate-limit rejection must be a NOTICE, not OK (accepted={}, message={})",
                    ok.accepted, ok.message
                );
            }
            _ => {}
        }
    }
}

/// Scenario: Requests are h-scoped
///
/// Private channel C with members A,B; outsider E's C-scoped REQ is CLOSED;
/// A's C-scoped sub receives B's KIND_PAYMENT_REQUEST.
#[tokio::test]
#[ignore]
async fn test_payment_request_h_scoped_private_channel() {
    let url = relay_url();
    let host = host_from_relay_url(&url);
    let _ = ensure_test_community(&host).await;

    let keys_a = Keys::generate();
    let keys_b = Keys::generate();
    let keys_e = Keys::generate();

    let mut client_a = BuzzTestClient::connect(&url, &keys_a)
        .await
        .expect("connect A");
    let channel_id = create_private_channel_ws(&mut client_a, &keys_a).await;
    add_member_ws(
        &mut client_a,
        &channel_id,
        &keys_b.public_key().to_hex(),
        &keys_a,
    )
    .await;

    let mut client_b = BuzzTestClient::connect(&url, &keys_b)
        .await
        .expect("connect B");
    let mut client_e = BuzzTestClient::connect(&url, &keys_e)
        .await
        .expect("connect E");

    let sid_a = sub_id("member");
    client_a
        .subscribe(
            &sid_a,
            vec![channel_kind_filter(
                &channel_id,
                KIND_PAYMENT_REQUEST as u16,
            )],
        )
        .await
        .expect("A subscribe");
    client_a
        .collect_until_eose(&sid_a, Duration::from_secs(5))
        .await
        .expect("A EOSE");

    let sid_e = sub_id("outsider");
    client_e
        .subscribe(
            &sid_e,
            vec![channel_kind_filter(
                &channel_id,
                KIND_PAYMENT_REQUEST as u16,
            )],
        )
        .await
        .expect("E subscribe");
    expect_subscribe_closed(&mut client_e, &sid_e).await;

    let channel_uuid = Uuid::parse_str(&channel_id).expect("channel uuid");
    let request = build_payment_request(
        channel_uuid,
        500_000,
        &keys_b.public_key().to_hex(),
        None,
        Some("bob@wallet.example"),
        Some("lunch"),
        None,
    )
    .expect("build_payment_request")
    .sign_with_keys(&keys_b)
    .expect("sign payment request");

    let ok = client_b
        .send_event(request.clone())
        .await
        .expect("B publish payment request");
    assert!(ok.accepted, "payment request rejected: {}", ok.message);

    let msg = client_a
        .recv_event(Duration::from_secs(5))
        .await
        .expect("A recv payment request");
    match msg {
        RelayMessage::Event {
            subscription_id,
            event,
        } => {
            assert_eq!(subscription_id, sid_a);
            assert_eq!(event.id, request.id);
            assert_eq!(event.kind, Kind::Custom(KIND_PAYMENT_REQUEST as u16));
            assert_eq!(event_tag_rows(&event), event_tag_rows(&request));
        }
        other => panic!("expected EVENT for A, got {other:?}"),
    }

    client_a.disconnect().await.expect("disconnect A");
    client_b.disconnect().await.expect("disconnect B");
    client_e.disconnect().await.expect("disconnect E");
}

/// Scenario: The relay applies no payment logic
///
/// A receipt with preimage that does not match payment_hash is accepted,
/// stored, and fanned out with tags identical to the published event.
#[tokio::test]
#[ignore]
async fn test_payment_receipt_relay_applies_no_payment_logic() {
    let url = relay_url();
    let host = host_from_relay_url(&url);
    let _ = ensure_test_community(&host).await;

    let keys_a = Keys::generate();
    let keys_b = Keys::generate();

    let mut client_a = BuzzTestClient::connect(&url, &keys_a)
        .await
        .expect("connect A");
    let channel_id = create_private_channel_ws(&mut client_a, &keys_a).await;
    add_member_ws(
        &mut client_a,
        &channel_id,
        &keys_b.public_key().to_hex(),
        &keys_a,
    )
    .await;

    let sid_a = sub_id("receipt");
    client_a
        .subscribe(
            &sid_a,
            vec![channel_kind_filter(
                &channel_id,
                KIND_PAYMENT_RECEIPT as u16,
            )],
        )
        .await
        .expect("A subscribe");
    client_a
        .collect_until_eose(&sid_a, Duration::from_secs(5))
        .await
        .expect("A EOSE");

    let mut client_b = BuzzTestClient::connect(&url, &keys_b)
        .await
        .expect("connect B");

    // Deliberately forge: SHA256(preimage) != payment_hash. Relay must not check.
    let payment_hash = "aa".repeat(32);
    let preimage = "bb".repeat(32);
    let fake_request_id = EventId::from_hex(&"cc".repeat(32)).expect("fake request event id");
    let channel_uuid = Uuid::parse_str(&channel_id).expect("channel uuid");

    let receipt = build_payment_receipt(
        channel_uuid,
        fake_request_id,
        &payment_hash,
        &preimage,
        500_000,
    )
    .expect("build_payment_receipt")
    .sign_with_keys(&keys_b)
    .expect("sign forged receipt");

    let published_tags = event_tag_rows(&receipt);

    let ok = client_b
        .send_event(receipt.clone())
        .await
        .expect("B publish forged receipt");
    assert!(
        ok.accepted,
        "relay must accept unverified receipt, got: {}",
        ok.message
    );

    let msg = client_a
        .recv_event(Duration::from_secs(5))
        .await
        .expect("A recv forged receipt");
    match msg {
        RelayMessage::Event {
            subscription_id,
            event,
        } => {
            assert_eq!(subscription_id, sid_a);
            assert_eq!(event.id, receipt.id);
            assert_eq!(event.kind, Kind::Custom(KIND_PAYMENT_RECEIPT as u16));
            assert_eq!(
                event_tag_rows(&event),
                published_tags,
                "relay must fan out receipt tags unchanged"
            );
        }
        other => panic!("expected EVENT for A, got {other:?}"),
    }

    client_a.disconnect().await.expect("disconnect A");
    client_b.disconnect().await.expect("disconnect B");
}

/// Scenario: Requests are rate-limited like other messaging kinds
///
/// Boots a dedicated relay with a low human message quota. After N accepted
/// KIND_PAYMENT_REQUEST publishes, N+1 is rejected with a NOTICE (not OK).
#[tokio::test]
#[ignore]
async fn test_payment_request_rate_limited_like_messaging_kinds() {
    const PORT: u16 = 3100;
    const LIMIT: u64 = 5;

    let relay = DedicatedRelay::spawn(PORT, LIMIT);
    relay.wait_ready().await;

    let host = host_from_relay_url(&relay.ws_url);
    let _ = ensure_test_community(&host).await;

    let owner = Keys::generate();
    let publisher = Keys::generate();

    // Setup uses the owner so the publisher's message quota starts unused.
    let mut owner_client = BuzzTestClient::connect(&relay.ws_url, &owner)
        .await
        .expect("connect owner on dedicated relay");
    let channel_id = create_private_channel_ws(&mut owner_client, &owner).await;
    add_member_ws(
        &mut owner_client,
        &channel_id,
        &publisher.public_key().to_hex(),
        &owner,
    )
    .await;
    owner_client.disconnect().await.expect("disconnect owner");

    let mut pub_client = BuzzTestClient::connect(&relay.ws_url, &publisher)
        .await
        .expect("connect publisher");
    let channel_uuid = Uuid::parse_str(&channel_id).expect("channel uuid");
    let payee = publisher.public_key().to_hex();

    for i in 0..LIMIT {
        let request = build_payment_request(
            channel_uuid,
            1_000 + i,
            &payee,
            None,
            Some("rate@wallet.example"),
            Some(&format!("rate-limit-{i}")),
            None,
        )
        .expect("build_payment_request")
        .sign_with_keys(&publisher)
        .expect("sign payment request");

        let ok = pub_client
            .send_event(request)
            .await
            .unwrap_or_else(|e| panic!("publish #{i} within limit: {e}"));
        assert!(
            ok.accepted,
            "event #{i} within limit must be accepted, got: {}",
            ok.message
        );
    }

    let overflow = build_payment_request(
        channel_uuid,
        9_999,
        &payee,
        None,
        Some("rate@wallet.example"),
        Some("rate-limit-overflow"),
        None,
    )
    .expect("build overflow request")
    .sign_with_keys(&publisher)
    .expect("sign overflow request");

    // Rate-limit path emits NOTICE (no OK). send_event would time out waiting for OK.
    pub_client
        .send_raw(&serde_json::json!(["EVENT", &overflow]))
        .await
        .expect("send overflow EVENT");
    expect_rate_limit_notice(&mut pub_client).await;

    pub_client.disconnect().await.expect("disconnect publisher");
    // DedicatedRelay Drop kills the child.
}

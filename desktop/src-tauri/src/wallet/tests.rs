//! Fakes-based tests for the desktop wallet driver (U10).

use super::{
    nwc_blob_key, payments_path, AttemptKeyDto, BlobBackend, JsonPaymentStore,
    KeyringNwcSecretStore, MapBlobBackend, SendTargetDto, WalletPorts, WalletRuntime,
    WalletStatusView, NOT_LINKED,
};
use buzz_core_pkg::payment::Amount;
use buzz_wallet_pkg::fakes::{
    ConnectorScript, FakeClock, FakeLnurlResolver, FakeWalletConnector, FakeWalletService,
    InvoiceScript, PayScript, RecordingProfilePublisher,
};
use buzz_wallet_pkg::ports::{PaymentStore, SecretStore, WalletService};
use buzz_wallet_pkg::test_support::mint_bolt11_for_amount;
use buzz_wallet_pkg::{
    AttemptId, Capabilities, ClaimOutcome, InvoiceStatus, PersistedPaymentState, StoredSecret,
    WalletMethod, WalletTimeouts,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

const VALID_URI: &str = "nostr+walletconnect://pubkey?relay=wss://relay.example&secret=abc";
const CLOCK_NOW: u64 = 1_700_000_000;

fn timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(5),
        pay_response: Duration::from_secs(30),
    }
}

fn full_caps() -> Capabilities {
    Capabilities::from_methods([
        WalletMethod::PayInvoice,
        WalletMethod::MakeInvoice,
        WalletMethod::LookupInvoice,
        WalletMethod::GetBalance,
    ])
}

/// BlobBackend wrapper around `Arc<MapBlobBackend>` for shared inspection.
struct SharedBlob(Arc<MapBlobBackend>);

impl BlobBackend for SharedBlob {
    fn load(&self, key: &str) -> Result<Option<String>, String> {
        self.0.load(key)
    }
    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.store(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.delete(key)
    }
}

struct TestHarness {
    runtime: WalletRuntime,
    connector: Arc<FakeWalletConnector>,
    service: Arc<FakeWalletService>,
    blob: Arc<MapBlobBackend>,
    _tmp: TempDir,
}

fn queue_connect(
    connector: &FakeWalletConnector,
    service: &Arc<FakeWalletService>,
    lud16: Option<&str>,
) {
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(service) as Arc<dyn WalletService>,
        capabilities: full_caps(),
        lud16: lud16.map(str::to_string),
    });
}

fn harness(community_id: &str) -> TestHarness {
    let tmp = TempDir::new().expect("tempdir");
    let store_path = payments_path(tmp.path(), community_id);
    let store = Arc::new(JsonPaymentStore::open(&store_path).expect("open store"));
    let blob = Arc::new(MapBlobBackend::new());
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringNwcSecretStore::new(
        community_id.to_string(),
        SharedBlob(Arc::clone(&blob)),
    ));
    let service = Arc::new(FakeWalletService::new());
    let connector = Arc::new(FakeWalletConnector::new());
    queue_connect(&connector, &service, Some("alice@getalby.com"));
    let ports = WalletPorts {
        connector: Arc::clone(&connector) as Arc<dyn buzz_wallet_pkg::WalletConnector>,
        secrets,
        profiles: Arc::new(RecordingProfilePublisher::new())
            as Arc<dyn buzz_wallet_pkg::ProfilePublisher>,
        resolver: Arc::new(FakeLnurlResolver::new()) as Arc<dyn buzz_wallet_pkg::LnurlResolver>,
        store: store as Arc<dyn PaymentStore>,
        clock: Arc::new(FakeClock::new(CLOCK_NOW)) as Arc<dyn buzz_wallet_pkg::Clock>,
        timeouts: timeouts(),
    };
    let runtime = WalletRuntime::new(community_id.to_string(), ports);
    TestHarness {
        runtime,
        connector,
        service,
        blob,
        _tmp: tmp,
    }
}

fn runtime_over_store(
    community_id: &str,
    store: Arc<JsonPaymentStore>,
    service: Arc<FakeWalletService>,
    secrets: Arc<dyn SecretStore>,
    connector: Arc<FakeWalletConnector>,
) -> WalletRuntime {
    queue_connect(&connector, &service, None);
    let ports = WalletPorts {
        connector: connector as Arc<dyn buzz_wallet_pkg::WalletConnector>,
        secrets,
        profiles: Arc::new(RecordingProfilePublisher::new())
            as Arc<dyn buzz_wallet_pkg::ProfilePublisher>,
        resolver: Arc::new(FakeLnurlResolver::new()) as Arc<dyn buzz_wallet_pkg::LnurlResolver>,
        store: store as Arc<dyn PaymentStore>,
        clock: Arc::new(FakeClock::new(CLOCK_NOW)) as Arc<dyn buzz_wallet_pkg::Clock>,
        timeouts: timeouts(),
    };
    WalletRuntime::new(community_id.to_string(), ports)
}

#[tokio::test]
async fn link_stores_secret_at_nwc_community_key_and_status_reflects_capabilities() {
    let h = harness("community-a");
    let status = h.runtime.link(VALID_URI).await.expect("link");
    assert!(status.linked);
    assert!(status.capabilities.contains(&"pay_invoice".to_string()));
    assert!(status.capabilities.contains(&"make_invoice".to_string()));
    assert_eq!(status.lud16.as_deref(), Some("alice@getalby.com"));
    assert_eq!(status.receive_mode, "static_address");

    let key = nwc_blob_key("community-a");
    assert!(
        h.blob.keys().contains(&key),
        "expected secret at {key}, keys={:?}",
        h.blob.keys()
    );
    let raw = h.blob.get_raw(&key).expect("raw secret");
    assert!(raw.contains("pay_invoice"));
}

#[tokio::test]
async fn unlink_removes_secret_and_status_unlinked() {
    let h = harness("community-a");
    h.runtime.link(VALID_URI).await.expect("link");
    h.runtime.unlink().await.expect("unlink");
    assert!(!h.blob.keys().contains(&nwc_blob_key("community-a")));
    let status = h.runtime.status().await.expect("status");
    assert!(!status.linked);
    assert!(status.capabilities.is_empty());
}

/// After unlink, prepare_send must fail on stored-secret gate — not a stale service.
#[tokio::test]
async fn unlink_then_prepare_send_fails_not_linked() {
    let h = harness("community-unlink-prepare");
    h.runtime.link(VALID_URI).await.expect("link");
    h.runtime.unlink().await.expect("unlink");

    let minted = mint_bolt11_for_amount(3_000, 0x55, CLOCK_NOW);
    let err = h
        .runtime
        .prepare_send(
            SendTargetDto::Bolt11 {
                invoice: minted.bolt11.as_str().to_string(),
            },
            3_000,
            AttemptKeyDto::Standalone,
            None,
        )
        .await
        .expect_err("prepare after unlink");
    assert_eq!(err, NOT_LINKED);
}

/// After unlink, a previously prepared handle cannot confirm (registry cleared + no service).
#[tokio::test]
async fn unlink_then_confirm_prepared_handle_fails() {
    let h = harness("community-unlink-confirm");
    h.runtime.link(VALID_URI).await.expect("link");

    let minted = mint_bolt11_for_amount(4_000, 0x56, CLOCK_NOW);
    let quote = h
        .runtime
        .prepare_send(
            SendTargetDto::Bolt11 {
                invoice: minted.bolt11.as_str().to_string(),
            },
            4_000,
            AttemptKeyDto::Standalone,
            None,
        )
        .await
        .expect("prepare");

    h.runtime.unlink().await.expect("unlink");

    let err = h
        .runtime
        .confirm(&quote.handle_id)
        .await
        .expect_err("confirm after unlink");
    // Secret gate runs first; handle registry is also cleared.
    assert_eq!(err, NOT_LINKED);
}

/// Teardown must not brick relink — pay path works after link → unlink → link.
#[tokio::test]
async fn unlink_then_relink_pay_path_works() {
    let h = harness("community-relink");
    h.runtime.link(VALID_URI).await.expect("link");
    h.runtime.unlink().await.expect("unlink");

    // Fresh facade needs a fresh connect script for the second link.
    queue_connect(&h.connector, &h.service, Some("alice@getalby.com"));
    h.runtime.link(VALID_URI).await.expect("relink");

    let minted = mint_bolt11_for_amount(5_000, 0x57, CLOCK_NOW);
    h.service.script_pay(PayScript::Settle {
        preimage: minted.preimage_hex.clone(),
    });

    let quote = h
        .runtime
        .prepare_send(
            SendTargetDto::Bolt11 {
                invoice: minted.bolt11.as_str().to_string(),
            },
            5_000,
            AttemptKeyDto::Standalone,
            None,
        )
        .await
        .expect("prepare after relink");
    let outcome = h
        .runtime
        .confirm(&quote.handle_id)
        .await
        .expect("confirm after relink");
    assert!(matches!(outcome, super::SendConfirmOutcome::Settled { .. }));
}

/// Scenario: A community switch does not orphan a pending payment.
#[tokio::test]
async fn community_switch_does_not_orphan_pending_payment() {
    let tmp = TempDir::new().expect("tempdir");
    let path_a = payments_path(tmp.path(), "community-a");
    let path_b = payments_path(tmp.path(), "community-b");

    let store_a = Arc::new(JsonPaymentStore::open(&path_a).expect("store a"));
    let minted = mint_bolt11_for_amount(50_000, 0x11, CLOCK_NOW);
    let attempt = AttemptId::new("request:evt-pending");
    let claim = store_a
        .claim_paying(
            &attempt,
            &minted.payment_hash_hex,
            &minted.bolt11,
            Amount::from_msat(50_000),
            CLOCK_NOW + 3600,
        )
        .await
        .expect("claim");
    assert!(matches!(claim, ClaimOutcome::Claimed(_)));
    store_a
        .update_state(&attempt, PersistedPaymentState::Unknown, None)
        .await
        .expect("park unknown");

    assert!(path_a.exists(), "community A store must be on disk");

    // Switch to B: new runtime, different store file. A's file must survive.
    let secrets_b: Arc<dyn SecretStore> = Arc::new(KeyringNwcSecretStore::new(
        "community-b",
        MapBlobBackend::new(),
    ));
    let service_b = Arc::new(FakeWalletService::new());
    let connector_b = Arc::new(FakeWalletConnector::new());
    let store_b = Arc::new(JsonPaymentStore::open(&path_b).expect("store b"));
    let _runtime_b = runtime_over_store("community-b", store_b, service_b, secrets_b, connector_b);
    assert!(path_a.exists(), "A store must survive switch to B");

    // Switch back to A: rebuild runtime over surviving store + scripted lookup.
    let blob_a = Arc::new(MapBlobBackend::new());
    let secrets_a: Arc<dyn SecretStore> = Arc::new(KeyringNwcSecretStore::new(
        "community-a",
        SharedBlob(Arc::clone(&blob_a)),
    ));
    secrets_a
        .store(&StoredSecret {
            uri: VALID_URI.to_string(),
            capabilities: full_caps(),
            lud16: None,
        })
        .await
        .expect("store secret");

    let service_a = Arc::new(FakeWalletService::new());
    service_a.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Settled {
            preimage: Some(minted.preimage_hex.clone()),
        }),
    );
    let connector_a = Arc::new(FakeWalletConnector::new());
    let store_a2 = Arc::new(JsonPaymentStore::open(&path_a).expect("reopen a"));
    let pending = store_a2
        .list_by_states(&[PersistedPaymentState::Unknown])
        .await
        .expect("list");
    assert_eq!(pending.len(), 1, "pending record for A intact after switch");

    let runtime_a = runtime_over_store(
        "community-a",
        store_a2,
        Arc::clone(&service_a),
        secrets_a,
        connector_a,
    );
    runtime_a.reconcile().await.expect("reconcile");

    let settled = runtime_a
        .store()
        .get(&attempt)
        .await
        .expect("get")
        .expect("record");
    assert_eq!(settled.state, PersistedPaymentState::Settled);
    assert!(
        path_a.exists(),
        "A store file still present after reconcile"
    );
}

#[tokio::test]
async fn claim_paying_atomicity_double_claim_one_record() {
    let tmp = TempDir::new().expect("tempdir");
    let path = payments_path(tmp.path(), "c1");
    let store = JsonPaymentStore::open(&path).expect("open");
    let minted = mint_bolt11_for_amount(1_000, 0x22, CLOCK_NOW);
    let attempt = AttemptId::new("standalone:dup");
    let first = store
        .claim_paying(
            &attempt,
            &minted.payment_hash_hex,
            &minted.bolt11,
            Amount::from_msat(1_000),
            CLOCK_NOW + 600,
        )
        .await
        .expect("first");
    let second = store
        .claim_paying(
            &attempt,
            &minted.payment_hash_hex,
            &minted.bolt11,
            Amount::from_msat(1_000),
            CLOCK_NOW + 600,
        )
        .await
        .expect("second");
    assert!(matches!(first, ClaimOutcome::Claimed(_)));
    assert!(matches!(second, ClaimOutcome::AlreadyClaimed(_)));
    let all = store
        .list_by_states(&[PersistedPaymentState::Paying])
        .await
        .expect("list");
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn durable_store_survives_restart_including_expires_at() {
    let tmp = TempDir::new().expect("tempdir");
    let path = payments_path(tmp.path(), "c-restart");
    let expires = CLOCK_NOW + 9_999;
    {
        let store = JsonPaymentStore::open(&path).expect("open");
        let minted = mint_bolt11_for_amount(2_000, 0x33, CLOCK_NOW);
        store
            .claim_paying(
                &AttemptId::new("request:restart"),
                &minted.payment_hash_hex,
                &minted.bolt11,
                Amount::from_msat(2_000),
                expires,
            )
            .await
            .expect("claim");
    }
    let store2 = JsonPaymentStore::open(&path).expect("reopen");
    let record = store2
        .get(&AttemptId::new("request:restart"))
        .await
        .expect("get")
        .expect("present");
    assert_eq!(record.expires_at_unix, expires);
    assert_eq!(record.state, PersistedPaymentState::Paying);
    assert_eq!(record.amount.as_msat(), 2_000);
}

#[tokio::test]
async fn confirm_unknown_handle_fails_cleanly_cancel_removes_handle() {
    let h = harness("community-handles");
    h.runtime.link(VALID_URI).await.expect("link");

    let err = h
        .runtime
        .confirm("does-not-exist")
        .await
        .expect_err("unknown handle");
    assert_eq!(err, "unknown_handle");

    let minted = mint_bolt11_for_amount(3_000, 0x44, CLOCK_NOW);
    let quote = h
        .runtime
        .prepare_send(
            SendTargetDto::Bolt11 {
                invoice: minted.bolt11.as_str().to_string(),
            },
            3_000,
            AttemptKeyDto::Standalone,
            None,
        )
        .await
        .expect("prepare");
    h.runtime.cancel(&quote.handle_id).expect("cancel");
    let err = h
        .runtime
        .confirm(&quote.handle_id)
        .await
        .expect_err("cancelled");
    assert_eq!(err, "unknown_handle");
}

#[tokio::test]
async fn wallet_status_serialized_contains_no_nwc_uri_substring() {
    let h = harness("community-hygiene");
    let status = h.runtime.link(VALID_URI).await.expect("link");
    let json = serde_json::to_string(&status).expect("serialize");
    assert!(
        !json.contains("nostr+walletconnect"),
        "status JSON must not embed NWC URI: {json}"
    );
    assert!(!json.contains("secret=abc"));
    let _probe: WalletStatusView = status;
}

#[test]
fn nwc_blob_key_convention() {
    assert_eq!(nwc_blob_key("abc-123"), "nwc:abc-123");
}

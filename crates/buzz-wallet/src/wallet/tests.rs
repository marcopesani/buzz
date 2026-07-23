//! Link + Receive + Send + Lifecycle + Trust acceptance scenarios.

use crate::error::WalletError;
use crate::fakes::{
    ConnectorScript, FakeClock, FakeLnurlResolver, FakeWalletConnector, FakeWalletService,
    InMemoryPaymentStore, InMemorySecretStore, InvoiceScript, LnurlScript, MakeInvoiceScript,
    PayScript, RecordingProfilePublisher, SeededKind0, WalletCall,
};
use crate::ports::{PaymentStore, SecretStore, WalletService};
use crate::test_support::{mint_bolt11, mint_bolt11_for_amount, MintInvoiceParams};
use crate::types::{
    AttemptId, AttemptKey, Bolt11, Capabilities, IncomingStatus, InvoiceStatus, Kind0Fields,
    PersistedPaymentState, ReceiveMode, ResolvedPay, SendOutcome, SendTarget, StoredSecret,
    WalletMethod, WalletTimeouts,
};
use crate::wallet::Wallet;
use async_trait::async_trait;
use buzz_core::payment::{verify, Amount, PaymentReceipt, PaymentRequest, PaymentTarget};
use std::sync::Arc;
use std::time::Duration;

const COMMUNITY: &str = "community-a";
const VALID_URI: &str = "nostr+walletconnect://pubkey?relay=wss://relay.example&secret=abc";
const CLOCK_NOW: u64 = 1_700_000_000;
const BOB: &str = "bob@example.com";
fn default_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(5),
        pay_response: Duration::from_secs(30),
    }
}

fn short_pay_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(5),
        pay_response: Duration::from_secs(1),
    }
}

fn full_capabilities() -> Capabilities {
    Capabilities::from_methods([
        WalletMethod::PayInvoice,
        WalletMethod::MakeInvoice,
        WalletMethod::LookupInvoice,
        WalletMethod::GetBalance,
    ])
}

struct Harness {
    wallet: Wallet,
    service: Arc<FakeWalletService>,
    resolver: Arc<FakeLnurlResolver>,
    store: Arc<InMemoryPaymentStore>,
    clock: Arc<FakeClock>,
}

fn harness_linked() -> Harness {
    harness_with_timeouts(default_timeouts())
}

fn harness_with_timeouts(timeouts: WalletTimeouts) -> Harness {
    let service = Arc::new(FakeWalletService::new());
    service.set_balance(Some(Amount::from_msat(100_000_000)));
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service) as Arc<dyn WalletService>,
        capabilities: full_capabilities(),
        lud16: None,
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let resolver = Arc::new(FakeLnurlResolver::new());
    let store = Arc::new(InMemoryPaymentStore::new(COMMUNITY));
    let clock = Arc::new(FakeClock::new(CLOCK_NOW));
    let wallet = Wallet::new(
        connector as Arc<dyn crate::ports::WalletConnector>,
        Arc::clone(&secrets) as Arc<dyn SecretStore>,
        profiles as Arc<dyn crate::ports::ProfilePublisher>,
        Arc::clone(&resolver) as Arc<dyn crate::ports::LnurlResolver>,
        Arc::clone(&store) as Arc<dyn PaymentStore>,
        Arc::clone(&clock) as Arc<dyn crate::ports::Clock>,
        timeouts,
    );
    Harness {
        wallet,
        service,
        resolver,
        store,
        clock,
    }
}

fn link_only_harness(
    connector: Arc<FakeWalletConnector>,
    secrets: Arc<InMemorySecretStore>,
    profiles: Arc<RecordingProfilePublisher>,
) -> Wallet {
    Wallet::new(
        connector as Arc<dyn crate::ports::WalletConnector>,
        secrets as Arc<dyn SecretStore>,
        profiles as Arc<dyn crate::ports::ProfilePublisher>,
        Arc::new(FakeLnurlResolver::new()) as Arc<dyn crate::ports::LnurlResolver>,
        Arc::new(InMemoryPaymentStore::new(COMMUNITY)) as Arc<dyn PaymentStore>,
        Arc::new(FakeClock::new(CLOCK_NOW)) as Arc<dyn crate::ports::Clock>,
        default_timeouts(),
    )
}

fn wallet_over_store(
    store: Arc<InMemoryPaymentStore>,
    service: Arc<FakeWalletService>,
    clock: Arc<FakeClock>,
) -> Wallet {
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service) as Arc<dyn WalletService>,
        capabilities: full_capabilities(),
        lud16: None,
    });
    Wallet::new(
        connector as Arc<dyn crate::ports::WalletConnector>,
        Arc::new(InMemorySecretStore::new(store.community_id())) as Arc<dyn SecretStore>,
        Arc::new(RecordingProfilePublisher::new()) as Arc<dyn crate::ports::ProfilePublisher>,
        Arc::new(FakeLnurlResolver::new()) as Arc<dyn crate::ports::LnurlResolver>,
        Arc::clone(&store) as Arc<dyn PaymentStore>,
        clock as Arc<dyn crate::ports::Clock>,
        default_timeouts(),
    )
}

fn script_lud16(
    resolver: &FakeLnurlResolver,
    minted: &crate::test_support::MintedInvoice,
    amount: Amount,
) {
    resolver.script(
        BOB,
        LnurlScript::Ok(ResolvedPay {
            bolt11: minted.bolt11.clone(),
            payment_hash: minted.payment_hash_hex.clone(),
            amount,
            min_sendable: Amount::from_msat(1),
            max_sendable: Amount::from_msat(100_000_000),
        }),
    );
}

fn pay_invoice_calls(service: &FakeWalletService) -> Vec<Bolt11> {
    service
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            WalletCall::PayInvoice { bolt11 } => Some(bolt11),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Feature: Link a wallet
// ---------------------------------------------------------------------------

/// Scenario: Link a wallet that advertises full capabilities
#[tokio::test]
async fn link_a_wallet_that_advertises_full_capabilities() {
    let service = Arc::new(FakeWalletService::new());
    let caps = full_capabilities();
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service,
        capabilities: caps.clone(),
        lud16: None,
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(Arc::clone(&connector), Arc::clone(&secrets), profiles);

    let handle = wallet.link(VALID_URI).await.expect("link");

    assert_eq!(handle.capabilities, caps);
    assert_eq!(
        handle.capabilities,
        Capabilities::from_methods([
            WalletMethod::PayInvoice,
            WalletMethod::MakeInvoice,
            WalletMethod::LookupInvoice,
            WalletMethod::GetBalance,
        ])
    );
    let stored = secrets.load().await.expect("load").expect("secret stored");
    assert_eq!(stored.uri, VALID_URI);
    assert_eq!(stored.capabilities, caps);
    assert_eq!(secrets.community_id(), COMMUNITY);
}

/// Scenario: A malformed URI stores nothing
#[tokio::test]
async fn a_malformed_uri_stores_nothing() {
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Fail(WalletError::InvalidUri));
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let before = secrets.snapshot();
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(connector, Arc::clone(&secrets), profiles);

    let err = wallet
        .link("nostr+walletconnect://abc")
        .await
        .expect_err("must fail");

    assert_eq!(err, WalletError::InvalidUri);
    secrets.assert_unchanged_since(&before);
}

/// Scenario: An unreachable wallet relay stores nothing
#[tokio::test(start_paused = true)]
async fn an_unreachable_wallet_relay_stores_nothing() {
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::NeverRespond);
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let before = secrets.snapshot();
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(connector, Arc::clone(&secrets), profiles);

    let err = wallet.link(VALID_URI).await.expect_err("must fail");

    assert_eq!(err, WalletError::Unreachable);
    secrets.assert_unchanged_since(&before);
}

/// Scenario: Linking publishes the lightning address
#[tokio::test]
async fn linking_publishes_the_lightning_address() {
    let service = Arc::new(FakeWalletService::new());
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service,
        capabilities: full_capabilities(),
        lud16: Some("alice@example.com".into()),
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    profiles.seed(SeededKind0 {
        lud16: None,
        display_name: Some("Alice".into()),
    });
    let wallet = link_only_harness(connector, Arc::clone(&secrets), Arc::clone(&profiles));

    let handle = wallet.link(VALID_URI).await.expect("link");

    assert_eq!(handle.lud16.as_deref(), Some("alice@example.com"));
    assert_eq!(
        profiles.published(),
        vec![Kind0Fields {
            lud16: Some("alice@example.com".into()),
        }]
    );
    assert_eq!(
        profiles.profile(),
        SeededKind0 {
            lud16: Some("alice@example.com".into()),
            display_name: Some("Alice".into()),
        }
    );
    let stored = secrets.load().await.expect("load").expect("secret");
    assert_eq!(stored.lud16.as_deref(), Some("alice@example.com"));
}

/// Skip case (Flows § Link step 5): existing different lud16 → no publish.
#[tokio::test]
async fn linking_skips_publish_when_existing_lud16_differs() {
    let service = Arc::new(FakeWalletService::new());
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service,
        capabilities: full_capabilities(),
        lud16: Some("alice@example.com".into()),
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    profiles.seed(SeededKind0 {
        lud16: Some("bob@example.com".into()),
        display_name: Some("Bob".into()),
    });
    let wallet = link_only_harness(connector, Arc::clone(&secrets), Arc::clone(&profiles));

    let handle = wallet.link(VALID_URI).await.expect("link");

    assert_eq!(handle.lud16.as_deref(), Some("alice@example.com"));
    assert!(
        profiles.published().is_empty(),
        "must not overwrite a different existing lud16"
    );
    assert_eq!(
        profiles.profile().lud16.as_deref(),
        Some("bob@example.com"),
        "profile lud16 unchanged"
    );
    let stored = secrets.load().await.expect("load").expect("secret");
    assert_eq!(stored.lud16.as_deref(), Some("alice@example.com"));
}

// ---------------------------------------------------------------------------
// Feature: Receive
// ---------------------------------------------------------------------------

/// Scenario: Static receive with a lightning address
#[tokio::test]
async fn static_receive_with_a_lightning_address() {
    let service = Arc::new(FakeWalletService::new());
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service) as Arc<dyn WalletService>,
        capabilities: full_capabilities(),
        lud16: Some("alice@example.com".into()),
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(connector, secrets, profiles);

    wallet.link(VALID_URI).await.expect("link");

    let mode = wallet.receive_mode().await.expect("receive_mode");
    assert_eq!(mode, ReceiveMode::StaticAddress("alice@example.com".into()));
    assert!(
        !service
            .calls()
            .iter()
            .any(|c| matches!(c, WalletCall::MakeInvoice { .. })),
        "make_invoice must never be called for static receive"
    );
}

/// Scenario: Interactive receive without a lightning address
#[tokio::test]
async fn interactive_receive_without_a_lightning_address() {
    let service = Arc::new(FakeWalletService::new());
    service.script_make_invoice(MakeInvoiceScript::Ok(Bolt11::new("lnbc1coffee")));
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service) as Arc<dyn WalletService>,
        capabilities: full_capabilities(),
        lud16: None,
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(connector, secrets, profiles);

    wallet.link(VALID_URI).await.expect("link");
    assert_eq!(
        wallet.receive_mode().await.expect("mode"),
        ReceiveMode::Interactive
    );

    let bolt11 = wallet
        .receive(Amount::from_msat(21_000), Some("coffee"))
        .await
        .expect("receive");

    assert_eq!(bolt11.as_str(), "lnbc1coffee");
    let expected = WalletCall::MakeInvoice {
        amount: Amount::from_msat(21_000),
        memo: Some("coffee".into()),
    };
    assert_eq!(service.call_count(&expected), 1);
    assert_eq!(service.calls(), vec![expected]);
}

/// Scenario: Receive unavailable without make_invoice or an address
#[tokio::test]
async fn receive_unavailable_without_make_invoice_or_an_address() {
    let service = Arc::new(FakeWalletService::new());
    let caps = Capabilities::from_methods([
        WalletMethod::PayInvoice,
        WalletMethod::LookupInvoice,
        WalletMethod::GetBalance,
    ]);
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service,
        capabilities: caps.clone(),
        lud16: None,
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = link_only_harness(connector, Arc::clone(&secrets), profiles);

    wallet.link(VALID_URI).await.expect("link");

    assert_eq!(
        wallet.receive_mode().await.expect("mode"),
        ReceiveMode::Unavailable
    );
    let stored = secrets.load().await.expect("load").expect("still linked");
    assert_eq!(stored.uri, VALID_URI);
    assert_eq!(stored.capabilities, caps);
    assert!(stored.lud16.is_none());
}

/// receive_mode after "restart": persisted lud16 alone decides StaticAddress
/// without a live WalletService call.
#[tokio::test]
async fn receive_mode_uses_persisted_lud16_without_network() {
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    secrets
        .store(&StoredSecret {
            uri: VALID_URI.into(),
            capabilities: Capabilities::empty(),
            lud16: Some("alice@example.com".into()),
        })
        .await
        .expect("store");
    let wallet = link_only_harness(
        Arc::new(FakeWalletConnector::new()),
        secrets,
        Arc::new(RecordingProfilePublisher::new()),
    );

    assert_eq!(
        wallet.receive_mode().await.expect("mode"),
        ReceiveMode::StaticAddress("alice@example.com".into())
    );
}

/// balance() returns the linked FakeWalletService get_balance (msat).
#[tokio::test]
async fn balance_returns_linked_service_balance() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");
    let bal = h.wallet.balance().await.expect("balance");
    assert_eq!(bal, Some(Amount::from_msat(100_000_000)));
    assert!(
        h.service
            .calls()
            .iter()
            .any(|c| matches!(c, WalletCall::GetBalance)),
        "get_balance must be called on the linked service"
    );
}

// ---------------------------------------------------------------------------
// Feature: Send to a lightning address
// ---------------------------------------------------------------------------

/// Scenario: Happy path
#[tokio::test]
async fn happy_path() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x11, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::Settle {
        preimage: minted.preimage_hex.clone(),
    });

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    assert_eq!(handle.payment_hash(), minted.payment_hash_hex);
    assert_eq!(handle.bolt11(), &minted.bolt11);

    let outcome = h.wallet.confirm(&handle).await.expect("confirm");
    match outcome {
        SendOutcome::Settled { preimage } => {
            assert_eq!(preimage, minted.preimage_hex);
            assert!(verify(&preimage, &minted.payment_hash_hex));
        }
        other => panic!("expected Settled, got {other:?}"),
    }

    assert_eq!(pay_invoice_calls(&h.service), vec![minted.bolt11.clone()]);
    let records = h.store.all();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, PersistedPaymentState::Settled);
    assert_eq!(records[0].payment_hash, minted.payment_hash_hex);
}

/// Scenario: No confirmation, no spend
#[tokio::test]
async fn no_confirmation_no_spend() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x22, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);

    let _handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");

    assert!(
        pay_invoice_calls(&h.service).is_empty(),
        "pay_invoice must never be called without confirm"
    );
    assert!(h.store.all().is_empty(), "nothing persisted before confirm");
}

/// Scenario: Resolver amount mismatch is rejected before confirmation
#[tokio::test]
async fn resolver_amount_mismatch_is_rejected_before_confirmation() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let requested = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(24_000, 0x33, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, Amount::from_msat(24_000));

    let err = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            requested,
            None,
        )
        .await
        .expect_err("mismatch");

    assert_eq!(err, WalletError::ResolveRejected);
    assert!(pay_invoice_calls(&h.service).is_empty());
    assert!(h.store.all().is_empty());
}

/// Scenario: An amountless invoice is rejected
#[tokio::test]
async fn an_amountless_invoice_is_rejected() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11(MintInvoiceParams {
        amount_msat: None,
        preimage: [0x44; 32],
        timestamp_unix: CLOCK_NOW,
        expiry_secs: 3_600,
        description: None,
    });
    script_lud16(&h.resolver, &minted, amount);

    let err = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect_err("amountless");

    assert_eq!(err, WalletError::ResolveRejected);
    assert!(pay_invoice_calls(&h.service).is_empty());
}

/// Scenario: Insufficient balance is definitive
#[tokio::test]
async fn insufficient_balance_is_definitive() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x55, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service
        .script_pay(PayScript::Fail(WalletError::InsufficientBalance));

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    let outcome = h.wallet.confirm(&handle).await.expect("confirm");

    assert_eq!(
        outcome,
        SendOutcome::Failed {
            reason: WalletError::InsufficientBalance
        }
    );
    let records = h.store.all();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, PersistedPaymentState::Failed);
    assert!(h
        .store
        .list_by_states(&[
            PersistedPaymentState::Unknown,
            PersistedPaymentState::Paying
        ])
        .await
        .expect("list")
        .is_empty());
}

// ---------------------------------------------------------------------------
// U4 guards
// ---------------------------------------------------------------------------

/// Guard: expired invoice rejected at prepare (clock-driven)
#[tokio::test]
async fn expired_invoice_rejected_at_prepare() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    // Invoice created in the past with a short expiry; clock is already past it.
    let minted = mint_bolt11(MintInvoiceParams {
        amount_msat: Some(25_000),
        preimage: [0x66; 32],
        timestamp_unix: CLOCK_NOW - 7_200,
        expiry_secs: 3_600,
        description: None,
    });
    script_lud16(&h.resolver, &minted, amount);

    let err = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect_err("expired");

    assert_eq!(err, WalletError::ResolveRejected);
    assert!(pay_invoice_calls(&h.service).is_empty());
}

/// Guard: confirm-after-cancel is impossible (handle consumed by cancel)
#[tokio::test]
async fn confirm_after_cancel_is_impossible() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x77, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    h.wallet.cancel(handle);
    // handle moved — confirm(&handle) does not compile. Prove nothing was paid.
    assert!(pay_invoice_calls(&h.service).is_empty());
    assert!(h.store.all().is_empty());
}

/// Scenario: A double confirmation fires one payment
#[tokio::test]
async fn a_double_confirmation_fires_one_payment() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x88, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::Settle {
        preimage: minted.preimage_hex.clone(),
    });

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");

    let first = h.wallet.confirm(&handle).await.expect("first confirm");
    assert!(matches!(first, SendOutcome::Settled { .. }));

    let second = h.wallet.confirm(&handle).await.expect("second confirm");
    assert_eq!(
        second,
        SendOutcome::AlreadyClaimed {
            state: PersistedPaymentState::Settled
        }
    );
    assert_eq!(pay_invoice_calls(&h.service).len(), 1);
}

/// Guard: persist-before-pay — store holds Paying with H before pay_invoice runs
#[tokio::test]
async fn persist_before_pay_ordering() {
    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0x99, CLOCK_NOW);

    let store = Arc::new(InMemoryPaymentStore::new(COMMUNITY));
    let inner = Arc::new(FakeWalletService::new());
    inner.script_pay(PayScript::Fail(WalletError::PaymentFailed));
    let checking = Arc::new(StoreCheckingWallet {
        store: Arc::clone(&store),
        expected_hash: minted.payment_hash_hex.clone(),
        inner: Arc::clone(&inner),
        saw_paying: std::sync::Mutex::new(false),
    });

    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&checking) as Arc<dyn WalletService>,
        capabilities: full_capabilities(),
        lud16: None,
    });
    let resolver = Arc::new(FakeLnurlResolver::new());
    script_lud16(&resolver, &minted, amount);
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let clock = Arc::new(FakeClock::new(CLOCK_NOW));
    let wallet = Wallet::new(
        connector as Arc<dyn crate::ports::WalletConnector>,
        secrets as Arc<dyn SecretStore>,
        Arc::new(RecordingProfilePublisher::new()) as Arc<dyn crate::ports::ProfilePublisher>,
        resolver as Arc<dyn crate::ports::LnurlResolver>,
        Arc::clone(&store) as Arc<dyn PaymentStore>,
        clock as Arc<dyn crate::ports::Clock>,
        default_timeouts(),
    );
    wallet.link(VALID_URI).await.expect("link");

    let handle = wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    let outcome = wallet.confirm(&handle).await.expect("confirm");
    assert_eq!(
        outcome,
        SendOutcome::Failed {
            reason: WalletError::PaymentFailed
        }
    );
    assert!(
        checking.saw_paying_before_pay(),
        "Paying record with H must exist before pay_invoice"
    );
}

/// WalletService wrapper that records whether the store already holds Paying
/// for the expected hash at the moment `pay_invoice` is entered.
struct StoreCheckingWallet {
    store: Arc<InMemoryPaymentStore>,
    expected_hash: String,
    inner: Arc<FakeWalletService>,
    saw_paying: std::sync::Mutex<bool>,
}

impl StoreCheckingWallet {
    fn saw_paying_before_pay(&self) -> bool {
        *self.saw_paying.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl WalletService for StoreCheckingWallet {
    async fn get_balance(&self) -> Result<Option<Amount>, WalletError> {
        self.inner.get_balance().await
    }

    async fn make_invoice(
        &self,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<Bolt11, WalletError> {
        self.inner.make_invoice(amount, memo).await
    }

    async fn pay_invoice(&self, bolt11: &Bolt11) -> Result<String, WalletError> {
        let paying = self
            .store
            .list_by_states(&[PersistedPaymentState::Paying])
            .await
            .expect("list");
        let ok = paying
            .iter()
            .any(|r| r.payment_hash == self.expected_hash && r.bolt11 == *bolt11);
        *self.saw_paying.lock().unwrap_or_else(|e| e.into_inner()) = ok;
        self.inner.pay_invoice(bolt11).await
    }

    async fn lookup_invoice(
        &self,
        payment_hash: &str,
    ) -> Result<crate::types::InvoiceStatus, WalletError> {
        self.inner.lookup_invoice(payment_hash).await
    }

    async fn list_transactions(&self) -> Result<Vec<crate::types::Tx>, WalletError> {
        self.inner.list_transactions().await
    }
}

fn lookup_calls(service: &FakeWalletService) -> Vec<String> {
    service
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            WalletCall::LookupInvoice { payment_hash } => Some(payment_hash),
            _ => None,
        })
        .collect()
}

fn payment_request_bolt11(bolt11: &Bolt11, amount: Amount) -> PaymentRequest {
    PaymentRequest {
        amount,
        memo: None,
        target: PaymentTarget::Bolt11(bolt11.as_str().to_string()),
        channel_id: "channel-c".into(),
        payee_pubkey: "payee-pubkey".into(),
        expiry: Some(CLOCK_NOW + 3_600),
    }
}

// ---------------------------------------------------------------------------
// Feature: Payment lifecycle
// ---------------------------------------------------------------------------

/// Scenario: A timeout is Unknown, not Failed
#[tokio::test(start_paused = true)]
async fn a_timeout_is_unknown_not_failed() {
    let h = harness_with_timeouts(short_pay_timeouts());
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0xaa, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::NeverRespond);

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    let outcome = h.wallet.confirm(&handle).await.expect("confirm");

    assert_eq!(outcome, SendOutcome::Unknown);
    assert_eq!(h.store.all()[0].state, PersistedPaymentState::Unknown);
    assert_eq!(pay_invoice_calls(&h.service).len(), 1);
}

/// Scenario: Unknown reconciles to Settled
#[tokio::test(start_paused = true)]
async fn unknown_reconciles_to_settled() {
    let h = harness_with_timeouts(short_pay_timeouts());
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0xab, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::NeverRespond);

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    assert_eq!(
        h.wallet.confirm(&handle).await.expect("confirm"),
        SendOutcome::Unknown
    );
    let pay_count_before = pay_invoice_calls(&h.service).len();

    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Settled),
    );
    h.wallet.reconcile().await.expect("reconcile");

    assert_eq!(h.store.all()[0].state, PersistedPaymentState::Settled);
    assert_eq!(pay_invoice_calls(&h.service).len(), pay_count_before);
    assert_eq!(lookup_calls(&h.service), vec![minted.payment_hash_hex]);
}

/// Scenario: Unknown reconciles to Failed
#[tokio::test(start_paused = true)]
async fn unknown_reconciles_to_failed() {
    let h = harness_with_timeouts(short_pay_timeouts());
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0xac, CLOCK_NOW);
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::NeverRespond);

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    assert_eq!(
        h.wallet.confirm(&handle).await.expect("confirm"),
        SendOutcome::Unknown
    );

    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Failed),
    );
    h.wallet.reconcile().await.expect("reconcile");

    assert_eq!(h.store.all()[0].state, PersistedPaymentState::Failed);
    assert_eq!(pay_invoice_calls(&h.service).len(), 1);
}

/// Scenario: A double-tap on a lud16 pay card fires one payment
#[tokio::test]
async fn a_double_tap_on_a_lud16_pay_card_fires_one_payment() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let first_mint = mint_bolt11_for_amount(25_000, 0xad, CLOCK_NOW);
    let second_mint = mint_bolt11_for_amount(25_000, 0xae, CLOCK_NOW);
    assert_ne!(first_mint.payment_hash_hex, second_mint.payment_hash_hex);
    script_lud16(&h.resolver, &first_mint, amount);
    script_lud16(&h.resolver, &second_mint, amount);
    h.service.script_pay(PayScript::Settle {
        preimage: first_mint.preimage_hex.clone(),
    });

    let card = AttemptKey::PayRequest {
        event_id: "req-event-lud16-card".into(),
    };
    let handle_a = h
        .wallet
        .prepare_send(card.clone(), SendTarget::Lud16(BOB.into()), amount, None)
        .await
        .expect("prepare first tap");
    let handle_b = h
        .wallet
        .prepare_send(card, SendTarget::Lud16(BOB.into()), amount, None)
        .await
        .expect("prepare second tap");

    assert_eq!(handle_a.attempt_id(), handle_b.attempt_id());
    assert_ne!(handle_a.payment_hash(), handle_b.payment_hash());

    let first = h.wallet.confirm(&handle_a).await.expect("confirm a");
    let second = h.wallet.confirm(&handle_b).await.expect("confirm b");

    assert!(matches!(first, SendOutcome::Settled { .. }));
    assert_eq!(
        second,
        SendOutcome::AlreadyClaimed {
            state: PersistedPaymentState::Settled
        }
    );
    assert_eq!(pay_invoice_calls(&h.service).len(), 1);
    assert_eq!(h.store.all().len(), 1);
    assert_eq!(
        h.store
            .list_by_states(&[PersistedPaymentState::Paying])
            .await
            .expect("list")
            .len(),
        0
    );
}

/// Scenario: A lookup that finds no invoice stays Unknown until the invoice expires
#[tokio::test(start_paused = true)]
async fn a_lookup_that_finds_no_invoice_stays_unknown_until_the_invoice_expires() {
    let h = harness_with_timeouts(short_pay_timeouts());
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(25_000);
    let expiry_secs = 3_600u64;
    let minted = mint_bolt11(MintInvoiceParams {
        amount_msat: Some(25_000),
        preimage: [0xaf; 32],
        timestamp_unix: CLOCK_NOW,
        expiry_secs,
        description: None,
    });
    script_lud16(&h.resolver, &minted, amount);
    h.service.script_pay(PayScript::NeverRespond);

    let handle = h
        .wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Lud16(BOB.into()),
            amount,
            None,
        )
        .await
        .expect("prepare");
    assert_eq!(
        h.wallet.confirm(&handle).await.expect("confirm"),
        SendOutcome::Unknown
    );
    let expires_at = handle.expires_at_unix();

    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::NotFound),
    );
    h.wallet.reconcile().await.expect("reconcile before expiry");
    assert_eq!(h.store.all()[0].state, PersistedPaymentState::Unknown);

    h.clock.set(expires_at);
    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::NotFound),
    );
    h.wallet.reconcile().await.expect("reconcile after expiry");
    assert_eq!(h.store.all()[0].state, PersistedPaymentState::Failed);
    assert_eq!(pay_invoice_calls(&h.service).len(), 1);
}

/// Scenario: Reconciliation survives a restart
#[tokio::test]
async fn reconciliation_survives_a_restart() {
    let store = Arc::new(InMemoryPaymentStore::new(COMMUNITY));
    let service = Arc::new(FakeWalletService::new());
    let clock = Arc::new(FakeClock::new(CLOCK_NOW));
    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0xb0, CLOCK_NOW);

    // Crash mid-pay: record already latched in Paying, no wallet response yet.
    let claim = store
        .claim_paying(
            &AttemptId::new("standalone:restart-attempt"),
            &minted.payment_hash_hex,
            &minted.bolt11,
            amount,
            CLOCK_NOW + 3_600,
        )
        .await
        .expect("claim");
    assert!(matches!(claim, crate::types::ClaimOutcome::Claimed(_)));
    assert_eq!(store.all()[0].state, PersistedPaymentState::Paying);

    // Restart: new Wallet over the same store.
    let wallet = wallet_over_store(Arc::clone(&store), Arc::clone(&service), Arc::clone(&clock));
    wallet.link(VALID_URI).await.expect("link after restart");
    service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Settled),
    );
    wallet.reconcile().await.expect("reconcile");

    assert_eq!(store.all()[0].state, PersistedPaymentState::Settled);
    assert!(
        pay_invoice_calls(&service).is_empty(),
        "restart reconcile must not call pay_invoice"
    );
}

/// Scenario: A community switch does not orphan a pending payment
#[tokio::test]
async fn a_community_switch_does_not_orphan_a_pending_payment() {
    let store_x = Arc::new(InMemoryPaymentStore::new("community-x"));
    let service_x = Arc::new(FakeWalletService::new());
    let clock = Arc::new(FakeClock::new(CLOCK_NOW));
    let amount = Amount::from_msat(25_000);
    let minted = mint_bolt11_for_amount(25_000, 0xb1, CLOCK_NOW);

    store_x
        .claim_paying(
            &AttemptId::new("request:card-x"),
            &minted.payment_hash_hex,
            &minted.bolt11,
            amount,
            CLOCK_NOW + 3_600,
        )
        .await
        .expect("claim");
    store_x
        .update_state(
            &AttemptId::new("request:card-x"),
            PersistedPaymentState::Unknown,
        )
        .await
        .expect("mark unknown");

    // Switch to community Y: different store / connection. X's record untouched.
    let store_y = Arc::new(InMemoryPaymentStore::new("community-y"));
    let service_y = Arc::new(FakeWalletService::new());
    let wallet_y = wallet_over_store(Arc::clone(&store_y), service_y, Arc::clone(&clock));
    wallet_y.link(VALID_URI).await.expect("link y");
    assert!(store_y.all().is_empty());
    assert_eq!(store_x.all().len(), 1);
    assert_eq!(store_x.all()[0].state, PersistedPaymentState::Unknown);

    // Switch back to X: fresh Wallet over X's store (reset drops connection only).
    let wallet_x = wallet_over_store(Arc::clone(&store_x), Arc::clone(&service_x), clock);
    wallet_x.link(VALID_URI).await.expect("link x again");
    assert_eq!(store_x.all().len(), 1);
    assert_eq!(store_x.all()[0].payment_hash, minted.payment_hash_hex);

    service_x.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Settled),
    );
    wallet_x.reconcile().await.expect("reconcile x");
    assert_eq!(store_x.all()[0].state, PersistedPaymentState::Settled);
    assert!(pay_invoice_calls(&service_x).is_empty());
}

// ---------------------------------------------------------------------------
// Feature: Trust is local
// ---------------------------------------------------------------------------

/// Scenario: A forged receipt does not convince the payee
#[tokio::test]
async fn a_forged_receipt_does_not_convince_the_payee() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(500_000);
    let minted = mint_bolt11_for_amount(500_000, 0xb2, CLOCK_NOW);
    let request = payment_request_bolt11(&minted.bolt11, amount);

    // Channel noise: a forged receipt exists — but check_incoming cannot accept it.
    let _forged_receipt = PaymentReceipt {
        request_id: "req-event-b".into(),
        payment_hash: minted.payment_hash_hex.clone(),
        preimage: hex::encode([0xde; 32]),
        amount,
    };

    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::NotFound),
    );
    let status = h.wallet.check_incoming(&request).await.expect("check");
    assert_eq!(status, IncomingStatus::Unpaid);
}

/// Scenario: The payee confirms from its own wallet
#[tokio::test]
async fn the_payee_confirms_from_its_own_wallet() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let amount = Amount::from_msat(500_000);
    let minted = mint_bolt11_for_amount(500_000, 0xb3, CLOCK_NOW);
    let request = payment_request_bolt11(&minted.bolt11, amount);

    h.service.script_lookup(
        &minted.payment_hash_hex,
        InvoiceScript::Status(InvoiceStatus::Settled),
    );
    let status = h.wallet.check_incoming(&request).await.expect("check");
    assert_eq!(status, IncomingStatus::Paid);
}

/// Scenario: A lud16-only request is not confirmable
#[tokio::test]
async fn a_lud16_only_request_is_not_confirmable() {
    let h = harness_linked();
    h.wallet.link(VALID_URI).await.expect("link");

    let request = PaymentRequest {
        amount: Amount::from_msat(500_000),
        memo: None,
        target: PaymentTarget::Lud16(BOB.into()),
        channel_id: "channel-c".into(),
        payee_pubkey: "payee-pubkey".into(),
        expiry: Some(CLOCK_NOW + 3_600),
    };

    let status = h.wallet.check_incoming(&request).await.expect("check");
    assert_eq!(status, IncomingStatus::Unconfirmable);
    assert!(
        lookup_calls(&h.service).is_empty(),
        "lud16-only has no hash to look up"
    );
}

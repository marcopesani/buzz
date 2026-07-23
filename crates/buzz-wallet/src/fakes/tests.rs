//! Fake-harness self-tests — scripting contract U3–U5 will rely on.

use crate::error::WalletError;
use crate::fakes::{
    ConnectorScript, FakeClock, FakeLnurlResolver, FakeWalletConnector, FakeWalletService,
    InMemoryPaymentStore, InMemorySecretStore, InvoiceScript, LnurlScript, MakeInvoiceScript,
    PayScript, RecordingProfilePublisher, WalletCall,
};
use crate::ports::{
    Clock, LnurlResolver, PaymentStore, ProfilePublisher, SecretStore, WalletConnector,
    WalletService,
};
use crate::types::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, InvoiceStatus, Kind0Fields,
    PersistedPaymentState, ResolvedPay, StoredSecret, WalletMethod,
};
use buzz_core::payment::Amount;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Duration as TokioDuration};

#[tokio::test]
async fn scripted_settle_returns_preimage_and_logs_one_pay_invoice_call() {
    let wallet = FakeWalletService::new();
    wallet.script_pay(PayScript::Settle {
        preimage: "aa".repeat(32),
    });

    let bolt11 = Bolt11::new("lnbc1test");
    let preimage = wallet.pay_invoice(&bolt11).await.expect("settle");
    assert_eq!(preimage, "aa".repeat(32));
    assert_eq!(
        wallet.calls(),
        vec![WalletCall::PayInvoice {
            bolt11: bolt11.clone()
        }]
    );
    assert_eq!(wallet.call_count(&WalletCall::PayInvoice { bolt11 }), 1);
}

#[tokio::test]
async fn scripted_error_surfaces_exact_wallet_error() {
    let wallet = FakeWalletService::new();
    wallet.script_pay(PayScript::Fail(WalletError::InsufficientBalance));

    let err = wallet
        .pay_invoice(&Bolt11::new("lnbc1fail"))
        .await
        .expect_err("must fail");
    assert_eq!(err, WalletError::InsufficientBalance);
}

#[tokio::test(start_paused = true)]
async fn never_respond_yields_future_that_stays_pending() {
    let wallet = FakeWalletService::new();
    wallet.script_pay(PayScript::NeverRespond);

    let result = timeout(
        TokioDuration::from_secs(60),
        wallet.pay_invoice(&Bolt11::new("lnbc1hang")),
    )
    .await;

    assert!(
        result.is_err(),
        "NeverRespond must stay pending past the timeout"
    );
    // Call was still recorded when the method started.
    assert_eq!(
        wallet.call_count(&WalletCall::PayInvoice {
            bolt11: Bolt11::new("lnbc1hang")
        }),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn delay_script_honours_tokio_time_pause() {
    let wallet = Arc::new(FakeWalletService::new());
    wallet.script_pay(PayScript::Delay {
        duration: Duration::from_secs(30),
        then: Box::new(PayScript::Settle {
            preimage: "bb".repeat(32),
        }),
    });
    let w = Arc::clone(&wallet);
    let handle = tokio::spawn(async move { w.pay_invoice(&Bolt11::new("lnbc1delay")).await });

    tokio::task::yield_now().await;
    assert!(!handle.is_finished());

    tokio::time::advance(TokioDuration::from_secs(30)).await;
    let preimage = handle.await.expect("join").expect("settle after delay");
    assert_eq!(preimage, "bb".repeat(32));
}

#[tokio::test]
async fn call_log_asserts_exactly_once_and_never() {
    let wallet = FakeWalletService::new();
    wallet.script_make_invoice(MakeInvoiceScript::Ok(Bolt11::new("lnbc1recv")));
    wallet.script_lookup("hash-1", InvoiceScript::Status(InvoiceStatus::Pending));

    let _ = wallet
        .make_invoice(Amount::from_msat(1_000), Some("coffee"))
        .await
        .unwrap();
    let _ = wallet.lookup_invoice("hash-1").await.unwrap();

    let make = WalletCall::MakeInvoice {
        amount: Amount::from_msat(1_000),
        memo: Some("coffee".into()),
    };
    assert_eq!(wallet.call_count(&make), 1);
    assert_eq!(
        wallet.call_count(&WalletCall::PayInvoice {
            bolt11: Bolt11::new("lnbc1never")
        }),
        0,
        "pay_invoice must never have been called"
    );
}

#[tokio::test]
async fn claim_paying_two_concurrent_claims_same_attempt_admits_exactly_one() {
    let store = Arc::new(InMemoryPaymentStore::new("community-a"));
    let attempt = AttemptId::new("attempt-1");
    let bolt11 = Bolt11::new("lnbc1a");
    let amount = Amount::from_msat(500_000);

    let s1 = Arc::clone(&store);
    let a1 = attempt.clone();
    let b1 = bolt11.clone();
    let t1 =
        tokio::spawn(async move { s1.claim_paying(&a1, "hash-a", &b1, amount).await.unwrap() });

    let s2 = Arc::clone(&store);
    let a2 = attempt.clone();
    let b2 = bolt11.clone();
    let t2 =
        tokio::spawn(async move { s2.claim_paying(&a2, "hash-a", &b2, amount).await.unwrap() });

    let (r1, r2) = tokio::join!(t1, t2);
    let outcomes = [r1.unwrap(), r2.unwrap()];
    let claimed = outcomes
        .iter()
        .filter(|o| matches!(o, ClaimOutcome::Claimed(_)))
        .count();
    let already = outcomes
        .iter()
        .filter(|o| matches!(o, ClaimOutcome::AlreadyClaimed(_)))
        .count();
    assert_eq!(claimed, 1, "exactly one Claimed");
    assert_eq!(already, 1, "exactly one AlreadyClaimed");
    assert_eq!(store.all().len(), 1);
}

#[tokio::test]
async fn second_attempt_id_claims_independently() {
    let store = InMemoryPaymentStore::new("community-a");
    let amount = Amount::from_msat(1_000);

    let first = store
        .claim_paying(
            &AttemptId::new("attempt-1"),
            "hash-1",
            &Bolt11::new("lnbc1one"),
            amount,
        )
        .await
        .unwrap();
    assert!(matches!(first, ClaimOutcome::Claimed(_)));

    let second = store
        .claim_paying(
            &AttemptId::new("attempt-2"),
            "hash-2",
            &Bolt11::new("lnbc1two"),
            amount,
        )
        .await
        .unwrap();
    assert!(matches!(second, ClaimOutcome::Claimed(_)));
    assert_eq!(store.all().len(), 2);

    // Same attempt again → AlreadyClaimed.
    let again = store
        .claim_paying(
            &AttemptId::new("attempt-1"),
            "hash-other",
            &Bolt11::new("lnbc1other"),
            amount,
        )
        .await
        .unwrap();
    match again {
        ClaimOutcome::AlreadyClaimed(r) => {
            assert_eq!(r.payment_hash, "hash-1");
            assert_eq!(r.state, PersistedPaymentState::Paying);
        }
        ClaimOutcome::Claimed(_) => panic!("second claim of attempt-1 must be AlreadyClaimed"),
    }
}

#[tokio::test]
async fn in_memory_secret_store_unchanged_assertion_works() {
    let store = InMemorySecretStore::new("community-a");
    let before = store.snapshot();
    assert!(before.is_none());

    // Failure path: no store call → unchanged.
    store.assert_unchanged_since(&before);

    let secret = StoredSecret {
        uri: "nostr+walletconnect://example".into(),
        capabilities: Capabilities::parse(["pay_invoice"]),
        lud16: None,
    };
    store.store(&secret).await.unwrap();

    // After a write, asserting against the empty snapshot must panic.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.assert_unchanged_since(&before);
    }));
    assert!(result.is_err(), "must panic when storage changed");

    // Snapshot after write matches.
    let after = store.snapshot();
    store.assert_unchanged_since(&after);
}

#[test]
fn capabilities_parse_ignores_unknown_and_intersects_13194_with_get_info() {
    let caps = Capabilities::from_link_advertisement(
        [
            "pay_invoice",
            "make_invoice",
            "lookup_invoice",
            "get_balance",
            "totally_unknown_method",
            "pay_keysend",
        ],
        [
            "pay_invoice",
            "make_invoice",
            "lookup_invoice",
            "get_balance",
            "another_unknown",
            // get_info omits pay_keysend → dropped by intersect
        ],
    );

    let expected = Capabilities::from_methods([
        WalletMethod::PayInvoice,
        WalletMethod::MakeInvoice,
        WalletMethod::LookupInvoice,
        WalletMethod::GetBalance,
    ]);
    assert_eq!(caps, expected);
    assert!(!caps.contains(WalletMethod::PayKeysend));
    assert!(!caps.contains(WalletMethod::Notifications));
}

#[tokio::test]
async fn fake_connector_succeeds_with_service_caps_and_lud16() {
    let service = Arc::new(FakeWalletService::new());
    let caps = Capabilities::parse(["pay_invoice", "make_invoice"]);
    let connector = FakeWalletConnector::new();
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service) as Arc<dyn WalletService>,
        capabilities: caps.clone(),
        lud16: Some("alice@example.com".into()),
    });

    let (svc, got_caps, lud16) = connector
        .connect("nostr+walletconnect://ok")
        .await
        .expect("connect");
    assert_eq!(got_caps, caps);
    assert_eq!(lud16.as_deref(), Some("alice@example.com"));
    // Returned service is usable (and unscripted balance defaults to None).
    assert_eq!(svc.get_balance().await.unwrap(), None);
    assert_eq!(
        connector.calls(),
        vec!["nostr+walletconnect://ok".to_string()]
    );
}

#[tokio::test]
async fn fake_connector_fails_with_invalid_uri() {
    let connector = FakeWalletConnector::new();
    connector.script(ConnectorScript::Fail(WalletError::InvalidUri));
    match connector.connect("nostr+walletconnect://abc").await {
        Err(WalletError::InvalidUri) => {}
        Err(e) => panic!("expected InvalidUri, got {e:?}"),
        Ok(_) => panic!("expected InvalidUri, got Ok"),
    }
}

#[tokio::test]
async fn fake_connector_fails_with_unreachable() {
    let connector = FakeWalletConnector::new();
    connector.script(ConnectorScript::Fail(WalletError::Unreachable));
    match connector.connect("nostr+walletconnect://relay-down").await {
        Err(WalletError::Unreachable) => {}
        Err(e) => panic!("expected Unreachable, got {e:?}"),
        Ok(_) => panic!("expected Unreachable, got Ok"),
    }
}

#[tokio::test(start_paused = true)]
async fn fake_connector_never_respond_stays_pending() {
    let connector = FakeWalletConnector::new();
    connector.script(ConnectorScript::NeverRespond);
    let result = timeout(
        TokioDuration::from_secs(60),
        connector.connect("nostr+walletconnect://hang"),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn fake_lnurl_resolver_scripts_ok() {
    let resolver = FakeLnurlResolver::new();
    resolver.script(
        "alice@example.com",
        LnurlScript::Ok(ResolvedPay {
            bolt11: Bolt11::new("lnbc1resolved"),
            payment_hash: "hh".repeat(32),
            amount: Amount::from_msat(1_000),
            min_sendable: Amount::from_msat(1),
            max_sendable: Amount::from_msat(10_000_000),
        }),
    );

    let ok = resolver
        .resolve("alice@example.com", Amount::from_msat(1_000), None)
        .await
        .unwrap();
    assert_eq!(ok.bolt11.as_str(), "lnbc1resolved");
    assert_eq!(resolver.calls().len(), 1);
}

#[tokio::test]
async fn fake_lnurl_resolver_surfaces_resolve_rejected() {
    let resolver = FakeLnurlResolver::new();
    resolver.script(
        "bob@example.com",
        LnurlScript::Fail(WalletError::ResolveRejected),
    );
    let err = resolver
        .resolve("bob@example.com", Amount::from_msat(1_000), Some("tip"))
        .await
        .unwrap_err();
    assert_eq!(err, WalletError::ResolveRejected);
}

#[tokio::test]
async fn fake_lnurl_resolver_surfaces_resolve_failed() {
    let resolver = FakeLnurlResolver::new();
    resolver.script(
        "carol@example.com",
        LnurlScript::Fail(WalletError::ResolveFailed),
    );
    let err = resolver
        .resolve("carol@example.com", Amount::from_msat(1_000), None)
        .await
        .unwrap_err();
    assert_eq!(err, WalletError::ResolveFailed);
}

#[tokio::test]
async fn recording_profile_publisher_records_kind0_field_sets() {
    let publisher = RecordingProfilePublisher::new();
    publisher
        .merge_publish(Kind0Fields {
            lud16: Some("alice@example.com".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        publisher.published(),
        vec![Kind0Fields {
            lud16: Some("alice@example.com".into()),
        }]
    );
}

#[test]
fn fake_clock_advances() {
    let clock = FakeClock::new(1_000);
    assert_eq!(clock.now_unix(), 1_000);
    clock.advance(60);
    assert_eq!(clock.now_unix(), 1_060);
    clock.set(2_000);
    assert_eq!(clock.now_unix(), 2_000);
}

#[tokio::test]
async fn payment_store_update_and_list_by_states() {
    let store = InMemoryPaymentStore::new("c1");
    let id = AttemptId::new("a1");
    store
        .claim_paying(&id, "h1", &Bolt11::new("lnbc1"), Amount::from_msat(10))
        .await
        .unwrap();
    store
        .update_state(&id, PersistedPaymentState::Unknown)
        .await
        .unwrap();
    let listed = store
        .list_by_states(&[
            PersistedPaymentState::Unknown,
            PersistedPaymentState::Paying,
        ])
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, PersistedPaymentState::Unknown);
}

#[tokio::test]
async fn make_invoice_and_lookup_scripting() {
    let wallet = FakeWalletService::new();
    wallet.script_make_invoice(MakeInvoiceScript::Fail(WalletError::Unsupported));
    wallet.script_lookup("deadbeef", InvoiceScript::Status(InvoiceStatus::Settled));
    wallet.script_lookup("deadbeef", InvoiceScript::Status(InvoiceStatus::NotFound));

    let err = wallet
        .make_invoice(Amount::from_msat(1), None)
        .await
        .unwrap_err();
    assert_eq!(err, WalletError::Unsupported);

    assert_eq!(
        wallet.lookup_invoice("deadbeef").await.unwrap(),
        InvoiceStatus::Settled
    );
    assert_eq!(
        wallet.lookup_invoice("deadbeef").await.unwrap(),
        InvoiceStatus::NotFound
    );
}

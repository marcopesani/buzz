//! Link + Receive acceptance scenarios — named after the Gherkin contract.

use crate::error::WalletError;
use crate::fakes::{
    ConnectorScript, FakeWalletConnector, FakeWalletService, InMemorySecretStore,
    MakeInvoiceScript, RecordingProfilePublisher, SeededKind0, WalletCall,
};
use crate::ports::SecretStore;
use crate::types::{Bolt11, Capabilities, Kind0Fields, ReceiveMode, StoredSecret, WalletMethod};
use crate::wallet::Wallet;
use buzz_core::payment::Amount;
use std::sync::Arc;
use std::time::Duration;

const COMMUNITY: &str = "community-a";
const VALID_URI: &str = "nostr+walletconnect://pubkey?relay=wss://relay.example&secret=abc";

fn full_capabilities() -> Capabilities {
    Capabilities::from_methods([
        WalletMethod::PayInvoice,
        WalletMethod::MakeInvoice,
        WalletMethod::LookupInvoice,
        WalletMethod::GetBalance,
    ])
}

fn harness(
    connector: Arc<FakeWalletConnector>,
    secrets: Arc<InMemorySecretStore>,
    profiles: Arc<RecordingProfilePublisher>,
) -> Wallet {
    Wallet::new(
        connector as Arc<dyn crate::ports::WalletConnector>,
        secrets as Arc<dyn SecretStore>,
        profiles as Arc<dyn crate::ports::ProfilePublisher>,
        Duration::from_secs(5),
    )
}

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
    let wallet = harness(Arc::clone(&connector), Arc::clone(&secrets), profiles);

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
    let wallet = harness(connector, Arc::clone(&secrets), profiles);

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
    // Paused clock auto-advances when all tasks sleep on the connect timeout.
    let wallet = harness(connector, Arc::clone(&secrets), profiles);

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
    let wallet = harness(connector, Arc::clone(&secrets), Arc::clone(&profiles));

    let handle = wallet.link(VALID_URI).await.expect("link");

    assert_eq!(handle.lud16.as_deref(), Some("alice@example.com"));
    assert_eq!(
        profiles.published(),
        vec![Kind0Fields {
            lud16: Some("alice@example.com".into()),
        }]
    );
    // Merge semantics: other kind:0 fields preserved.
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
    let wallet = harness(connector, Arc::clone(&secrets), Arc::clone(&profiles));

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
    // Secret still stores the connector-reported address for receive_mode.
    let stored = secrets.load().await.expect("load").expect("secret");
    assert_eq!(stored.lud16.as_deref(), Some("alice@example.com"));
}

/// Scenario: Static receive with a lightning address
#[tokio::test]
async fn static_receive_with_a_lightning_address() {
    let service = Arc::new(FakeWalletService::new());
    let connector = Arc::new(FakeWalletConnector::new());
    connector.script(ConnectorScript::Succeed {
        service: Arc::clone(&service),
        capabilities: full_capabilities(),
        lud16: Some("alice@example.com".into()),
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = harness(connector, secrets, profiles);

    wallet.link(VALID_URI).await.expect("link");

    let mode = wallet.receive_mode().await.expect("receive_mode");
    assert_eq!(mode, ReceiveMode::StaticAddress("alice@example.com".into()));
    assert_eq!(
        service.call_count(&WalletCall::MakeInvoice {
            amount: Amount::from_msat(1),
            memo: None,
        }),
        0
    );
    // Stronger: no make_invoice of any shape.
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
        service: Arc::clone(&service),
        capabilities: full_capabilities(),
        lud16: None,
    });
    let secrets = Arc::new(InMemorySecretStore::new(COMMUNITY));
    let profiles = Arc::new(RecordingProfilePublisher::new());
    let wallet = harness(connector, secrets, profiles);

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
    let wallet = harness(connector, Arc::clone(&secrets), profiles);

    wallet.link(VALID_URI).await.expect("link");

    assert_eq!(
        wallet.receive_mode().await.expect("mode"),
        ReceiveMode::Unavailable
    );
    // Wallet remains linked — secret still present.
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
    let wallet = harness(
        Arc::new(FakeWalletConnector::new()),
        secrets,
        Arc::new(RecordingProfilePublisher::new()),
    );

    assert_eq!(
        wallet.receive_mode().await.expect("mode"),
        ReceiveMode::StaticAddress("alice@example.com".into())
    );
}

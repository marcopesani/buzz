//! Joint acceptance: real `NwcWalletService` (U6) against this mock wallet (U7).
//!
//! Proves the live pay / receive / lookup round-trip over NIP-04 wire, plus
//! receive-only → `RESTRICTED` → [`WalletError::Unauthorized`].

use bitcoin::hashes::Hash;
use buzz_core::payment::{verify, Amount};
use buzz_mock_wallet::{MockWallet, MockWalletConfig, Script};
use buzz_wallet::{
    Bolt11, InvoiceStatus, NwcWalletConnector, WalletConnector, WalletError, WalletMethod,
    WalletService, WalletTimeouts,
};
use std::sync::Arc;
use std::time::Duration;

fn short_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(5),
        pay_response: Duration::from_secs(5),
    }
}

fn payment_hash_hex(bolt11: &Bolt11) -> String {
    let invoice: lightning_invoice::Bolt11Invoice = bolt11.as_str().parse().expect("decode bolt11");
    hex::encode(invoice.payment_hash().to_byte_array())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,buzz_mock_wallet=debug,nwc=debug".into()),
        )
        .with_test_writer()
        .try_init();
}

#[tokio::test]
async fn real_adapter_pay_receive_lookup_round_trip() {
    init_tracing();

    let balance_msat = 100_000_000u64;
    let wallet = MockWallet::start(MockWalletConfig {
        balance_msat,
        ..Default::default()
    })
    .await
    .expect("start mock wallet");

    tracing::info!(uri = wallet.uri(), "connecting real NwcWalletConnector");

    let connector = NwcWalletConnector::new(short_timeouts());
    let (service, caps, lud16): (Arc<dyn WalletService>, _, _) = connector
        .connect(wallet.uri())
        .await
        .expect("connect via real adapter");
    assert!(lud16.is_none());
    assert!(
        caps.contains(WalletMethod::MakeInvoice),
        "missing make_invoice: {caps:?}"
    );
    assert!(
        caps.contains(WalletMethod::LookupInvoice),
        "missing lookup_invoice: {caps:?}"
    );
    assert!(
        caps.contains(WalletMethod::PayInvoice),
        "missing pay_invoice: {caps:?}"
    );
    assert!(
        caps.contains(WalletMethod::GetBalance),
        "missing get_balance: {caps:?}"
    );

    let bal = service.get_balance().await.expect("get_balance");
    assert_eq!(bal, Some(Amount::from_msat(balance_msat)));

    let amount = Amount::from_msat(2_500);
    let bolt11 = service
        .make_invoice(amount, Some("u7-round-trip"))
        .await
        .expect("make_invoice");
    let hash = payment_hash_hex(&bolt11);
    tracing::info!(%bolt11, %hash, "minted invoice via real adapter");

    let pending = service.lookup_invoice(&hash).await.expect("lookup pending");
    assert_eq!(pending, InvoiceStatus::Pending);

    let preimage = service
        .pay_invoice(&bolt11)
        .await
        .expect("pay_invoice self-pay");
    assert!(
        verify(&preimage, &hash),
        "preimage must verify against invoice payment_hash"
    );
    tracing::info!(%preimage, %hash, "pay_invoice returned verifying preimage");

    let settled = service.lookup_invoice(&hash).await.expect("lookup settled");
    assert_eq!(settled, InvoiceStatus::Settled);

    wallet.shutdown();
}

/// Regression: wallets whose `get_info` advertises extension methods unknown
/// to rust-nostr's strict `Method` enum (real Alby behavior: `sign_message`,
/// `get_budget`) make the whole get_info response undeserializable. The
/// connector must degrade to the kind-13194 advertisement instead of
/// reporting a reachable wallet as [`WalletError::Unreachable`].
#[tokio::test]
async fn real_adapter_links_when_get_info_has_unknown_extension_methods() {
    init_tracing();

    let wallet = MockWallet::start(MockWalletConfig {
        balance_msat: 1_000_000,
        script: Script {
            get_info_extra_methods: vec!["sign_message".into(), "get_budget".into()],
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .expect("start mock with alby-like get_info extensions");

    let connector = NwcWalletConnector::new(short_timeouts());
    let (service, caps, _): (Arc<dyn WalletService>, _, _) = connector
        .connect(wallet.uri())
        .await
        .expect("connect must fall back to 13194 advertisement");

    assert!(
        caps.contains(WalletMethod::MakeInvoice) && caps.contains(WalletMethod::PayInvoice),
        "13194 fallback must carry full capabilities: {caps:?}"
    );
    let bal = service.get_balance().await.expect("get_balance");
    assert_eq!(bal, Some(Amount::from_msat(1_000_000)));

    wallet.shutdown();
}

#[tokio::test]
async fn real_adapter_receive_only_rejects_pay() {
    init_tracing();

    let wallet = MockWallet::start(MockWalletConfig {
        balance_msat: 1_000_000,
        receive_only: true,
        ..Default::default()
    })
    .await
    .expect("start receive-only mock");

    let connector = NwcWalletConnector::new(short_timeouts());
    let (service, caps, _): (Arc<dyn WalletService>, _, _) = connector
        .connect(wallet.uri())
        .await
        .expect("connect receive-only");

    assert!(
        !caps.contains(WalletMethod::PayInvoice),
        "receive-only must omit pay_invoice from capabilities: {caps:?}"
    );
    assert!(caps.contains(WalletMethod::MakeInvoice));
    assert!(caps.contains(WalletMethod::LookupInvoice));

    // Mint a payable invoice first (make_invoice still allowed), then attempt pay.
    let bolt11 = service
        .make_invoice(Amount::from_msat(100), Some("restricted-pay"))
        .await
        .expect("make_invoice still works");

    let err = service
        .pay_invoice(&bolt11)
        .await
        .expect_err("pay must fail as RESTRICTED");
    assert_eq!(
        err,
        WalletError::Unauthorized,
        "RESTRICTED must map to Unauthorized"
    );

    wallet.shutdown();
}

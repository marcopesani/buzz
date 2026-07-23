//! NWC adapter — [`NwcWalletConnector`] + [`NwcWalletService`].
//!
//! Translates rust-nostr `nwc` / NIP-47 into the wallet ports. Timeouts come
//! from injected [`WalletTimeouts`]; a pay-response timeout becomes
//! [`WalletError::Unknown`] so the use-case can park the attempt — this
//! adapter never decides Settled / Failed / Unknown lifecycle itself.

use crate::error::WalletError;
use crate::ports::{WalletConnector, WalletService};
use crate::types::{Bolt11, Capabilities, InvoiceStatus, Tx, WalletTimeouts};
use async_trait::async_trait;
use buzz_core::payment::Amount;
use nostr::nips::nip47::{
    Error as Nip47Error, ErrorCode, ListTransactionsRequest, LookupInvoiceRequest,
    MakeInvoiceRequest, NostrWalletConnectURI, PayInvoiceRequest, TransactionState,
};
use nostr::{Filter, Kind};
use nostr_relay_pool::prelude::{RelayPool, ReqExitPolicy, StreamExt};
use nwc::{Error as NwcError, NostrWalletConnectOptions, NWC};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

/// Parsed NWC URI with a redacting [`Debug`] — the secret never appears.
#[derive(Clone)]
pub struct ParsedNwcUri {
    uri: NostrWalletConnectURI,
}

impl ParsedNwcUri {
    /// Wallet service pubkey (hex).
    pub fn public_key_hex(&self) -> String {
        self.uri.public_key.to_hex()
    }

    /// Relay URLs from the connection string.
    pub fn relays(&self) -> Vec<String> {
        self.uri
            .relays
            .iter()
            .map(|r| r.as_str_without_trailing_slash().to_string())
            .collect()
    }

    /// Lightning Address from the `lud16` query param, if any.
    pub fn lud16(&self) -> Option<&str> {
        self.uri.lud16.as_deref()
    }
}

impl fmt::Debug for ParsedNwcUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedNwcUri")
            .field("public_key", &self.uri.public_key.to_hex())
            .field("relays", &self.relays())
            .field("secret", &"<redacted>")
            .field("lud16", &self.uri.lud16)
            .finish()
    }
}

/// Parse and validate a `nostr+walletconnect://` URI.
///
/// Returns [`WalletError::InvalidUri`] when the scheme, pubkey, relay, or
/// secret is missing / malformed. The returned value's [`Debug`] redacts the
/// secret.
pub fn parse_nwc_uri(uri: &str) -> Result<ParsedNwcUri, WalletError> {
    let parsed = NostrWalletConnectURI::parse(uri).map_err(|_| WalletError::InvalidUri)?;
    Ok(ParsedNwcUri { uri: parsed })
}

/// Map a NIP-47 error code onto [`WalletError`].
///
/// Pure and exhaustive — every code lands in exactly one variant. Call sites
/// do not re-branch on NWC vocabulary.
pub fn map_nip47_error_code(code: ErrorCode) -> WalletError {
    match code {
        ErrorCode::RateLimited | ErrorCode::QuotaExceeded => WalletError::QuotaExceeded,
        ErrorCode::NotImplemented => WalletError::Unsupported,
        ErrorCode::InsufficientBalance => WalletError::InsufficientBalance,
        ErrorCode::PaymentFailed => WalletError::PaymentFailed,
        ErrorCode::Unauthorized | ErrorCode::Restricted => WalletError::Unauthorized,
        // Lookup uses NotFound → InvoiceStatus; pay / other RPCs treat it as failure.
        ErrorCode::NotFound | ErrorCode::Internal | ErrorCode::Other => WalletError::PaymentFailed,
    }
}

/// Map a transport / NWC client error for connect and non-pay RPCs.
///
/// Timeouts and premature exits are [`WalletError::Unreachable`] here —
/// pay-invoice uses [`map_pay_error`] so a timeout becomes Unknown.
fn map_query_error(err: NwcError) -> WalletError {
    match err {
        NwcError::Timeout | NwcError::PrematureExit => WalletError::Unreachable,
        NwcError::NIP47(Nip47Error::InvalidURI) => WalletError::InvalidUri,
        NwcError::NIP47(Nip47Error::ErrorCode(e)) => map_nip47_error_code(e.code),
        NwcError::NIP47(_) | NwcError::Pool(_) | NwcError::Handler(_) => WalletError::Unreachable,
    }
}

/// Map a pay-invoice error — timeout / disconnect → [`WalletError::Unknown`].
fn map_pay_error(err: NwcError) -> WalletError {
    match err {
        NwcError::Timeout | NwcError::PrematureExit => WalletError::Unknown,
        other => map_query_error(other),
    }
}

/// Production [`WalletConnector`] over rust-nostr `nwc`.
pub struct NwcWalletConnector {
    timeouts: WalletTimeouts,
}

impl fmt::Debug for NwcWalletConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NwcWalletConnector")
            .field("timeouts", &self.timeouts)
            .finish()
    }
}

impl NwcWalletConnector {
    /// Build a connector with injected connect / pay timeouts.
    pub fn new(timeouts: WalletTimeouts) -> Self {
        Self { timeouts }
    }
}

#[async_trait]
impl WalletConnector for NwcWalletConnector {
    async fn connect(
        &self,
        uri: &str,
    ) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError> {
        let parsed = parse_nwc_uri(uri)?;
        let lud16 = parsed.uri.lud16.clone();
        let opts = NostrWalletConnectOptions::new().timeout(self.timeouts.connect);
        let client = NWC::with_opts(parsed.uri.clone(), opts);

        match timeout(
            self.timeouts.connect,
            probe_capabilities(&client, &parsed, self.timeouts.connect),
        )
        .await
        {
            Ok(Ok(capabilities)) => {
                let service: Arc<dyn WalletService> =
                    Arc::new(NwcWalletService::new(client, self.timeouts));
                Ok((service, capabilities, lud16))
            }
            Ok(Err(err)) => Err(err),
            Err(_) => Err(WalletError::Unreachable),
        }
    }
}

/// Fetch `13194 ∩ get_info.methods`.
async fn probe_capabilities(
    client: &NWC,
    parsed: &ParsedNwcUri,
    connect_timeout: Duration,
) -> Result<Capabilities, WalletError> {
    let info_13194 = fetch_methods_13194(parsed, connect_timeout).await?;
    let info = client.get_info().await.map_err(map_query_error)?;
    let get_info_methods: Vec<String> = info
        .methods
        .iter()
        .map(|m| m.as_str().to_string())
        .collect();
    Ok(Capabilities::from_link_advertisement(
        info_13194,
        get_info_methods,
    ))
}

/// Read the wallet's kind-13194 info event (space-separated methods).
async fn fetch_methods_13194(
    parsed: &ParsedNwcUri,
    timeout_dur: Duration,
) -> Result<Vec<String>, WalletError> {
    let pool = RelayPool::default();
    for relay in &parsed.uri.relays {
        pool.add_relay(relay, Default::default())
            .await
            .map_err(|_| WalletError::Unreachable)?;
    }
    pool.connect().await;

    let filter = Filter::new()
        .author(parsed.uri.public_key)
        .kind(Kind::WalletConnectInfo)
        .limit(1);

    let mut stream = pool
        .stream_events(filter, timeout_dur, ReqExitPolicy::WaitForEvents(1))
        .await
        .map_err(|_| WalletError::Unreachable)?;

    let event = match stream.next().await {
        Some(event) => event,
        None => {
            pool.disconnect().await;
            return Err(WalletError::Unreachable);
        }
    };
    pool.disconnect().await;

    Ok(event
        .content
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

/// Production [`WalletService`] over a live [`NWC`] client.
pub struct NwcWalletService {
    client: NWC,
    timeouts: WalletTimeouts,
}

impl fmt::Debug for NwcWalletService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never dump the inner NWC URI / secret.
        f.debug_struct("NwcWalletService")
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

impl NwcWalletService {
    /// Wrap a connected NWC client with injected RPC timeouts.
    pub fn new(client: NWC, timeouts: WalletTimeouts) -> Self {
        Self { client, timeouts }
    }

    async fn with_timeout<T, F, Fut>(&self, fut: F) -> Result<T, WalletError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, NwcError>>,
    {
        match timeout(self.timeouts.pay_response, fut()).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(map_query_error(err)),
            Err(_) => Err(WalletError::Unreachable),
        }
    }
}

#[async_trait]
impl WalletService for NwcWalletService {
    async fn get_balance(&self) -> Result<Option<Amount>, WalletError> {
        match self.with_timeout(|| self.client.get_balance()).await {
            Ok(msat) => Ok(Some(Amount::from_msat(msat))),
            Err(WalletError::Unsupported) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn make_invoice(
        &self,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<Bolt11, WalletError> {
        let request = MakeInvoiceRequest {
            amount: amount.as_msat(),
            description: memo.map(str::to_string),
            description_hash: None,
            expiry: None,
        };
        let response = self
            .with_timeout(|| self.client.make_invoice(request))
            .await?;
        Ok(Bolt11::new(response.invoice))
    }

    async fn pay_invoice(&self, bolt11: &Bolt11) -> Result<String, WalletError> {
        let request = PayInvoiceRequest::new(bolt11.as_str());
        match timeout(self.timeouts.pay_response, self.client.pay_invoice(request)).await {
            Ok(Ok(response)) => Ok(response.preimage),
            Ok(Err(err)) => Err(map_pay_error(err)),
            Err(_) => Err(WalletError::Unknown),
        }
    }

    async fn lookup_invoice(&self, payment_hash: &str) -> Result<InvoiceStatus, WalletError> {
        let request = LookupInvoiceRequest {
            payment_hash: Some(payment_hash.to_string()),
            invoice: None,
        };
        match timeout(
            self.timeouts.pay_response,
            self.client.lookup_invoice(request),
        )
        .await
        {
            Ok(Ok(response)) => Ok(invoice_status_from_lookup(&response)),
            Ok(Err(NwcError::NIP47(Nip47Error::ErrorCode(e)))) if e.code == ErrorCode::NotFound => {
                Ok(InvoiceStatus::NotFound)
            }
            Ok(Err(err)) => Err(map_query_error(err)),
            Err(_) => Err(WalletError::Unreachable),
        }
    }

    async fn list_transactions(&self) -> Result<Vec<Tx>, WalletError> {
        let response = self
            .with_timeout(|| {
                self.client
                    .list_transactions(ListTransactionsRequest::default())
            })
            .await?;
        Ok(response
            .into_iter()
            .map(|row| Tx {
                payment_hash: row.payment_hash,
                amount: Amount::from_msat(row.amount),
                memo: row.description,
                settled_at: row.settled_at.map(|t| t.as_secs()),
            })
            .collect())
    }
}

fn invoice_status_from_lookup(
    response: &nostr::nips::nip47::LookupInvoiceResponse,
) -> InvoiceStatus {
    match response.state {
        Some(TransactionState::Settled) => InvoiceStatus::Settled,
        Some(TransactionState::Pending) => InvoiceStatus::Pending,
        Some(TransactionState::Failed) => InvoiceStatus::Failed,
        Some(TransactionState::Expired) => InvoiceStatus::Expired,
        None => {
            if response.settled_at.is_some()
                || response.preimage.as_ref().is_some_and(|p| !p.is_empty())
            {
                InvoiceStatus::Settled
            } else {
                InvoiceStatus::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const VALID_PUBKEY: &str = "b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4";
    const VALID_SECRET: &str = "71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c";

    fn valid_uri(relay: &str) -> String {
        format!(
            "nostr+walletconnect://{VALID_PUBKEY}?secret={VALID_SECRET}&relay={relay}&lud16=alice%40getalby.com"
        )
    }

    #[test]
    fn uri_parse_valid_extracts_pubkey_relay_and_lud16() {
        let parsed = parse_nwc_uri(&valid_uri("wss://relay.example.com")).expect("valid uri");
        assert_eq!(parsed.public_key_hex(), VALID_PUBKEY);
        assert_eq!(parsed.relays(), vec!["wss://relay.example.com".to_string()]);
        assert_eq!(parsed.lud16(), Some("alice@getalby.com"));
    }

    #[test]
    fn uri_parse_missing_secret_is_invalid() {
        let uri = format!("nostr+walletconnect://{VALID_PUBKEY}?relay=wss://relay.example.com");
        assert_eq!(
            parse_nwc_uri(&uri).expect_err("missing secret"),
            WalletError::InvalidUri
        );
    }

    #[test]
    fn uri_parse_missing_relay_is_invalid() {
        let uri = format!("nostr+walletconnect://{VALID_PUBKEY}?secret={VALID_SECRET}");
        assert_eq!(
            parse_nwc_uri(&uri).expect_err("missing relay"),
            WalletError::InvalidUri
        );
    }

    #[test]
    fn uri_parse_wrong_scheme_is_invalid() {
        assert_eq!(
            parse_nwc_uri("https://example.com").expect_err("wrong scheme"),
            WalletError::InvalidUri
        );
    }

    #[test]
    fn uri_debug_never_contains_secret() {
        let parsed = parse_nwc_uri(&valid_uri("wss://relay.example.com")).expect("valid");
        let debug = format!("{parsed:?}");
        assert!(
            !debug.contains(VALID_SECRET),
            "Debug leaked secret: {debug}"
        );
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains(VALID_PUBKEY));
    }

    #[test]
    fn nip47_error_code_mapping_table() {
        let cases = [
            (ErrorCode::RateLimited, WalletError::QuotaExceeded),
            (ErrorCode::QuotaExceeded, WalletError::QuotaExceeded),
            (ErrorCode::NotImplemented, WalletError::Unsupported),
            (
                ErrorCode::InsufficientBalance,
                WalletError::InsufficientBalance,
            ),
            (ErrorCode::PaymentFailed, WalletError::PaymentFailed),
            (ErrorCode::Unauthorized, WalletError::Unauthorized),
            (ErrorCode::Restricted, WalletError::Unauthorized),
            (ErrorCode::NotFound, WalletError::PaymentFailed),
            (ErrorCode::Internal, WalletError::PaymentFailed),
            (ErrorCode::Other, WalletError::PaymentFailed),
        ];
        for (code, expected) in cases {
            assert_eq!(
                map_nip47_error_code(code),
                expected,
                "code {code:?} mapped wrong"
            );
        }
    }

    #[tokio::test]
    async fn connect_timeout_against_blackhole_is_unreachable() {
        // TEST-NET-1 documentation address — not routable on a real network.
        let uri = valid_uri("ws://192.0.2.1:9");
        let connector = NwcWalletConnector::new(WalletTimeouts {
            connect: Duration::from_millis(80),
            pay_response: Duration::from_millis(80),
        });
        let started = std::time::Instant::now();
        let err = match connector.connect(&uri).await {
            Ok(_) => panic!("must time out"),
            Err(err) => err,
        };
        assert_eq!(err, WalletError::Unreachable);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "connect timeout took {:?}",
            started.elapsed()
        );
    }
}

//! HTTP LNURL-pay resolver ([`HttpLnurlResolver`]).
//!
//! Two GETs (LUD-16 well-known → callback) with reqwest. Bounds, HTTPS, and
//! invoice-amount checks live here so a fake at the port boundary cannot
//! observe them. The pay path still re-validates the bolt11 in the use-case.

use crate::error::WalletError;
use crate::ports::LnurlResolver;
use crate::types::{Bolt11, ResolvedPay};
use async_trait::async_trait;
use buzz_core::payment::Amount;
use lightning_invoice::Bolt11Invoice;
use serde::Deserialize;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;
use url::Url;

/// Production [`LnurlResolver`] over HTTPS LNURL-pay.
pub struct HttpLnurlResolver {
    client: reqwest::Client,
    /// When true, loopback `http://` is accepted (local capturing fakes only).
    allow_insecure_for_tests: bool,
}

impl fmt::Debug for HttpLnurlResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpLnurlResolver")
            .field("allow_insecure_for_tests", &self.allow_insecure_for_tests)
            .finish_non_exhaustive()
    }
}

impl HttpLnurlResolver {
    /// Production constructor — HTTPS only for well-known and callback URLs.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// Test constructor — permits `http://` only for loopback hosts.
    ///
    /// Non-loopback `http://` callbacks are still rejected, so the production
    /// HTTPS rule is exercised against a capturing local fake.
    pub fn allow_insecure_for_tests() -> Self {
        Self::build(true)
    }

    fn build(allow_insecure_for_tests: bool) -> Self {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
        {
            Ok(client) => client,
            Err(_) => reqwest::Client::new(),
        };
        Self {
            client,
            allow_insecure_for_tests,
        }
    }
}

impl Default for HttpLnurlResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LnurlResolver for HttpLnurlResolver {
    async fn resolve(
        &self,
        lud16: &str,
        amount: Amount,
        memo: Option<&str>,
    ) -> Result<ResolvedPay, WalletError> {
        let well_known = lud16_to_well_known(lud16, self.allow_insecure_for_tests)?;
        let metadata = self.fetch_pay_request(&well_known).await?;

        ensure_https_url(&metadata.callback, self.allow_insecure_for_tests)?;
        ensure_amount_within_bounds(amount, metadata.min_sendable, metadata.max_sendable)?;

        let callback_url = build_callback_url(&metadata.callback, amount, memo)?;
        let invoice = self.fetch_invoice(&callback_url).await?;
        ensure_invoice_amount_matches(&invoice.pr, amount)?;

        let payment_hash = payment_hash_hex(&invoice.pr)?;
        Ok(ResolvedPay {
            bolt11: Bolt11::new(invoice.pr),
            payment_hash,
            amount,
            min_sendable: Amount::from_msat(metadata.min_sendable),
            max_sendable: Amount::from_msat(metadata.max_sendable),
        })
    }
}

#[derive(Debug, Deserialize)]
struct LnurlPayRequest {
    callback: String,
    #[serde(rename = "minSendable")]
    min_sendable: u64,
    #[serde(rename = "maxSendable")]
    max_sendable: u64,
}

#[derive(Debug, Deserialize)]
struct LnurlPayInvoice {
    #[serde(default)]
    pr: String,
}

impl HttpLnurlResolver {
    async fn fetch_pay_request(&self, url: &str) -> Result<LnurlPayRequest, WalletError> {
        let value: serde_json::Value = self.get_json(url).await?;
        reject_lnurl_error(&value)?;
        serde_json::from_value(value).map_err(|_| WalletError::ResolveFailed)
    }

    async fn fetch_invoice(&self, url: &str) -> Result<LnurlPayInvoice, WalletError> {
        let value: serde_json::Value = self.get_json(url).await?;
        reject_lnurl_error(&value)?;
        let invoice: LnurlPayInvoice =
            serde_json::from_value(value).map_err(|_| WalletError::ResolveFailed)?;
        if invoice.pr.is_empty() {
            return Err(WalletError::ResolveRejected);
        }
        Ok(invoice)
    }

    async fn get_json(&self, url: &str) -> Result<serde_json::Value, WalletError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| WalletError::ResolveFailed)?;
        if !response.status().is_success() {
            return Err(WalletError::ResolveFailed);
        }
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|_| WalletError::ResolveFailed)
    }
}

fn reject_lnurl_error(value: &serde_json::Value) -> Result<(), WalletError> {
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if status.eq_ignore_ascii_case("ERROR") {
        return Err(WalletError::ResolveFailed);
    }
    Ok(())
}

/// `user@domain` → `{http|https}://domain/.well-known/lnurlp/user`.
fn lud16_to_well_known(lud16: &str, allow_insecure: bool) -> Result<String, WalletError> {
    let (user, host) = split_lud16(lud16)?;
    if user.is_empty() || host.is_empty() {
        return Err(WalletError::ResolveRejected);
    }
    let scheme = if allow_insecure && is_loopback_host(Some(host)) {
        "http"
    } else {
        "https"
    };
    // Encode user for path safety (bech32-ish local parts are already safe).
    let user_enc = urlencoding_lightweight(user);
    Ok(format!("{scheme}://{host}/.well-known/lnurlp/{user_enc}"))
}

fn split_lud16(lud16: &str) -> Result<(&str, &str), WalletError> {
    let (user, host) = lud16.rsplit_once('@').ok_or(WalletError::ResolveRejected)?;
    if user.contains('@') {
        return Err(WalletError::ResolveRejected);
    }
    Ok((user, host))
}

fn ensure_https_url(raw: &str, allow_insecure_loopback: bool) -> Result<(), WalletError> {
    let url = Url::parse(raw).map_err(|_| WalletError::ResolveRejected)?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_insecure_loopback && is_loopback_host(url.host_str()) => Ok(()),
        _ => Err(WalletError::ResolveRejected),
    }
}

fn is_loopback_host(host: Option<&str>) -> bool {
    match host {
        Some("localhost") | Some("127.0.0.1") | Some("::1") => true,
        Some(h) => h.starts_with("127."),
        None => false,
    }
}

fn ensure_amount_within_bounds(
    amount: Amount,
    min_sendable: u64,
    max_sendable: u64,
) -> Result<(), WalletError> {
    let msat = amount.as_msat();
    if msat < min_sendable || msat > max_sendable {
        return Err(WalletError::ResolveRejected);
    }
    Ok(())
}

fn build_callback_url(
    callback: &str,
    amount: Amount,
    memo: Option<&str>,
) -> Result<String, WalletError> {
    let mut url = Url::parse(callback).map_err(|_| WalletError::ResolveRejected)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("amount", &amount.as_msat().to_string());
        if let Some(comment) = memo {
            if !comment.is_empty() {
                pairs.append_pair("comment", comment);
            }
        }
    }
    Ok(url.to_string())
}

fn ensure_invoice_amount_matches(bolt11: &str, requested: Amount) -> Result<(), WalletError> {
    let invoice = Bolt11Invoice::from_str(bolt11).map_err(|_| WalletError::ResolveRejected)?;
    let amount_msat = invoice
        .amount_milli_satoshis()
        .ok_or(WalletError::ResolveRejected)?;
    if amount_msat != requested.as_msat() {
        return Err(WalletError::ResolveRejected);
    }
    Ok(())
}

fn payment_hash_hex(bolt11: &str) -> Result<String, WalletError> {
    use bitcoin::hashes::Hash;
    let invoice = Bolt11Invoice::from_str(bolt11).map_err(|_| WalletError::ResolveRejected)?;
    Ok(hex::encode(invoice.payment_hash().to_byte_array()))
}

/// Minimal path-segment encode — avoids a dedicated urlencoding crate.
fn urlencoding_lightweight(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mint_bolt11_for_amount;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    #[derive(Debug, Clone)]
    struct CapturedGet {
        path_and_query: String,
    }

    /// Capturing HTTP/1.1 fake — records each request line, serves canned JSON.
    ///
    /// `make_responses` receives the base URL (`http://127.0.0.1:port`) so
    /// callbacks can point back at the same listener.
    async fn spawn_capturing_http(
        make_responses: impl FnOnce(&str) -> Vec<serde_json::Value>,
    ) -> (String, Arc<Mutex<Vec<CapturedGet>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let responses = make_responses(&base);
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let captures: Arc<Mutex<Vec<CapturedGet>>> = Arc::new(Mutex::new(Vec::new()));
        let captures_clone = captures.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let queue = queue.clone();
                let captures = captures_clone.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                        if buf.len() > 1_000_000 {
                            return;
                        }
                    }
                    let header_str = String::from_utf8_lossy(&buf);
                    let request_line = header_str.lines().next().unwrap_or("");
                    let path_and_query = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("")
                        .to_string();
                    captures.lock().await.push(CapturedGet { path_and_query });

                    let body =
                        queue.lock().await.pop_front().unwrap_or_else(
                            || json!({ "status": "ERROR", "reason": "no response" }),
                        );
                    let body_s = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body_s.len(),
                        body_s
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (base, captures)
    }

    fn host_port(base: &str) -> String {
        base.trim_start_matches("http://").to_string()
    }

    #[tokio::test]
    async fn happy_path_lud16_captures_amount_and_returns_bolt11() {
        let amount_msat = 25_000u64;
        let minted = mint_bolt11_for_amount(amount_msat, 0x11, 1_700_000_000);
        let bolt11 = minted.bolt11.as_str().to_string();
        let (base, captures) = spawn_capturing_http(|base| {
            vec![
                json!({
                    "callback": format!("{base}/lnurl-pay"),
                    "minSendable": 1_000,
                    "maxSendable": 100_000,
                    "metadata": "[[\"text/plain\",\"pay bob\"]]",
                    "tag": "payRequest"
                }),
                json!({ "pr": bolt11, "routes": [] }),
            ]
        })
        .await;

        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let resolved = resolver
            .resolve(&lud16, Amount::from_msat(amount_msat), Some("thanks"))
            .await
            .expect("resolve");

        assert_eq!(resolved.amount, Amount::from_msat(amount_msat));
        assert_eq!(resolved.bolt11.as_str(), minted.bolt11.as_str());
        assert_eq!(resolved.payment_hash, minted.payment_hash_hex);
        assert_eq!(resolved.min_sendable, Amount::from_msat(1_000));
        assert_eq!(resolved.max_sendable, Amount::from_msat(100_000));

        let log = captures.lock().await;
        assert_eq!(log.len(), 2);
        assert!(
            log[0].path_and_query.starts_with("/.well-known/lnurlp/bob"),
            "well-known path: {}",
            log[0].path_and_query
        );
        assert!(
            log[1].path_and_query.contains("amount=25000"),
            "callback must carry amount msat: {}",
            log[1].path_and_query
        );
        assert!(log[1].path_and_query.contains("comment=thanks"));
    }

    #[tokio::test]
    async fn non_https_callback_is_rejected_and_no_callback_request() {
        let (base, captures) = spawn_capturing_http(|_| {
            vec![json!({
                "callback": "http://evil.example.com/pay",
                "minSendable": 1_000,
                "maxSendable": 100_000,
                "metadata": "[[\"text/plain\",\"x\"]]",
                "tag": "payRequest"
            })]
        })
        .await;

        // allow_insecure only for loopback well-known; non-loopback http callback
        // exercises the production HTTPS rejection rule.
        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let err = resolver
            .resolve(&lud16, Amount::from_msat(5_000), None)
            .await
            .expect_err("http callback must be rejected");
        assert_eq!(err, WalletError::ResolveRejected);

        let log = captures.lock().await;
        assert_eq!(log.len(), 1, "callback must not be contacted: {log:?}");
        assert!(log[0].path_and_query.starts_with("/.well-known/lnurlp/bob"));
    }

    #[tokio::test]
    async fn amount_below_min_sendable_rejected_before_callback() {
        let (base, captures) = spawn_capturing_http(|base| {
            vec![json!({
                "callback": format!("{base}/lnurl-pay"),
                "minSendable": 1_000,
                "maxSendable": 10_000,
                "metadata": "[[\"text/plain\",\"x\"]]",
                "tag": "payRequest"
            })]
        })
        .await;

        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let err = resolver
            .resolve(&lud16, Amount::from_msat(500), None)
            .await
            .expect_err("below min");
        assert_eq!(err, WalletError::ResolveRejected);
        assert_eq!(captures.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn amount_above_max_sendable_rejected_before_callback() {
        let (base, captures) = spawn_capturing_http(|base| {
            vec![json!({
                "callback": format!("{base}/lnurl-pay"),
                "minSendable": 1_000,
                "maxSendable": 10_000,
                "metadata": "[[\"text/plain\",\"x\"]]",
                "tag": "payRequest"
            })]
        })
        .await;

        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let err = resolver
            .resolve(&lud16, Amount::from_msat(25_000), None)
            .await
            .expect_err("above max");
        assert_eq!(err, WalletError::ResolveRejected);
        assert_eq!(captures.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn callback_invoice_amount_mismatch_is_rejected() {
        let minted = mint_bolt11_for_amount(24_000, 0x22, 1_700_000_000);
        let bolt11 = minted.bolt11.as_str().to_string();
        let (base, captures) = spawn_capturing_http(|base| {
            vec![
                json!({
                    "callback": format!("{base}/lnurl-pay"),
                    "minSendable": 1_000,
                    "maxSendable": 100_000,
                    "metadata": "[[\"text/plain\",\"x\"]]",
                    "tag": "payRequest"
                }),
                json!({ "pr": bolt11, "routes": [] }),
            ]
        })
        .await;

        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let err = resolver
            .resolve(&lud16, Amount::from_msat(25_000), None)
            .await
            .expect_err("amount mismatch");
        assert_eq!(err, WalletError::ResolveRejected);
        assert_eq!(captures.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn lnurl_error_json_maps_to_resolve_failed() {
        let (base, _captures) = spawn_capturing_http(|_| {
            vec![json!({
                "status": "ERROR",
                "reason": "nope"
            })]
        })
        .await;

        let resolver = HttpLnurlResolver::allow_insecure_for_tests();
        let lud16 = format!("bob@{}", host_port(&base));
        let err = resolver
            .resolve(&lud16, Amount::from_msat(5_000), None)
            .await
            .expect_err("ERROR status");
        assert_eq!(err, WalletError::ResolveFailed);
    }

    #[test]
    fn production_constructor_rejects_http_callback() {
        assert_eq!(
            ensure_https_url("http://example.com/pay", false),
            Err(WalletError::ResolveRejected)
        );
        assert!(ensure_https_url("https://example.com/pay", false).is_ok());
    }
}

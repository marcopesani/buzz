//! `buzz wallet` — thin CLI shell over the `buzz-wallet` facade.
//!
//! Parsing, secret I/O, JSON printing, and exit-code mapping live here.
//! Payment logic stays in `buzz-wallet`. The NWC secret never appears in
//! stdout, stderr, or error messages.

mod storage;

use std::io::{self, IsTerminal, Read};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use buzz_core::kind::KIND_PAYMENT_REQUEST;
use buzz_core::payment::{Amount, PaymentRequest, PaymentTarget};
use buzz_wallet::{
    AttemptKey, Bolt11, ConfirmHandle, HttpLnurlResolver, IncomingStatus, NwcWalletConnector,
    ProfilePublisher, ReceiveMode, SendOutcome, SendTarget, SystemClock, Wallet, WalletConnector,
    WalletError, WalletMethod, WalletTimeouts,
};
use lightning_invoice::Bolt11Invoice;
use nostr::EventId;
use uuid::Uuid;

use crate::client::{normalize_write_response, BuzzClient};
use crate::error::CliError;
use crate::validate::validate_hex64;
use crate::WalletCmd;

pub use storage::{
    payments_dir, resolve_nwc_uri, resolve_nwc_uri_status, secret_file_path, FileSecretStore,
    JsonPaymentStore, NwcUriResolution, ENV_NWC_URI,
};

/// Default connect / pay timeouts for one-shot CLI processes.
fn default_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(15),
        pay_response: Duration::from_secs(60),
    }
}

/// Map [`WalletError`] → [`CliError`] without embedding secret material.
///
/// `Unsupported` / `Unauthorized` → auth (exit 3): the wallet refused the
/// method (receive-only / revoked). That matches the agent "cannot spend"
/// acceptance criterion.
pub fn map_wallet_error(err: WalletError) -> CliError {
    match err {
        WalletError::InvalidUri => CliError::Usage("invalid NWC URI".into()),
        WalletError::Unauthorized | WalletError::Unsupported => CliError::Auth(err.to_string()),
        WalletError::SecretUnavailable => {
            CliError::Usage("NWC secret unavailable (link a wallet first)".into())
        }
        WalletError::ResolveRejected => {
            CliError::Usage("invoice rejected (amount mismatch, amountless, or expired)".into())
        }
        WalletError::InsufficientBalance
        | WalletError::PaymentFailed
        | WalletError::QuotaExceeded => CliError::Other(err.to_string()),
        WalletError::Unreachable | WalletError::ResolveFailed | WalletError::Unknown => {
            CliError::Other(err.to_string())
        }
    }
}

/// No-op profile publisher injected into the facade.
///
/// Kind:0 lud16 publish is a driver concern — [`maybe_publish_lud16`] runs
/// after `link` when a [`BuzzClient`] is available (avoids tying
/// `Arc<dyn ProfilePublisher>` to a non-`'static` client borrow).
struct NoopProfilePublisher;

#[async_trait]
impl ProfilePublisher for NoopProfilePublisher {
    async fn current_lud16(&self) -> Result<Option<String>, WalletError> {
        Ok(None)
    }

    async fn merge_publish(&self, _fields: buzz_wallet::Kind0Fields) -> Result<(), WalletError> {
        Ok(())
    }
}

/// Merge-publish `lud16` onto kind:0 when the user already has a Buzz identity.
///
/// Skips when the existing kind:0 already carries a *different* address
/// (same rule as `Wallet::link`).
async fn maybe_publish_lud16(client: &BuzzClient, lud16: &str) -> Result<(), CliError> {
    let current = fetch_kind0_map(client).await?;
    if let Some(existing) = current.get("lud16").and_then(|v| v.as_str()) {
        if !existing.is_empty() && existing != lud16 {
            return Ok(());
        }
    }
    let display_name = current
        .get("display_name")
        .or_else(|| current.get("name"))
        .and_then(|v| v.as_str());
    let picture = current.get("picture").and_then(|v| v.as_str());
    let about = current.get("about").and_then(|v| v.as_str());
    let nip05 = current.get("nip05").and_then(|v| v.as_str());
    let builder = buzz_sdk::build_profile(display_name, None, picture, about, nip05, Some(lud16))
        .map_err(|e| CliError::Other(format!("build_profile failed: {e}")))?;
    let event = client.sign_event(builder)?;
    let _ = client.submit_event(event).await?;
    Ok(())
}

async fn fetch_kind0_map(
    client: &BuzzClient,
) -> Result<serde_json::Map<String, serde_json::Value>, CliError> {
    let my_pk = client.keys().public_key().to_hex();
    let filter = serde_json::json!({
        "kinds": [0],
        "authors": [my_pk],
        "limit": 1
    });
    let raw = client.query(&filter).await?;
    let events: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("failed to parse profile query: {e}")))?;
    let Some(arr) = events.as_array() else {
        return Ok(serde_json::Map::new());
    };
    let Some(event) = arr.first() else {
        return Ok(serde_json::Map::new());
    };
    let content_str = event
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("{}");
    let content: serde_json::Value = serde_json::from_str(content_str).unwrap_or_default();
    Ok(content.as_object().cloned().unwrap_or_default())
}

/// Context for wallet dispatch (relay optional for NWC-only verbs).
pub struct WalletCtx<'a> {
    /// Normalized Buzz relay URL (community boundary for payment store).
    pub relay_url: &'a str,
    /// Optional Buzz client for request / receipt / profile publish.
    pub client: Option<&'a BuzzClient>,
}

/// Dispatch `buzz wallet <subcommand>`.
pub async fn dispatch(cmd: WalletCmd, ctx: WalletCtx<'_>) -> Result<(), CliError> {
    match cmd {
        WalletCmd::Link { uri } => cmd_link(uri.as_deref(), &ctx).await,
        WalletCmd::Status => cmd_status(&ctx).await,
        WalletCmd::Balance => cmd_balance(&ctx).await,
        WalletCmd::Receive {
            amount_msat,
            description,
        } => cmd_receive(&ctx, amount_msat, description.as_deref()).await,
        WalletCmd::Request {
            channel,
            amount_msat,
            description,
        } => cmd_request(&ctx, &channel, amount_msat, description.as_deref()).await,
        WalletCmd::Pay {
            bolt11,
            request,
            channel,
            yes,
        } => {
            cmd_pay(
                &ctx,
                bolt11.as_deref(),
                request.as_deref(),
                channel.as_deref(),
                yes,
            )
            .await
        }
        WalletCmd::Check { request, channel } => cmd_check(&ctx, &request, &channel).await,
        WalletCmd::Reconcile => cmd_reconcile(&ctx).await,
    }
}

/// Read the link URI from flag, env, or stdin — never echo it.
fn read_link_uri(uri_flag: Option<&str>) -> Result<String, CliError> {
    if let Some(u) = uri_flag {
        if u == "-" {
            return read_uri_from_stdin();
        }
        let trimmed = u.trim().to_string();
        if trimmed.is_empty() {
            return Err(CliError::Usage("NWC URI is empty".into()));
        }
        return Ok(trimmed);
    }
    if let Ok(env_uri) = std::env::var(ENV_NWC_URI) {
        let trimmed = env_uri.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    if !io::stdin().is_terminal() {
        return read_uri_from_stdin();
    }
    Err(CliError::Usage(
        "NWC URI required (--uri, BUZZ_NWC_URI, or stdin)".into(),
    ))
}

fn read_uri_from_stdin() -> Result<String, CliError> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| CliError::Usage(format!("failed to read NWC URI from stdin: {e}")))?;
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        return Err(CliError::Usage("NWC URI is empty".into()));
    }
    Ok(trimmed)
}

fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

fn receive_mode_json(mode: &ReceiveMode) -> serde_json::Value {
    match mode {
        ReceiveMode::StaticAddress(addr) => serde_json::json!({
            "mode": "static_address",
            "lud16": addr,
        }),
        ReceiveMode::Interactive => serde_json::json!({ "mode": "interactive" }),
        ReceiveMode::Unavailable => serde_json::json!({ "mode": "unavailable" }),
    }
}

fn capabilities_strings(caps: &buzz_wallet::Capabilities) -> Vec<String> {
    caps.iter().map(|m| m.as_str().to_string()).collect()
}

/// Build a connected [`Wallet`] facade for this process.
async fn open_wallet(ctx: &WalletCtx<'_>, uri: &str, reconcile: bool) -> Result<Wallet, CliError> {
    let timeouts = default_timeouts();
    let connector = Arc::new(NwcWalletConnector::new(timeouts));
    let secrets = Arc::new(FileSecretStore::new(secret_file_path()?));
    let store = Arc::new(JsonPaymentStore::open(payments_dir()?, ctx.relay_url)?);
    let resolver = Arc::new(HttpLnurlResolver::new());
    let clock = Arc::new(SystemClock);
    let profiles: Arc<dyn ProfilePublisher> = Arc::new(NoopProfilePublisher);

    let wallet = Wallet::new(
        connector, secrets, profiles, resolver, store, clock, timeouts,
    );
    wallet.link(uri).await.map_err(map_wallet_error)?;
    if reconcile {
        // Best-effort: a fresh link with an empty store is a no-op drain.
        let _ = wallet.reconcile().await;
    }
    Ok(wallet)
}

async fn cmd_link(uri_flag: Option<&str>, ctx: &WalletCtx<'_>) -> Result<(), CliError> {
    let uri = read_link_uri(uri_flag)?;
    let wallet = open_wallet(ctx, &uri, false).await?;
    let mode = wallet.receive_mode().await.map_err(map_wallet_error)?;
    let secret = FileSecretStore::new(secret_file_path()?)
        .load_blocking()?
        .ok_or_else(|| CliError::Other("NWC secret missing after link".into()))?;
    if let (Some(client), Some(lud16)) = (ctx.client, secret.lud16.as_deref()) {
        // Best-effort: NWC link succeeds even if Buzz profile publish fails.
        let _ = maybe_publish_lud16(client, lud16).await;
    }
    print_json(&serde_json::json!({
        "linked": true,
        "capabilities": capabilities_strings(&secret.capabilities),
        "receive_mode": receive_mode_json(&mode),
        "lud16": secret.lud16,
    }));
    Ok(())
}

async fn cmd_status(ctx: &WalletCtx<'_>) -> Result<(), CliError> {
    // Classify once: NotLinked → linked:false; permission/I/O errors propagate.
    let uri = match resolve_nwc_uri_status()? {
        NwcUriResolution::NotLinked => {
            print_json(&serde_json::json!({
                "linked": false,
                "capabilities": [],
                "receive_mode": { "mode": "unavailable" },
            }));
            return Ok(());
        }
        NwcUriResolution::Uri(u) => u,
    };
    let wallet = open_wallet(ctx, &uri, true).await?;
    let mode = wallet.receive_mode().await.map_err(map_wallet_error)?;
    let secret = FileSecretStore::new(secret_file_path()?)
        .load_blocking()?
        .ok_or_else(|| CliError::Usage("no wallet linked".into()))?;
    print_json(&serde_json::json!({
        "linked": true,
        "capabilities": capabilities_strings(&secret.capabilities),
        "receive_mode": receive_mode_json(&mode),
        "lud16": secret.lud16,
    }));
    Ok(())
}

async fn cmd_balance(ctx: &WalletCtx<'_>) -> Result<(), CliError> {
    let uri = resolve_nwc_uri()?;
    // Reconnect via facade (persists capabilities); balance is a service RPC.
    let _wallet = open_wallet(ctx, &uri, false).await?;
    let timeouts = default_timeouts();
    let connector = NwcWalletConnector::new(timeouts);
    let (service, caps, _) = connector.connect(&uri).await.map_err(map_wallet_error)?;
    if !caps.contains(WalletMethod::GetBalance) {
        print_json(&serde_json::json!({ "balance_msat": null }));
        return Ok(());
    }
    let balance = service.get_balance().await.map_err(map_wallet_error)?;
    print_json(&serde_json::json!({
        "balance_msat": balance.map(|a| a.as_msat()),
    }));
    Ok(())
}

async fn cmd_receive(
    ctx: &WalletCtx<'_>,
    amount_msat: u64,
    description: Option<&str>,
) -> Result<(), CliError> {
    if amount_msat == 0 {
        return Err(CliError::Usage(
            "--amount-msat must be greater than zero".into(),
        ));
    }
    let uri = resolve_nwc_uri()?;
    let wallet = open_wallet(ctx, &uri, false).await?;
    let amount = Amount::from_msat(amount_msat);
    let bolt11 = wallet
        .receive(amount, description)
        .await
        .map_err(map_wallet_error)?;
    let decoded = buzz_wallet::decode_bolt11(&bolt11).map_err(map_wallet_error)?;
    print_json(&serde_json::json!({
        "bolt11": bolt11.as_str(),
        "payment_hash": decoded.payment_hash_hex,
        "amount_msat": amount_msat,
        "expires_at_unix": decoded.expires_at_unix,
    }));
    Ok(())
}

async fn cmd_request(
    ctx: &WalletCtx<'_>,
    channel: &str,
    amount_msat: u64,
    description: Option<&str>,
) -> Result<(), CliError> {
    let client = ctx.client.ok_or_else(|| {
        CliError::Auth("BUZZ_PRIVATE_KEY is required for buzz wallet request".into())
    })?;
    if amount_msat == 0 {
        return Err(CliError::Usage(
            "--amount-msat must be greater than zero".into(),
        ));
    }
    let channel_id = Uuid::parse_str(channel)
        .map_err(|_| CliError::Usage(format!("invalid UUID: {channel}")))?;
    let amount = Amount::from_msat(amount_msat);

    // Prefer an embedded bolt11 when a wallet is linked (confirmable request).
    // Permission/I/O errors from resolution must not look like "not linked".
    let (bolt11, expiry) = match resolve_nwc_uri_status()? {
        NwcUriResolution::Uri(uri) => {
            let wallet = open_wallet(ctx, &uri, false).await?;
            let invoice = wallet
                .receive(amount, description)
                .await
                .map_err(map_wallet_error)?;
            let decoded = decode_invoice(invoice.as_str())?;
            let expiry = decoded.expires_at().map(|t| t.as_secs());
            (Some(invoice.into_inner()), expiry)
        }
        NwcUriResolution::NotLinked => {
            return Err(CliError::Usage(
                "wallet request requires a linked wallet to embed a bolt11 (confirmable)".into(),
            ));
        }
    };

    let payee = client.keys().public_key().to_hex();
    let event = build_and_sign_payment_request(
        client,
        channel_id,
        amount_msat,
        &payee,
        bolt11.as_deref(),
        description,
        expiry,
    )?;
    let resp = client.submit_event(event).await?;
    println!("{}", normalize_write_response(&resp));
    Ok(())
}

/// Build + sign a kind-40009 the same way `request` does (self-payee default).
///
/// Extracted so the command-seam regression test can assert the signed event
/// retains the `p` tag when payee == author.
fn build_and_sign_payment_request(
    client: &BuzzClient,
    channel_id: Uuid,
    amount_msat: u64,
    payee: &str,
    bolt11: Option<&str>,
    description: Option<&str>,
    expiry: Option<u64>,
) -> Result<nostr::Event, CliError> {
    let builder = buzz_sdk::build_payment_request(
        channel_id,
        amount_msat,
        payee,
        bolt11,
        None,
        description,
        expiry,
    )
    .map_err(|e| CliError::Usage(format!("build_payment_request failed: {e}")))?;
    client.sign_event(builder)
}

async fn cmd_pay(
    ctx: &WalletCtx<'_>,
    bolt11: Option<&str>,
    request: Option<&str>,
    channel: Option<&str>,
    yes: bool,
) -> Result<(), CliError> {
    match (bolt11, request, channel) {
        (Some(inv), None, None) => pay_bolt11(ctx, inv, yes).await,
        (None, Some(event_id), Some(ch)) => pay_request(ctx, event_id, ch, yes).await,
        (None, Some(_), None) | (None, None, Some(_)) => Err(CliError::Usage(
            "pay --request requires --channel (and vice versa)".into(),
        )),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(CliError::Usage(
            "pay: use either --bolt11 or --request/--channel, not both".into(),
        )),
        (None, None, None) => Err(CliError::Usage(
            "pay requires --bolt11 <invoice> or --request <id> --channel <id>".into(),
        )),
    }
}

async fn pay_bolt11(ctx: &WalletCtx<'_>, bolt11: &str, yes: bool) -> Result<(), CliError> {
    let uri = resolve_nwc_uri()?;
    let wallet = open_wallet(ctx, &uri, true).await?;
    let invoice = decode_invoice(bolt11)?;
    let amount_msat = invoice
        .amount_milli_satoshis()
        .ok_or_else(|| CliError::Usage("bolt11 invoice is amountless".into()))?;
    let amount = Amount::from_msat(amount_msat);
    let handle = wallet
        .prepare_send(
            AttemptKey::Standalone,
            SendTarget::Bolt11(Bolt11::new(bolt11)),
            amount,
            None,
        )
        .await
        .map_err(map_wallet_error)?;
    if !yes {
        print_quote(&handle);
        wallet.cancel(handle);
        return Ok(());
    }
    confirm_and_print(&wallet, &handle).await
}

async fn pay_request(
    ctx: &WalletCtx<'_>,
    event_id: &str,
    channel: &str,
    yes: bool,
) -> Result<(), CliError> {
    let client = ctx.client.ok_or_else(|| {
        CliError::Auth("BUZZ_PRIVATE_KEY is required for buzz wallet pay --request".into())
    })?;
    validate_hex64(event_id)?;
    let channel_id = Uuid::parse_str(channel)
        .map_err(|_| CliError::Usage(format!("invalid UUID: {channel}")))?;
    let (request, request_event_id) = fetch_payment_request(client, event_id, &channel_id).await?;

    let bolt11 = match &request.target {
        PaymentTarget::Bolt11(b) | PaymentTarget::Both { bolt11: b, .. } => b.clone(),
        PaymentTarget::Lud16(_) => {
            return Err(CliError::Usage(
                "request carries only lud16; resolve externally or use a bolt11 request".into(),
            ));
        }
    };

    let uri = resolve_nwc_uri()?;
    let wallet = open_wallet(ctx, &uri, true).await?;
    let handle = wallet
        .prepare_send(
            AttemptKey::PayRequest {
                event_id: event_id.to_string(),
            },
            SendTarget::Bolt11(Bolt11::new(bolt11)),
            request.amount,
            request.memo.as_deref(),
        )
        .await
        .map_err(map_wallet_error)?;

    if !yes {
        print_quote(&handle);
        wallet.cancel(handle);
        return Ok(());
    }

    let payment_hash = handle.payment_hash().to_string();
    let amount_msat = handle.amount().as_msat();
    let outcome = wallet.confirm(&handle).await.map_err(map_wallet_error)?;
    match outcome {
        SendOutcome::Settled { preimage } => {
            let builder = buzz_sdk::build_payment_receipt(
                channel_id,
                request_event_id,
                &payment_hash,
                &preimage,
                amount_msat,
            )
            .map_err(|e| CliError::Other(format!("build_payment_receipt failed: {e}")))?;
            let event = client.sign_event(builder)?;
            let resp = client.submit_event(event).await?;
            let mut out: serde_json::Value = serde_json::from_str(&normalize_write_response(&resp))
                .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = out.as_object_mut() {
                obj.insert("paid".into(), serde_json::json!(true));
                obj.insert("preimage".into(), serde_json::json!(preimage));
                obj.insert("payment_hash".into(), serde_json::json!(payment_hash));
                obj.insert("amount_msat".into(), serde_json::json!(amount_msat));
            }
            print_json(&out);
            Ok(())
        }
        other => map_send_outcome(other),
    }
}

fn print_quote(handle: &ConfirmHandle) {
    print_json(&serde_json::json!({
        "quote": true,
        "confirmed": false,
        "bolt11": handle.bolt11().as_str(),
        "payment_hash": handle.payment_hash(),
        "amount_msat": handle.amount().as_msat(),
        "expires_at": handle.expires_at_unix(),
    }));
}

async fn confirm_and_print(wallet: &Wallet, handle: &ConfirmHandle) -> Result<(), CliError> {
    let payment_hash = handle.payment_hash().to_string();
    let amount_msat = handle.amount().as_msat();
    let outcome = wallet.confirm(handle).await.map_err(map_wallet_error)?;
    match outcome {
        SendOutcome::Settled { preimage } => {
            print_json(&serde_json::json!({
                "paid": true,
                "preimage": preimage,
                "payment_hash": payment_hash,
                "amount_msat": amount_msat,
            }));
            Ok(())
        }
        other => map_send_outcome(other),
    }
}

fn map_send_outcome(outcome: SendOutcome) -> Result<(), CliError> {
    match outcome {
        SendOutcome::Settled { .. } => Ok(()),
        SendOutcome::Failed { reason } => Err(map_wallet_error(reason)),
        SendOutcome::Unknown => Err(CliError::Other(
            "payment outcome unknown — run buzz wallet reconcile".into(),
        )),
        SendOutcome::AlreadyClaimed { state } => {
            print_json(&serde_json::json!({
                "paid": false,
                "already_claimed": true,
                "state": format!("{state:?}"),
            }));
            Ok(())
        }
    }
}

async fn cmd_check(ctx: &WalletCtx<'_>, request: &str, channel: &str) -> Result<(), CliError> {
    let client = ctx.client.ok_or_else(|| {
        CliError::Auth("BUZZ_PRIVATE_KEY is required for buzz wallet check".into())
    })?;
    validate_hex64(request)?;
    let channel_id = Uuid::parse_str(channel)
        .map_err(|_| CliError::Usage(format!("invalid UUID: {channel}")))?;
    let (payment_request, _) = fetch_payment_request(client, request, &channel_id).await?;

    let uri = resolve_nwc_uri()?;
    let wallet = open_wallet(ctx, &uri, true).await?;
    let status = wallet
        .check_incoming(&payment_request)
        .await
        .map_err(map_wallet_error)?;
    let status_str = match status {
        IncomingStatus::Paid => "paid",
        IncomingStatus::Unpaid => "unpaid",
        IncomingStatus::Unconfirmable => "unconfirmable",
    };
    print_json(&serde_json::json!({ "status": status_str }));
    Ok(())
}

async fn cmd_reconcile(ctx: &WalletCtx<'_>) -> Result<(), CliError> {
    let uri = resolve_nwc_uri()?;
    let wallet = open_wallet(ctx, &uri, false).await?;
    let settled = wallet.reconcile().await.map_err(map_wallet_error)?;
    print_json(&serde_json::json!({
        "reconciled": true,
        "settled_count": settled.len(),
    }));
    Ok(())
}

async fn fetch_payment_request(
    client: &BuzzClient,
    event_id: &str,
    channel_id: &Uuid,
) -> Result<(PaymentRequest, EventId), CliError> {
    let filter = serde_json::json!({
        "ids": [event_id],
        "kinds": [KIND_PAYMENT_REQUEST],
        "#h": [channel_id.to_string()],
        "limit": 1
    });
    let raw = client.query(&filter).await?;
    let events: Vec<serde_json::Value> = serde_json::from_str(&raw)
        .map_err(|e| CliError::Other(format!("failed to parse request query: {e}")))?;
    let event = events
        .first()
        .ok_or_else(|| CliError::NotFound(format!("payment request {event_id} not found")))?;
    let tags = event_tags(event)?;
    let request = PaymentRequest::from_tags(&tags)
        .map_err(|e| CliError::Usage(format!("invalid payment request: {e}")))?;
    let eid =
        EventId::parse(event_id).map_err(|e| CliError::Usage(format!("invalid event ID: {e}")))?;
    Ok((request, eid))
}

fn event_tags(event: &serde_json::Value) -> Result<Vec<Vec<String>>, CliError> {
    let tags = event
        .get("tags")
        .and_then(|t| t.as_array())
        .ok_or_else(|| CliError::Other("event missing tags".into()))?;
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let arr = tag
            .as_array()
            .ok_or_else(|| CliError::Other("malformed event tag".into()))?;
        let mut row = Vec::with_capacity(arr.len());
        for v in arr {
            let s = v
                .as_str()
                .ok_or_else(|| CliError::Other("malformed event tag value".into()))?;
            row.push(s.to_string());
        }
        out.push(row);
    }
    Ok(out)
}

fn decode_invoice(bolt11: &str) -> Result<Bolt11Invoice, CliError> {
    Bolt11Invoice::from_str(bolt11.trim())
        .map_err(|_| CliError::Usage("invalid bolt11 invoice".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::exit_code;
    use buzz_wallet::fakes::{
        FakeClock, FakeLnurlResolver, FakeWalletConnector, FakeWalletService, InMemoryPaymentStore,
        InMemorySecretStore, RecordingProfilePublisher,
    };
    use buzz_wallet::{Capabilities, StoredSecret, WalletMethod};

    fn test_timeouts() -> WalletTimeouts {
        WalletTimeouts {
            connect: Duration::from_secs(5),
            pay_response: Duration::from_secs(30),
        }
    }

    #[test]
    fn unsupported_maps_to_auth_exit_3() {
        let err = map_wallet_error(WalletError::Unsupported);
        assert!(matches!(err, CliError::Auth(_)));
        assert_eq!(exit_code(&err), 3);
        assert!(!err.to_string().contains("nostr+walletconnect"));
    }

    #[test]
    fn unauthorized_maps_to_auth_exit_3() {
        let err = map_wallet_error(WalletError::Unauthorized);
        assert_eq!(exit_code(&err), 3);
    }

    #[test]
    fn invalid_uri_maps_to_usage_exit_1() {
        let err = map_wallet_error(WalletError::InvalidUri);
        assert_eq!(exit_code(&err), 1);
    }

    #[tokio::test]
    async fn quote_without_yes_does_not_pay() {
        use buzz_wallet::fakes::{ConnectorScript, PayScript, WalletCall};

        let service = Arc::new(FakeWalletService::new());
        let connector = Arc::new(FakeWalletConnector::new());
        connector.script(ConnectorScript::Succeed {
            service: service.clone(),
            capabilities: Capabilities::from_methods([
                WalletMethod::PayInvoice,
                WalletMethod::MakeInvoice,
                WalletMethod::LookupInvoice,
                WalletMethod::GetBalance,
            ]),
            lud16: None,
        });
        let secrets = Arc::new(InMemorySecretStore::new("test"));
        let store = Arc::new(InMemoryPaymentStore::new("test"));
        let wallet = Wallet::new(
            connector,
            secrets,
            Arc::new(RecordingProfilePublisher::new()),
            Arc::new(FakeLnurlResolver::new()),
            store.clone(),
            Arc::new(FakeClock::new(1_700_000_000)),
            test_timeouts(),
        );
        let uri = "nostr+walletconnect://pub?relay=ws://127.0.0.1&secret=ab";
        wallet.link(uri).await.unwrap();

        let minted = buzz_wallet::test_support::mint_bolt11_for_amount(21_000, 0x11, 1_700_000_000);
        service.script_pay(PayScript::Settle {
            preimage: minted.preimage_hex.clone(),
        });

        let handle = wallet
            .prepare_send(
                AttemptKey::Standalone,
                SendTarget::Bolt11(minted.bolt11.clone()),
                Amount::from_msat(21_000),
                None,
            )
            .await
            .unwrap();
        // No confirm — the --yes gate. Cancel drops the handle.
        wallet.cancel(handle);
        assert_eq!(
            service
                .calls()
                .iter()
                .filter(|c| matches!(c, WalletCall::PayInvoice { .. }))
                .count(),
            0
        );
        assert!(store.all().is_empty());
    }

    #[tokio::test]
    async fn receive_only_pay_maps_to_auth_exit_3() {
        use buzz_wallet::fakes::{ConnectorScript, PayScript};

        let service = Arc::new(FakeWalletService::new());
        let connector = Arc::new(FakeWalletConnector::new());
        connector.script(ConnectorScript::Succeed {
            service: service.clone(),
            capabilities: Capabilities::from_methods([
                WalletMethod::MakeInvoice,
                WalletMethod::LookupInvoice,
            ]),
            lud16: None,
        });
        // Mock receive-only wallets answer pay with RESTRICTED → Unauthorized.
        service.script_pay(PayScript::Fail(WalletError::Unauthorized));
        let secrets = Arc::new(InMemorySecretStore::new("agent"));
        let wallet = Wallet::new(
            connector,
            secrets,
            Arc::new(RecordingProfilePublisher::new()),
            Arc::new(FakeLnurlResolver::new()),
            Arc::new(InMemoryPaymentStore::new("agent")),
            Arc::new(FakeClock::new(1_700_000_000)),
            test_timeouts(),
        );
        wallet
            .link("nostr+walletconnect://pub?relay=ws://127.0.0.1&secret=ab")
            .await
            .unwrap();
        let minted = buzz_wallet::test_support::mint_bolt11_for_amount(21_000, 0x22, 1_700_000_000);
        let handle = wallet
            .prepare_send(
                AttemptKey::Standalone,
                SendTarget::Bolt11(minted.bolt11),
                Amount::from_msat(21_000),
                None,
            )
            .await
            .unwrap();
        let err = wallet.confirm(&handle).await.expect_err("receive-only pay");
        assert!(matches!(
            err,
            WalletError::Unauthorized | WalletError::Unsupported
        ));
        assert_eq!(exit_code(&map_wallet_error(err)), 3);
    }

    #[test]
    fn stored_secret_debug_redacts_uri() {
        let secret = StoredSecret {
            uri: "nostr+walletconnect://secret-value".into(),
            capabilities: Capabilities::empty(),
            lud16: None,
        };
        let dbg = format!("{secret:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("secret-value"));
        assert!(!dbg.contains("nostr+walletconnect://secret-value"));
    }

    #[test]
    fn self_payee_signed_request_retains_p_tag() {
        // Command-seam regression: default request flow sets payee = signer.
        use nostr::Keys;

        let keys = Keys::generate();
        let payee = keys.public_key().to_hex();
        let client =
            BuzzClient::new("http://localhost:3000".into(), keys, None, None).expect("client");
        let channel = Uuid::new_v4();
        let event = build_and_sign_payment_request(
            &client,
            channel,
            21_000,
            &payee,
            Some("lnbc210n1commandseam"),
            Some("self"),
            Some(1_800_000_000),
        )
        .expect("sign");
        assert_eq!(event.pubkey.to_hex(), payee);
        let has_p = event.tags.iter().any(|t| {
            let s = t.as_slice();
            s.len() >= 2 && s[0] == "p" && s[1] == payee
        });
        assert!(
            has_p,
            "signed self-payee 40009 must retain p; tags={:?}",
            event
                .tags
                .iter()
                .map(|t| t.as_slice().to_vec())
                .collect::<Vec<_>>()
        );
    }
}

//! Dev-only loopback harness for Lightning wallet Playwright proofs.
//!
//! **Not for production.** Binds `127.0.0.1` only. Embeds `buzz-mock-wallet`,
//! hosts the real desktop [`WalletRuntime`](buzz_lib::wallet::WalletRuntime)
//! wired with `NwcWalletConnector` + `HttpLnurlResolver`, and exposes a tiny
//! HTTP surface so the E2E mock bridge can proxy wallet IPC to live NWC.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use buzz_lib::wallet::{
    AttemptKeyDto, JsonPaymentStore, KeyringNwcSecretStore, MapBlobBackend, SendTargetDto,
    WalletPorts, WalletRuntime,
};
use buzz_mock_wallet::{MockWallet, MockWalletConfig};
use buzz_wallet_pkg::fakes::RecordingProfilePublisher;
use buzz_wallet_pkg::{HttpLnurlResolver, NwcWalletConnector, SystemClock, WalletTimeouts};
use serde::Deserialize;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tracing::{error, info};

const BIND_HOST: &str = "127.0.0.1";
const BIND_PORT: u16 = 4189;
const COMMUNITY_ID: &str = "e2e-wallet";
const STARTING_BALANCE_MSAT: u64 = 100_000_000;

/// Where the paste-ready URI and payee invoices come from.
///
/// `Real` (opted into with `BUZZ_NWC_URI`) points at an operator-provided
/// wallet; payee invoices are minted from that same wallet via the runtime,
/// so a paid invoice is a genuine self-payment — real preimage, net-zero cost.
enum Payee {
    Mock(MockWallet),
    Real { uri: String },
}

struct HarnessState {
    payee: Payee,
    runtime: WalletRuntime,
    /// Keep tempdir alive for the process lifetime.
    _data_dir: TempDir,
}

#[derive(Debug, Deserialize)]
struct InvokeBody {
    cmd: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize)]
struct MintBody {
    amount_msat: u64,
    #[serde(default)]
    description: Option<String>,
}

fn production_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(30),
        pay_response: Duration::from_secs(60),
    }
}

fn build_runtime(data_dir: &std::path::Path) -> Result<WalletRuntime, String> {
    let timeouts = production_timeouts();
    let store_path = buzz_lib::wallet::payments_path(data_dir, COMMUNITY_ID);
    let ports = WalletPorts {
        connector: Arc::new(NwcWalletConnector::new(timeouts)),
        secrets: Arc::new(KeyringNwcSecretStore::new(
            COMMUNITY_ID.to_string(),
            MapBlobBackend::new(),
        )),
        profiles: Arc::new(RecordingProfilePublisher::new()),
        resolver: Arc::new(HttpLnurlResolver::new()),
        store: Arc::new(JsonPaymentStore::open(store_path)?),
        clock: Arc::new(SystemClock),
        timeouts,
    };
    Ok(WalletRuntime::new(COMMUNITY_ID.to_string(), ports))
}

fn cors_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    // Loopback-only harness: allow both localhost and 127.0.0.1 preview origins.
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    response
}

async fn options_ok() -> Response {
    cors_headers(StatusCode::NO_CONTENT.into_response())
}

async fn get_uri(State(state): State<Arc<Mutex<HarnessState>>>) -> Response {
    let guard = state.lock().await;
    let uri = match &guard.payee {
        Payee::Mock(mock) => mock.uri().to_string(),
        Payee::Real { uri } => uri.clone(),
    };
    cors_headers(Json(json!({ "uri": uri })).into_response())
}

/// Mint a payee invoice: from the embedded mock, or (real mode) from the
/// linked wallet itself, making the subsequent pay a self-payment.
async fn mint_payee(
    state: &HarnessState,
    amount_msat: u64,
    description: &str,
) -> Result<(String, String), String> {
    match &state.payee {
        Payee::Mock(mock) => {
            let minted = mock
                .mint_payee(amount_msat, description)
                .map_err(|e| e.to_string())?;
            Ok((minted.bolt11, minted.payment_hash_hex))
        }
        Payee::Real { .. } => {
            let bolt11 = state
                .runtime
                .receive(amount_msat, Some(description))
                .await?;
            let hash =
                buzz_wallet_pkg::payment_hash_hex(&buzz_wallet_pkg::Bolt11::new(bolt11.clone()))
                    .map_err(|e| e.to_string())?;
            Ok((bolt11, hash))
        }
    }
}

async fn post_mint(
    State(state): State<Arc<Mutex<HarnessState>>>,
    Json(body): Json<MintBody>,
) -> Response {
    let guard = state.lock().await;
    let description = body.description.unwrap_or_else(|| "e2e-payee".to_string());
    match mint_payee(&guard, body.amount_msat, &description).await {
        Ok((bolt11, payment_hash)) => cors_headers(
            Json(json!({
                "bolt11": bolt11,
                "payment_hash": payment_hash,
            }))
            .into_response(),
        ),
        Err(err) => {
            cors_headers((StatusCode::BAD_REQUEST, Json(json!({ "error": err }))).into_response())
        }
    }
}

fn arg_u64(args: &Value, camel: &str, snake: &str) -> Option<u64> {
    args.get(camel)
        .or_else(|| args.get(snake))
        .and_then(|v| v.as_u64())
}

fn arg_string(args: &Value, camel: &str, snake: &str) -> Option<String> {
    args.get(camel)
        .or_else(|| args.get(snake))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

async fn dispatch_cmd(runtime: &WalletRuntime, cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "link_wallet" => {
            let uri = arg_string(args, "uri", "uri").ok_or_else(|| "missing uri".to_string())?;
            let view = runtime.link(&uri).await?;
            serde_json::to_value(view).map_err(|e| e.to_string())
        }
        "unlink_wallet" => {
            runtime.unlink().await?;
            Ok(Value::Null)
        }
        "wallet_status" => {
            let view = runtime.status().await?;
            serde_json::to_value(view).map_err(|e| e.to_string())
        }
        "wallet_receive" => {
            let amount_msat = arg_u64(args, "amountMsat", "amount_msat")
                .ok_or_else(|| "missing amount_msat".to_string())?;
            let description = arg_string(args, "description", "description");
            let bolt11 = runtime.receive(amount_msat, description.as_deref()).await?;
            Ok(Value::String(bolt11))
        }
        "wallet_prepare_send" => {
            let amount_msat = arg_u64(args, "amountMsat", "amount_msat")
                .ok_or_else(|| "missing amount_msat".to_string())?;
            let memo = arg_string(args, "memo", "memo");
            let target_val = args
                .get("target")
                .cloned()
                .ok_or_else(|| "missing target".to_string())?;
            let target: SendTargetDto =
                serde_json::from_value(target_val).map_err(|e| e.to_string())?;
            let attempt_val = args
                .get("attempt")
                .cloned()
                .unwrap_or(json!({ "type": "standalone" }));
            let attempt: AttemptKeyDto =
                serde_json::from_value(attempt_val).map_err(|e| e.to_string())?;
            let quote = runtime
                .prepare_send(target, amount_msat, attempt, memo.as_deref())
                .await?;
            serde_json::to_value(quote).map_err(|e| e.to_string())
        }
        "wallet_confirm" => {
            let handle_id = arg_string(args, "handleId", "handle_id")
                .ok_or_else(|| "missing handle_id".to_string())?;
            let outcome = runtime.confirm(&handle_id).await?;
            serde_json::to_value(outcome).map_err(|e| e.to_string())
        }
        "wallet_cancel" => {
            let handle_id = arg_string(args, "handleId", "handle_id")
                .ok_or_else(|| "missing handle_id".to_string())?;
            runtime.cancel(&handle_id)?;
            Ok(Value::Null)
        }
        "wallet_reconcile" => {
            runtime.reconcile().await?;
            Ok(Value::Null)
        }
        other => Err(format!("unknown_cmd:{other}")),
    }
}

async fn post_invoke(
    State(state): State<Arc<Mutex<HarnessState>>>,
    Json(body): Json<InvokeBody>,
) -> Response {
    let guard = state.lock().await;
    info!(cmd = %body.cmd, "harness invoke");
    match dispatch_cmd(&guard.runtime, &body.cmd, &body.args).await {
        Ok(result) => cors_headers(Json(json!({ "ok": true, "result": result })).into_response()),
        Err(error) => {
            error!(cmd = %body.cmd, %error, "harness invoke failed");
            cors_headers(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "ok": false, "error": error })),
                )
                    .into_response(),
            )
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("wallet_e2e_harness: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Dependencies enable both rustls providers; choose one before any TLS
    // (wss://) connection — mirrors the app's install_crypto_provider().
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,buzz_mock_wallet=info,buzz_wallet=info,nwc=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let data_dir = TempDir::new()?;
    // Spec fetches the paste-ready URI from GET /uri — never log it (embeds secret).
    // Real mode requires the explicit flag: a merely-exported BUZZ_NWC_URI (common
    // for CLI use) must not silently move real money in the mock-wallet specs.
    let payee = if std::env::var("BUZZ_HARNESS_REAL_WALLET").as_deref() == Ok("1") {
        let uri = std::env::var("BUZZ_NWC_URI")
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .ok_or("BUZZ_HARNESS_REAL_WALLET=1 requires BUZZ_NWC_URI")?;
        info!("real wallet mode: BUZZ_NWC_URI set (URI not logged)");
        Payee::Real { uri }
    } else {
        let mock = MockWallet::start(MockWalletConfig {
            balance_msat: STARTING_BALANCE_MSAT,
            ..Default::default()
        })
        .await?;
        info!(
            wallet_pubkey = mock.wallet_pubkey(),
            relay = mock.relay_url(),
            balance_msat = STARTING_BALANCE_MSAT,
            "mock wallet ready (NWC URI not logged)"
        );
        Payee::Mock(mock)
    };

    let runtime = build_runtime(data_dir.path())?;
    let state = Arc::new(Mutex::new(HarnessState {
        payee,
        runtime,
        _data_dir: data_dir,
    }));

    let app = Router::new()
        .route(
            "/",
            get(|| async {
                cors_headers(
                    Json(json!({ "ok": true, "service": "wallet_e2e_harness" })).into_response(),
                )
            })
            .options(options_ok),
        )
        .route("/uri", get(get_uri).options(options_ok))
        .route("/mint", post(post_mint).options(options_ok))
        .route("/invoke", post(post_invoke).options(options_ok))
        .with_state(state);

    let addr = SocketAddr::new(BIND_HOST.parse()?, BIND_PORT);
    info!(%addr, "wallet_e2e_harness listening (dev-only, loopback)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

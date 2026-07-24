//! CLI wrapper for the dev-only buzz-mock-wallet daemon.
//!
//! Moves no real money. Binds loopback only.

use buzz_mock_wallet::{MockWallet, MockWalletConfig, Script};
use clap::Parser;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// Local NWC mock wallet for Buzz end-to-end testing (dev-only, no real money).
#[derive(Debug, Parser)]
#[command(name = "buzz-mock-wallet", about = "Dev-only local NWC mock wallet")]
struct Args {
    /// Starting balance in millisatoshis.
    #[arg(long, default_value_t = 1_000_000)]
    balance_msat: u64,

    /// Loopback bind port (0 = ephemeral).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Omit pay methods; answer pay attempts with RESTRICTED.
    #[arg(long, default_value_t = false)]
    receive_only: bool,

    /// Fail the next pay_invoice with this NIP-47 error code (e.g. PAYMENT_FAILED).
    #[arg(long)]
    fail_next_pay: Option<String>,

    /// Fixed delay in milliseconds before every RPC response.
    #[arg(long, default_value_t = 0)]
    response_delay_ms: u64,

    /// Swallow NWC requests (no 23195) — for client-timeout testing.
    #[arg(long, default_value_t = false)]
    swallow_requests: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("info".parse().unwrap_or_default()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let script = Script {
        fail_next_pay_code: args.fail_next_pay,
        response_delay: Duration::from_millis(args.response_delay_ms),
        swallow_requests: args.swallow_requests,
        ..Default::default()
    };
    let config = MockWalletConfig {
        balance_msat: args.balance_msat,
        port: Some(args.port),
        receive_only: args.receive_only,
        script,
    };

    let wallet = match MockWallet::start(config).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("fatal: {e}");
            std::process::exit(1);
        }
    };

    println!("{}", wallet.uri());
    eprintln!("buzz-mock-wallet listening on {}", wallet.relay_url());
    eprintln!("dev-only — moves no real money; Ctrl-C to stop");

    match tokio::signal::ctrl_c().await {
        Ok(()) => {}
        Err(e) => eprintln!("signal error: {e}"),
    }
    wallet.shutdown();
}

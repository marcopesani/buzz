//! Dev-only local NWC mock wallet for end-to-end testing.
//!
//! **Moves no real money.** Binds loopback only (`127.0.0.1`), keeps an
//! in-memory msat ledger, and speaks the NIP-47 wire protocol so clients can
//! exercise pay / receive / lookup against a paste-ready
//! `nostr+walletconnect://` URI.
//!
//! Encryption for kinds 23194 / 23195 / 23196 is **NIP-04**, matching rust-nostr
//! `nwc` 0.44 (see [`daemon`] module docs).

#![warn(missing_docs)]

mod daemon;
mod error;
mod ledger;
mod mint;
mod relay;

pub use daemon::advertised_methods;
pub use error::MockWalletError;
pub use ledger::Script;
pub use mint::MintedInvoice;

use daemon::run_daemon;
use ledger::Ledger;
use nostr::Keys;
use relay::Relay;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Configuration for [`MockWallet::start`].
#[derive(Debug, Clone)]
pub struct MockWalletConfig {
    /// Starting ledger balance in millisatoshis.
    pub balance_msat: u64,
    /// Bind port; `None` or `0` picks a free loopback port.
    pub port: Option<u16>,
    /// When true, omit pay methods from 13194 / `get_info` and reject pay with `RESTRICTED`.
    pub receive_only: bool,
    /// Scriptable failure / timing knobs.
    pub script: Script,
}

impl Default for MockWalletConfig {
    fn default() -> Self {
        Self {
            balance_msat: 1_000_000,
            port: None,
            receive_only: false,
            script: Script::default(),
        }
    }
}

/// Running mock wallet: loopback relay + NWC daemon.
pub struct MockWallet {
    /// Ready-to-paste NWC URI.
    uri: String,
    /// WebSocket URL of the loopback relay.
    relay_url: String,
    /// Bound loopback port.
    port: u16,
    /// Wallet service pubkey (hex).
    wallet_pubkey: String,
    /// Client secret hex from the URI (for tests that rebuild the URI).
    client_secret_hex: String,
    /// Shared ledger (tests may tweak [`Script`]).
    ledger: Arc<Ledger>,
    cancel: CancellationToken,
    _relay_task: tokio::task::JoinHandle<()>,
    _daemon_task: tokio::task::JoinHandle<()>,
}

impl MockWallet {
    /// Start the loopback relay and NWC daemon.
    pub async fn start(config: MockWalletConfig) -> Result<Self, MockWalletError> {
        let cancel = CancellationToken::new();
        let relay = Relay::new();
        let bind_port = config.port.unwrap_or(0);
        let (addr, relay_task) = relay
            .listen(bind_port, cancel.clone())
            .await
            .map_err(|e| MockWalletError::Network(e.to_string()))?;
        let port = addr.port();
        let relay_url = format!("ws://127.0.0.1:{port}");

        let wallet_keys = Keys::generate();
        let client_keys = Keys::generate();
        let client_secret_hex = hex::encode(client_keys.secret_key().as_secret_bytes());
        let wallet_pubkey = wallet_keys.public_key().to_hex();

        let uri = format!(
            "nostr+walletconnect://{wallet_pubkey}?relay={}&secret={client_secret_hex}",
            percent_encode_component(&relay_url)
        );

        let ledger = Ledger::new(config.balance_msat, &wallet_keys, config.script);
        let daemon_relay = Arc::clone(&relay);
        let daemon_ledger = Arc::clone(&ledger);
        let daemon_cancel = cancel.clone();
        let daemon_url = relay_url.clone();
        let client_pubkey = client_keys.public_key();
        let receive_only = config.receive_only;

        // Brief pause so the accept loop is ready.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let daemon_task = tokio::spawn(async move {
            if let Err(e) = run_daemon(
                daemon_relay,
                &daemon_url,
                wallet_keys,
                client_pubkey,
                daemon_ledger,
                receive_only,
                daemon_cancel,
            )
            .await
            {
                tracing::error!("mock wallet daemon exited: {e}");
            }
        });

        // Give the daemon a moment to publish the 13194 info event.
        tokio::time::sleep(Duration::from_millis(70)).await;

        // Never log the paste-ready URI — it embeds `secret=`. Standalone
        // `main.rs` prints it to stdout for BUZZ_NWC_URI export; library
        // callers (harness / tests) fetch via [`MockWallet::uri`].
        info!(
            wallet_pubkey = %wallet_pubkey,
            relay = %relay_url,
            "buzz-mock-wallet ready (dev-only, moves no real money; NWC URI not logged)"
        );

        Ok(Self {
            uri,
            relay_url,
            port,
            wallet_pubkey,
            client_secret_hex,
            ledger,
            cancel,
            _relay_task: relay_task,
            _daemon_task: daemon_task,
        })
    }

    /// Ready-to-paste `nostr+walletconnect://` URI.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Loopback relay WebSocket URL (`ws://127.0.0.1:{port}`).
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    /// Bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Wallet service pubkey (hex).
    pub fn wallet_pubkey(&self) -> &str {
        &self.wallet_pubkey
    }

    /// Client secret hex embedded in the URI.
    pub fn client_secret_hex(&self) -> &str {
        &self.client_secret_hex
    }

    /// Current ledger balance in millisatoshis.
    pub fn balance_msat(&self) -> u64 {
        self.ledger.balance_msat()
    }

    /// Mint a payee bolt11: paying it debits the ledger and returns a verifying
    /// preimage (balance actually moves — not a self-pay net-zero).
    pub fn mint_payee(
        &self,
        amount_msat: u64,
        description: &str,
    ) -> Result<MintedInvoice, MockWalletError> {
        self.ledger
            .make_payee_invoice(amount_msat, description, 3600)
    }

    /// Update scriptable failure / timing knobs after start.
    pub fn set_script(&self, script: Script) {
        self.ledger.set_script(script);
    }

    /// Gracefully stop the daemon and relay.
    pub fn shutdown(self) {
        self.cancel.cancel();
    }
}

fn percent_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

//! Durable CLI storage for the NWC secret and payment attempts.
//!
//! Secret resolution order for connect URIs: `BUZZ_NWC_URI` env, else the
//! 0600 secret file. Payment records are JSON under the XDG data dir, keyed
//! by relay URL (community boundary). Neither path ever logs secret material.

use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use buzz_core::payment::Amount;
use buzz_wallet::{
    AttemptId, Bolt11, Capabilities, ClaimOutcome, PaymentRecord, PaymentStore,
    PersistedPaymentState, SecretStore, StoredSecret, WalletError,
};

use crate::error::CliError;

/// Env override for the secret file path (tests + custom installs).
pub const ENV_NWC_SECRET_PATH: &str = "BUZZ_NWC_SECRET_PATH";
/// Env override for the payment-store directory (tests + custom installs).
pub const ENV_WALLET_DATA_DIR: &str = "BUZZ_WALLET_DATA_DIR";
/// Env var holding the NWC URI (takes precedence over the secret file).
pub const ENV_NWC_URI: &str = "BUZZ_NWC_URI";

/// Default relative path under the config dir for the NWC secret JSON.
pub const DEFAULT_SECRET_REL: &str = "buzz/nwc-secret.json";
/// Default relative path under the data dir for payment JSON files.
pub const DEFAULT_PAYMENTS_REL: &str = "buzz/payments";

/// Resolve the NWC secret file path.
///
/// Order: `BUZZ_NWC_SECRET_PATH`, else `$XDG_CONFIG_HOME/buzz/nwc-secret.json`
/// (via `dirs::config_dir()`).
pub fn secret_file_path() -> Result<PathBuf, CliError> {
    if let Ok(p) = std::env::var(ENV_NWC_SECRET_PATH) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let config = dirs::config_dir().ok_or_else(|| {
        CliError::Other("could not resolve config directory for NWC secret".into())
    })?;
    Ok(config.join(DEFAULT_SECRET_REL))
}

/// Resolve the directory that holds per-relay payment JSON files.
pub fn payments_dir() -> Result<PathBuf, CliError> {
    if let Ok(p) = std::env::var(ENV_WALLET_DATA_DIR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let data = dirs::data_dir().ok_or_else(|| {
        CliError::Other("could not resolve data directory for wallet payments".into())
    })?;
    Ok(data.join(DEFAULT_PAYMENTS_REL))
}

/// Outcome of resolving an NWC URI without treating "unlinked" as an error.
///
/// Callers that need a hard failure on missing config use [`resolve_nwc_uri`];
/// `status` uses this enum so permission / I/O failures are not masked as
/// `linked: false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NwcUriResolution {
    /// A usable URI from env or the secret file.
    Uri(String),
    /// No env URI and no secret file — genuinely unlinked.
    NotLinked,
}

/// Classify secret resolution: env URI, linked file, unlinked, or real error.
///
/// Never returns the URI in error messages. A secret file that exists but is
/// unreadable (wrong permissions, malformed JSON, I/O) is an `Err` — not
/// [`NwcUriResolution::NotLinked`].
pub fn resolve_nwc_uri_status() -> Result<NwcUriResolution, CliError> {
    if let Ok(uri) = std::env::var(ENV_NWC_URI) {
        let trimmed = uri.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(NwcUriResolution::Uri(trimmed));
        }
    }
    let path = secret_file_path()?;
    if !path.exists() {
        return Ok(NwcUriResolution::NotLinked);
    }
    // File exists: permission / parse failures must surface, not look unlinked.
    let stored = FileSecretStore::new(path).load_blocking()?;
    match stored {
        Some(s) if !s.uri.trim().is_empty() => Ok(NwcUriResolution::Uri(s.uri)),
        Some(_) => Err(CliError::Other("NWC secret file is malformed".into())),
        None => Ok(NwcUriResolution::NotLinked),
    }
}

/// Resolve the NWC URI to connect with: env first, else secret file.
///
/// Never returns the URI in error messages. Maps [`NwcUriResolution::NotLinked`]
/// to a usage error for verbs that require a linked wallet.
pub fn resolve_nwc_uri() -> Result<String, CliError> {
    match resolve_nwc_uri_status()? {
        NwcUriResolution::Uri(uri) => Ok(uri),
        NwcUriResolution::NotLinked => Err(CliError::Usage(
            "no wallet linked (set BUZZ_NWC_URI or run buzz wallet link)".into(),
        )),
    }
}

/// Refuse to read a secret file if group/other permission bits are set.
pub fn assert_secret_permissions(path: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path)
            .map_err(|e| CliError::Other(format!("failed to stat NWC secret file: {e}")))?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(CliError::Other(format!(
                "NWC secret file permissions are too open ({mode:04o}); expected 0600"
            )));
        }
    }
    let _ = path;
    Ok(())
}

/// Create parent directories with mode 0700 when possible.
fn ensure_parent_dirs(path: &Path) -> Result<(), CliError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent)
        .map_err(|e| CliError::Other(format!("failed to create NWC secret directory: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Write `contents` to `path` with mode 0600 (create or truncate).
pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    ensure_parent_dirs(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| CliError::Other(format!("failed to write NWC secret file: {e}")))?;
        file.write_all(contents)
            .map_err(|e| CliError::Other(format!("failed to write NWC secret file: {e}")))?;
        // Re-assert 0600 in case the umask interfered on create.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|e| CliError::Other(format!("failed to chmod NWC secret file: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
            .map_err(|e| CliError::Other(format!("failed to write NWC secret file: {e}")))?;
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredSecretFile {
    uri: String,
    capabilities: Vec<String>,
    lud16: Option<String>,
}

impl StoredSecretFile {
    fn from_secret(secret: &StoredSecret) -> Self {
        Self {
            uri: secret.uri.clone(),
            capabilities: secret
                .capabilities
                .iter()
                .map(|m| m.as_str().to_string())
                .collect(),
            lud16: secret.lud16.clone(),
        }
    }

    fn into_secret(self) -> StoredSecret {
        StoredSecret {
            uri: self.uri,
            capabilities: Capabilities::parse(self.capabilities),
            lud16: self.lud16,
        }
    }
}

/// File-backed [`SecretStore`] — JSON at a fixed path, mode 0600.
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    /// Store secrets at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Synchronous load for URI resolution (permission-checked).
    pub fn load_blocking(&self) -> Result<Option<StoredSecret>, CliError> {
        if !self.path.exists() {
            return Ok(None);
        }
        assert_secret_permissions(&self.path)?;
        let raw = fs::read_to_string(&self.path)
            .map_err(|e| CliError::Other(format!("failed to read NWC secret file: {e}")))?;
        let parsed: StoredSecretFile = serde_json::from_str(&raw)
            .map_err(|_| CliError::Other("NWC secret file is malformed".into()))?;
        Ok(Some(parsed.into_secret()))
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn store(&self, secret: &StoredSecret) -> Result<(), WalletError> {
        let file = StoredSecretFile::from_secret(secret);
        let json = serde_json::to_vec_pretty(&file).map_err(|_| WalletError::SecretUnavailable)?;
        write_private_file(&self.path, &json).map_err(|_| WalletError::SecretUnavailable)?;
        Ok(())
    }

    async fn load(&self) -> Result<Option<StoredSecret>, WalletError> {
        self.load_blocking()
            .map_err(|_| WalletError::SecretUnavailable)
    }

    async fn clear(&self) -> Result<(), WalletError> {
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|_| WalletError::SecretUnavailable)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PaymentFileRecord {
    attempt_id: String,
    payment_hash: String,
    bolt11: String,
    amount_msat: u64,
    expires_at_unix: u64,
    state: String,
}

impl PaymentFileRecord {
    fn from_record(r: &PaymentRecord) -> Self {
        Self {
            attempt_id: r.attempt_id.as_str().to_string(),
            payment_hash: r.payment_hash.clone(),
            bolt11: r.bolt11.as_str().to_string(),
            amount_msat: r.amount.as_msat(),
            expires_at_unix: r.expires_at_unix,
            state: state_to_str(r.state).to_string(),
        }
    }

    fn into_record(self) -> Option<PaymentRecord> {
        let state = state_from_str(&self.state)?;
        Some(PaymentRecord {
            attempt_id: AttemptId::new(self.attempt_id),
            payment_hash: self.payment_hash,
            bolt11: Bolt11::new(self.bolt11),
            amount: Amount::from_msat(self.amount_msat),
            expires_at_unix: self.expires_at_unix,
            state,
        })
    }
}

fn state_to_str(state: PersistedPaymentState) -> &'static str {
    match state {
        PersistedPaymentState::Paying => "paying",
        PersistedPaymentState::Settled => "settled",
        PersistedPaymentState::Failed => "failed",
        PersistedPaymentState::Unknown => "unknown",
    }
}

fn state_from_str(s: &str) -> Option<PersistedPaymentState> {
    match s {
        "paying" => Some(PersistedPaymentState::Paying),
        "settled" => Some(PersistedPaymentState::Settled),
        "failed" => Some(PersistedPaymentState::Failed),
        "unknown" => Some(PersistedPaymentState::Unknown),
        _ => None,
    }
}

/// Sanitize a relay URL into a single path segment.
pub fn relay_store_key(relay_url: &str) -> String {
    let mut out = String::with_capacity(relay_url.len());
    for c in relay_url.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".into()
    } else {
        out
    }
}

/// JSON-file [`PaymentStore`] keyed by relay URL (community boundary).
///
/// Loads the whole map into memory, mutates under a mutex, and rewrites the
/// 0600 file after every successful mutation so one-shot CLI processes can
/// reconcile on the next invocation.
pub struct JsonPaymentStore {
    path: PathBuf,
    records: Mutex<HashMap<String, PaymentRecord>>,
}

impl JsonPaymentStore {
    /// Open (or create) the store for `relay_url` under `dir`.
    pub fn open(dir: impl AsRef<Path>, relay_url: &str) -> Result<Self, CliError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(|e| {
            CliError::Other(format!("failed to create wallet payments directory: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        let path = dir.join(format!("{}.json", relay_store_key(relay_url)));
        let records = if path.exists() {
            assert_secret_permissions(&path)?;
            let raw = fs::read_to_string(&path)
                .map_err(|e| CliError::Other(format!("failed to read payment store: {e}")))?;
            let file_records: Vec<PaymentFileRecord> =
                serde_json::from_str(&raw).unwrap_or_default();
            file_records
                .into_iter()
                .filter_map(PaymentFileRecord::into_record)
                .map(|r| (r.attempt_id.as_str().to_string(), r))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            records: Mutex::new(records),
        })
    }

    fn persist_locked(
        path: &Path,
        records: &HashMap<String, PaymentRecord>,
    ) -> Result<(), WalletError> {
        let file: Vec<PaymentFileRecord> = records
            .values()
            .map(PaymentFileRecord::from_record)
            .collect();
        let json = serde_json::to_vec_pretty(&file).map_err(|_| WalletError::Unknown)?;
        write_private_file(path, &json).map_err(|_| WalletError::Unknown)
    }
}

#[async_trait]
impl PaymentStore for JsonPaymentStore {
    async fn claim_paying(
        &self,
        attempt_id: &AttemptId,
        payment_hash: &str,
        bolt11: &Bolt11,
        amount: Amount,
        expires_at_unix: u64,
    ) -> Result<ClaimOutcome, WalletError> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let key = attempt_id.as_str().to_string();
        if let Some(existing) = records.get(&key) {
            return Ok(ClaimOutcome::AlreadyClaimed(existing.clone()));
        }
        let record = PaymentRecord {
            attempt_id: attempt_id.clone(),
            payment_hash: payment_hash.to_string(),
            bolt11: bolt11.clone(),
            amount,
            expires_at_unix,
            state: PersistedPaymentState::Paying,
        };
        records.insert(key, record.clone());
        Self::persist_locked(&self.path, &records)?;
        Ok(ClaimOutcome::Claimed(record))
    }

    async fn get(&self, attempt_id: &AttemptId) -> Result<Option<PaymentRecord>, WalletError> {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        Ok(records.get(attempt_id.as_str()).cloned())
    }

    async fn update_state(
        &self,
        attempt_id: &AttemptId,
        state: PersistedPaymentState,
    ) -> Result<(), WalletError> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = records.get_mut(attempt_id.as_str()) else {
            return Err(WalletError::Unknown);
        };
        record.state = state;
        Self::persist_locked(&self.path, &records)
    }

    async fn list_by_states(
        &self,
        states: &[PersistedPaymentState],
    ) -> Result<Vec<PaymentRecord>, WalletError> {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        Ok(records
            .values()
            .filter(|r| states.contains(&r.state))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_wallet::WalletMethod;

    #[test]
    fn secret_file_round_trip_preserves_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nwc-secret.json");
        let store = FileSecretStore::new(&path);
        let secret = StoredSecret {
            uri: "nostr+walletconnect://test?relay=ws://127.0.0.1&secret=ab".into(),
            capabilities: Capabilities::from_methods([
                WalletMethod::MakeInvoice,
                WalletMethod::LookupInvoice,
            ]),
            lud16: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            store.store(&secret).await.unwrap();
            let loaded = store.load().await.unwrap().unwrap();
            assert_eq!(loaded.uri, secret.uri);
            assert!(loaded.capabilities.contains(WalletMethod::MakeInvoice));
            assert!(!loaded.capabilities.contains(WalletMethod::PayInvoice));
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn refuse_group_readable_secret_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nwc-secret.json");
        fs::write(&path, "{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            let err = assert_secret_permissions(&path).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("NWC secret"), "{msg}");
            assert!(!msg.contains("nostr+walletconnect"));
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    #[test]
    fn resolve_status_0644_is_error_not_unlinked() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nwc-secret.json");
            let body = serde_json::json!({
                "uri": "nostr+walletconnect://test?relay=ws://127.0.0.1&secret=ab",
                "capabilities": ["make_invoice"],
                "lud16": null
            });
            fs::write(&path, body.to_string()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

            // Isolate from ambient env / default paths.
            let prev_path = std::env::var_os(ENV_NWC_SECRET_PATH);
            let prev_uri = std::env::var_os(ENV_NWC_URI);
            std::env::set_var(ENV_NWC_SECRET_PATH, &path);
            std::env::remove_var(ENV_NWC_URI);

            let err = resolve_nwc_uri_status().expect_err("0644 must not look unlinked");
            let msg = err.to_string();
            assert!(
                msg.contains("permissions") || msg.contains("NWC secret"),
                "expected permission error, got: {msg}"
            );
            assert!(!matches!(
                resolve_nwc_uri_status().ok(),
                Some(NwcUriResolution::NotLinked)
            ));
            // status-shaped check: same classification balance/receive use via resolve_nwc_uri
            let usage_or_other = resolve_nwc_uri().unwrap_err();
            assert_eq!(crate::error::exit_code(&usage_or_other), 4);

            match prev_path {
                Some(v) => std::env::set_var(ENV_NWC_SECRET_PATH, v),
                None => std::env::remove_var(ENV_NWC_SECRET_PATH),
            }
            match prev_uri {
                Some(v) => std::env::set_var(ENV_NWC_URI, v),
                None => std::env::remove_var(ENV_NWC_URI),
            }
        }
    }

    #[test]
    fn resolve_status_missing_file_is_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-nwc-secret.json");
        let prev_path = std::env::var_os(ENV_NWC_SECRET_PATH);
        let prev_uri = std::env::var_os(ENV_NWC_URI);
        std::env::set_var(ENV_NWC_SECRET_PATH, &path);
        std::env::remove_var(ENV_NWC_URI);
        assert_eq!(
            resolve_nwc_uri_status().unwrap(),
            NwcUriResolution::NotLinked
        );
        match prev_path {
            Some(v) => std::env::set_var(ENV_NWC_SECRET_PATH, v),
            None => std::env::remove_var(ENV_NWC_SECRET_PATH),
        }
        match prev_uri {
            Some(v) => std::env::set_var(ENV_NWC_URI, v),
            None => std::env::remove_var(ENV_NWC_URI),
        }
    }

    #[test]
    fn payment_store_claim_paying_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let attempt = AttemptId::new("standalone:test-1");
        let bolt11 = Bolt11::new("lnbc1test");
        rt.block_on(async {
            let store = JsonPaymentStore::open(dir.path(), "http://localhost:3000").unwrap();
            let outcome = store
                .claim_paying(
                    &attempt,
                    "aa".repeat(32).as_str(),
                    &bolt11,
                    Amount::from_msat(21000),
                    1_800_000_000,
                )
                .await
                .unwrap();
            assert!(matches!(outcome, ClaimOutcome::Claimed(_)));
        });
        rt.block_on(async {
            let store = JsonPaymentStore::open(dir.path(), "http://localhost:3000").unwrap();
            let again = store
                .claim_paying(
                    &attempt,
                    "aa".repeat(32).as_str(),
                    &bolt11,
                    Amount::from_msat(21000),
                    1_800_000_000,
                )
                .await
                .unwrap();
            assert!(matches!(again, ClaimOutcome::AlreadyClaimed(_)));
            let pending = store
                .list_by_states(&[PersistedPaymentState::Paying])
                .await
                .unwrap();
            assert_eq!(pending.len(), 1);
        });
    }

    #[test]
    fn relay_store_key_sanitizes() {
        assert_eq!(
            relay_store_key("http://localhost:3000"),
            "http___localhost_3000"
        );
    }
}

//! Agent receive-only NWC provisioning.
//!
//! One gate: probe advertisements, refuse spend-capable wallets, store the URI
//! only at `agent-nwc:{pubkey}` in the keyring. Never writes to
//! `managed-agents.json`. Refusal stores nothing.
//!
//! Provision/unprovision for a given pubkey is serialized via
//! [`AgentNwcOpGates`] so probe + keyring write cannot interleave.

use crate::wallet::secret_store::BlobBackend;
use async_trait::async_trait;
use buzz_wallet_pkg::{
    Capabilities, NwcWalletConnector, WalletAdvertisement, WalletError, WalletMethod,
    WalletTimeouts,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// Spend methods that disqualify an agent wallet. Data — not an if-chain.
pub const AGENT_SPEND_METHODS: &[WalletMethod] = &[
    WalletMethod::PayInvoice,
    WalletMethod::PayKeysend,
    WalletMethod::MultiPayInvoice,
    WalletMethod::MultiPayKeysend,
];

/// Methods every agent wallet must advertise on the intersection.
const AGENT_REQUIRED_METHODS: &[WalletMethod] =
    &[WalletMethod::MakeInvoice, WalletMethod::LookupInvoice];

/// Keyring blob key for an agent's receive-only NWC URI.
pub fn agent_nwc_blob_key(pubkey: &str) -> String {
    format!("agent-nwc:{pubkey}")
}

/// Per-pubkey gates so concurrent provision/unprovision for one agent cannot
/// interleave probe + keyring write. Cross-agent ops stay concurrent.
#[derive(Default)]
pub struct AgentNwcOpGates {
    inner: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl AgentNwcOpGates {
    /// Empty gate map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the exclusive gate for `pubkey` (hold across probe + store/delete).
    pub async fn lock(&self, pubkey: &str) -> OwnedMutexGuard<()> {
        let gate = {
            let mut map = self.inner.lock().await;
            Arc::clone(
                map.entry(pubkey.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        gate.lock_owned().await
    }
}

/// Keyring/config presence for a managed agent's NWC wallet — no network, no URI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWalletStatus {
    /// `true` when `agent-nwc:{pubkey}` holds a non-empty NWC URI.
    pub provisioned: bool,
}

/// One keyring observation for spawn: URI for env injection + presence for hash.
///
/// By construction [`Self::provisioned`] == [`Self::uri`].is_some(). Callers
/// MUST use this single value for both `BUZZ_NWC_URI` and `spawn_config_hash`
/// so the stamped bit describes the same observation that decided the child env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNwcSpawnObservation {
    /// Non-empty URI to inject, or `None` to clear inherited `BUZZ_NWC_URI`.
    pub uri: Option<String>,
    /// Presence bit for `spawn_config_hash` — always `uri.is_some()`.
    pub provisioned: bool,
}

/// Resolve wallet status from keyring presence only (never probes NWC).
///
/// Propagates keyring load failures as `Err("agent_wallet_secret_unavailable")`
/// so the UI can distinguish "no wallet" from "keyring unavailable".
pub fn agent_wallet_status_for(
    pubkey: &str,
    backend: &dyn BlobBackend,
) -> Result<AgentWalletStatus, String> {
    let uri = observe_agent_nwc_uri(pubkey, backend)?;
    Ok(AgentWalletStatus {
        provisioned: uri.is_some(),
    })
}

/// Honest keyring observe: `Ok(Some(uri))` / `Ok(None)` / `Err(unavailable)`.
///
/// Prefer this over [`agent_nwc_spawn_uri`], which fail-closes errors to `None`.
pub fn observe_agent_nwc_uri(
    pubkey: &str,
    backend: &dyn BlobBackend,
) -> Result<Option<String>, String> {
    match load_agent_nwc_uri(pubkey, backend)? {
        Some(uri) if !uri.is_empty() => Ok(Some(uri)),
        _ => Ok(None),
    }
}

/// Single spawn-time observation: fail-closed on keyring Err.
///
/// Policy: a load failure withholds the capability (`uri = None`,
/// `provisioned = false`) rather than aborting spawn or inventing presence.
/// The returned struct is the only input for both env injection and the hash
/// stamp — never re-read the keyring for the stamp.
pub fn observe_agent_nwc_for_spawn(
    pubkey: &str,
    backend: &dyn BlobBackend,
) -> AgentNwcSpawnObservation {
    // Fail-closed on load Err: withhold BUZZ_NWC_URI; stamp matches that decision.
    let uri = observe_agent_nwc_uri(pubkey, backend).unwrap_or_default();
    let provisioned = uri.is_some();
    AgentNwcSpawnObservation { uri, provisioned }
}

/// `true` when a successful observe would inject `BUZZ_NWC_URI`.
///
/// Propagates keyring errors — do not use for IPC status (use
/// [`agent_wallet_status_for`]) or for spawn stamping (use
/// [`observe_agent_nwc_for_spawn`]).
pub fn agent_nwc_is_provisioned(pubkey: &str, backend: &dyn BlobBackend) -> Result<bool, String> {
    Ok(observe_agent_nwc_uri(pubkey, backend)?.is_some())
}

/// Probe both NWC info surfaces for agent provisioning.
#[async_trait]
pub trait AdvertisementProbe: Send + Sync {
    /// Return raw `13194` / `get_info` advertisements for `uri`.
    async fn probe(&self, uri: &str) -> Result<WalletAdvertisement, WalletError>;
}

#[async_trait]
impl AdvertisementProbe for NwcWalletConnector {
    async fn probe(&self, uri: &str) -> Result<WalletAdvertisement, WalletError> {
        self.probe_advertisement(uri).await.map(|(ads, _lud16)| ads)
    }
}

#[async_trait]
impl AdvertisementProbe for buzz_wallet_pkg::fakes::FakeAdvertisementProbe {
    async fn probe(&self, uri: &str) -> Result<WalletAdvertisement, WalletError> {
        self.respond(uri).await
    }
}

/// Validate receive-only: refuse spend on the **union**, require methods on
/// the **intersection**. Errors name the capability class — never the URI.
pub fn validate_agent_receive_only(ads: &WalletAdvertisement) -> Result<Capabilities, String> {
    let union = ads.union();
    for method in AGENT_SPEND_METHODS {
        if union.contains(*method) {
            return Err(format!(
                "agent_wallet_refused: spend capability advertised ({})",
                method.as_str()
            ));
        }
    }
    let intersection = ads.intersection();
    for method in AGENT_REQUIRED_METHODS {
        if !intersection.contains(*method) {
            return Err(format!(
                "agent_wallet_refused: missing required capability ({})",
                method.as_str()
            ));
        }
    }
    Ok(intersection)
}

/// Probe + validate + store.
///
/// On probe failure or receive-only refusal the keyring is not written for
/// *this* attempt. A previously successful provision is left in place —
/// refusal on re-provision does not wipe the last good URI (call
/// [`unprovision_agent_nwc`] explicitly to clear). Spec-compatible: the gate
/// prevents storing a spend-capable secret; it does not imply transactional
/// replace-or-rollback of an existing receive-only blob.
pub async fn provision_agent_nwc(
    pubkey: &str,
    uri: &str,
    probe: &dyn AdvertisementProbe,
    backend: &dyn BlobBackend,
) -> Result<Capabilities, String> {
    let key = agent_nwc_blob_key(pubkey);
    let ads = probe.probe(uri).await.map_err(map_probe_error)?;
    let caps = validate_agent_receive_only(&ads)?;
    // Store only after the gate passes — refusal writes nothing new.
    backend
        .store(&key, uri)
        .map_err(|_| "agent_wallet_secret_unavailable".to_string())?;
    Ok(caps)
}

/// Delete the agent's NWC URI from the keyring (missing is ok).
pub fn unprovision_agent_nwc(pubkey: &str, backend: &dyn BlobBackend) -> Result<(), String> {
    backend
        .delete(&agent_nwc_blob_key(pubkey))
        .map_err(|_| "agent_wallet_secret_unavailable".to_string())
}

/// Load the provisioned URI, if any.
pub fn load_agent_nwc_uri(
    pubkey: &str,
    backend: &dyn BlobBackend,
) -> Result<Option<String>, String> {
    backend
        .load(&agent_nwc_blob_key(pubkey))
        .map_err(|_| "agent_wallet_secret_unavailable".to_string())
}

/// Spawn-env injection helper that **fail-closes** keyring errors to `None`.
///
/// Prefer [`observe_agent_nwc_for_spawn`] at spawn sites so env + hash stamp
/// share one observation. This collapsing helper remains for redaction/tests
/// that only need a best-effort URI and must not surface keyring errors.
pub fn agent_nwc_spawn_uri(pubkey: &str, backend: &dyn BlobBackend) -> Option<String> {
    observe_agent_nwc_for_spawn(pubkey, backend).uri
}

/// Values that must be scrubbed from agent logs for a stored URI.
///
/// Includes the full URI and the bare `secret=` value (percent-decoded when
/// needed) so a leaked secret without the scheme prefix is still redacted.
pub fn agent_nwc_redaction_extras(stored_uri: &str) -> Vec<String> {
    let mut extras = Vec::new();
    if stored_uri.len() >= 4 {
        extras.push(stored_uri.to_string());
    }
    if let Some(secret) = nwc_secret_query_value(stored_uri) {
        if secret.len() >= 4 && secret != stored_uri {
            extras.push(secret);
        }
    }
    extras
}

/// Load keyring-derived NWC redaction extras for `pubkey` (empty if none).
///
/// Used by the agent-log read-tail path and by runtime when populating
/// `last_error` from a log — both feed the same [`crate::managed_agents::redact_secrets_with`].
pub fn agent_nwc_redaction_extras_for_pubkey(pubkey: &str) -> Vec<String> {
    match load_agent_nwc_uri(pubkey, &agent_nwc_os_backend()) {
        Ok(Some(uri)) => agent_nwc_redaction_extras(&uri),
        _ => Vec::new(),
    }
}

/// Extract the `secret` query parameter from an NWC URI (best-effort).
///
/// Percent-decodes the value so a bare hex leak still matches when the URI
/// stored the secret as `%XX`-encoded.
fn nwc_secret_query_value(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    for part in query.split('&') {
        if let Some(value) = part.strip_prefix("secret=") {
            let raw = value.split('#').next().unwrap_or(value);
            if raw.is_empty() {
                continue;
            }
            let decoded = percent_decode_component(raw);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

/// Minimal `%XX` decoder for a query component (no new dependency).
fn percent_decode_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn map_probe_error(err: WalletError) -> String {
    // Enumerated codes only — never Debug/Display that might carry the URI.
    match err {
        WalletError::InvalidUri => "agent_wallet_invalid_uri".to_string(),
        WalletError::Unreachable => "agent_wallet_unreachable".to_string(),
        WalletError::Unauthorized => "agent_wallet_unauthorized".to_string(),
        WalletError::Unsupported => "agent_wallet_unsupported".to_string(),
        WalletError::InsufficientBalance => "agent_wallet_insufficient_balance".to_string(),
        WalletError::QuotaExceeded => "agent_wallet_quota_exceeded".to_string(),
        WalletError::PaymentFailed => "agent_wallet_payment_failed".to_string(),
        WalletError::ResolveRejected => "agent_wallet_resolve_rejected".to_string(),
        WalletError::ResolveFailed => "agent_wallet_resolve_failed".to_string(),
        WalletError::SecretUnavailable => "agent_wallet_secret_unavailable".to_string(),
        WalletError::Unknown => "agent_wallet_unknown".to_string(),
    }
}

/// Production timeouts for agent NWC probes.
pub fn agent_nwc_timeouts() -> WalletTimeouts {
    WalletTimeouts {
        connect: Duration::from_secs(30),
        pay_response: Duration::from_secs(60),
    }
}

/// Production keyring backend for agent NWC blobs.
pub fn agent_nwc_os_backend() -> crate::wallet::secret_store::OsBlobBackend {
    crate::wallet::secret_store::OsBlobBackend
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::secret_store::MapBlobBackend;
    use buzz_wallet_pkg::fakes::FakeAdvertisementProbe;
    use std::sync::Arc;

    const URI: &str =
        "nostr+walletconnect://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?relay=wss://relay.example&secret=deadbeefcafebabe0123456789abcdef";
    const PUBKEY: &str = "agentpubkey001";

    fn receive_only_ads() -> WalletAdvertisement {
        Capabilities::from_advertisement_pair(
            ["make_invoice", "lookup_invoice", "get_balance"],
            ["make_invoice", "lookup_invoice", "get_balance"],
        )
    }

    fn spend_ads(spend: &str) -> WalletAdvertisement {
        Capabilities::from_advertisement_pair(
            ["make_invoice", "lookup_invoice", spend],
            ["make_invoice", "lookup_invoice", spend],
        )
    }

    #[test]
    fn validate_refuses_pay_invoice_on_union() {
        let ads = spend_ads("pay_invoice");
        let err = validate_agent_receive_only(&ads).expect_err("must refuse");
        assert!(err.contains("pay_invoice"), "{err}");
        assert!(!err.contains("nostr+walletconnect"), "{err}");
        assert!(!err.contains("deadbeef"), "{err}");
    }

    #[test]
    fn validate_refuses_spend_advertised_on_only_one_surface() {
        // Union catch: 13194 alone advertises pay_keysend.
        let ads = WalletAdvertisement {
            methods_13194: Capabilities::parse(["make_invoice", "lookup_invoice", "pay_keysend"]),
            get_info_methods: Capabilities::parse(["make_invoice", "lookup_invoice"]),
        };
        let err = validate_agent_receive_only(&ads).expect_err("union must refuse");
        assert!(err.contains("pay_keysend"), "{err}");
    }

    #[test]
    fn validate_refuses_multi_pay_variants() {
        for method in ["multi_pay_invoice", "multi_pay_keysend"] {
            let err = validate_agent_receive_only(&spend_ads(method)).expect_err(method);
            assert!(err.contains(method), "{err}");
        }
    }

    #[test]
    fn validate_refuses_missing_required() {
        let ads = Capabilities::from_advertisement_pair(
            ["make_invoice"],
            ["make_invoice"], // lookup_invoice missing from intersection
        );
        let err = validate_agent_receive_only(&ads).expect_err("missing lookup");
        assert!(err.contains("lookup_invoice"), "{err}");

        let ads = Capabilities::from_advertisement_pair(["lookup_invoice"], ["lookup_invoice"]);
        let err = validate_agent_receive_only(&ads).expect_err("missing make");
        assert!(err.contains("make_invoice"), "{err}");
    }

    #[test]
    fn validate_accepts_receive_only() {
        let caps = validate_agent_receive_only(&receive_only_ads()).expect("ok");
        assert!(caps.contains(WalletMethod::MakeInvoice));
        assert!(caps.contains(WalletMethod::LookupInvoice));
        assert!(!caps.contains(WalletMethod::PayInvoice));
    }

    #[tokio::test]
    async fn provision_refuses_spend_and_stores_nothing() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(spend_ads("pay_invoice"));
        let err = provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect_err("refuse");
        assert!(err.contains("pay_invoice"), "{err}");
        assert!(
            backend.keys().is_empty(),
            "keyring must be untouched: {:?}",
            backend.keys()
        );
    }

    #[tokio::test]
    async fn provision_probe_error_stores_nothing() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_err(WalletError::Unreachable);
        let err = provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect_err("probe fail");
        assert_eq!(err, "agent_wallet_unreachable");
        assert!(
            backend.keys().is_empty(),
            "keyring must be untouched on probe Err: {:?}",
            backend.keys()
        );
        assert!(
            backend.get_raw(&agent_nwc_blob_key(PUBKEY)).is_none(),
            "must not create agent-nwc:{{pubkey}} entry"
        );
    }

    #[tokio::test]
    async fn provision_refuses_missing_required_and_stores_nothing() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(Capabilities::from_advertisement_pair(
            ["get_balance"],
            ["get_balance"],
        ));
        let err = provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect_err("refuse");
        assert!(err.contains("missing required"), "{err}");
        assert!(backend.keys().is_empty());
    }

    #[tokio::test]
    async fn provision_success_stores_at_agent_nwc_key_and_unprovision_deletes() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect("provision");
        let key = agent_nwc_blob_key(PUBKEY);
        assert_eq!(backend.get_raw(&key).as_deref(), Some(URI));
        unprovision_agent_nwc(PUBKEY, &backend).expect("unprovision");
        assert!(backend.get_raw(&key).is_none());
    }

    #[tokio::test]
    async fn spawn_uri_present_only_when_provisioned() {
        let backend = Arc::new(MapBlobBackend::new());
        assert!(agent_nwc_spawn_uri(PUBKEY, backend.as_ref()).is_none());
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc(PUBKEY, URI, &probe, backend.as_ref())
            .await
            .expect("provision");
        assert_eq!(
            agent_nwc_spawn_uri(PUBKEY, backend.as_ref()).as_deref(),
            Some(URI)
        );
    }

    #[test]
    fn redaction_extras_include_uri_and_bare_secret() {
        let extras = agent_nwc_redaction_extras(URI);
        assert!(extras.iter().any(|e| e == URI));
        assert!(extras
            .iter()
            .any(|e| e == "deadbeefcafebabe0123456789abcdef"));
    }

    #[test]
    fn redaction_extras_percent_decode_secret() {
        // secret=deadbeef… with 'a' encoded as %61 → still yields bare hex for scrubbing.
        let encoded_uri = "nostr+walletconnect://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?relay=wss://relay.example&secret=de%61dbeefcafebabe0123456789abcdef";
        let extras = agent_nwc_redaction_extras(encoded_uri);
        assert!(
            extras
                .iter()
                .any(|e| e == "deadbeefcafebabe0123456789abcdef"),
            "expected decoded bare secret in extras: {extras:?}"
        );
    }

    #[test]
    fn blob_key_format() {
        assert_eq!(agent_nwc_blob_key("abc"), "agent-nwc:abc");
    }

    #[test]
    fn status_unprovisioned_is_false() {
        let backend = MapBlobBackend::new();
        assert!(!agent_nwc_is_provisioned(PUBKEY, &backend).expect("load"));
        assert_eq!(
            agent_wallet_status_for(PUBKEY, &backend).expect("status"),
            AgentWalletStatus { provisioned: false }
        );
    }

    #[tokio::test]
    async fn status_provisioned_is_true_after_store() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect("provision");
        assert!(agent_nwc_is_provisioned(PUBKEY, &backend).expect("load"));
        assert_eq!(
            agent_wallet_status_for(PUBKEY, &backend).expect("status"),
            AgentWalletStatus { provisioned: true }
        );
    }

    /// Keyring load Err must surface — never collapse to provisioned=false.
    #[test]
    fn status_propagates_keyring_unavailable() {
        struct FailBlob;
        impl BlobBackend for FailBlob {
            fn load(&self, _: &str) -> Result<Option<String>, String> {
                Err("boom".into())
            }
            fn store(&self, _: &str, _: &str) -> Result<(), String> {
                Err("boom".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Err("boom".into())
            }
        }
        let err = agent_wallet_status_for(PUBKEY, &FailBlob).expect_err("must Err");
        assert_eq!(err, "agent_wallet_secret_unavailable");
    }

    /// Defect-1 invariant: spawn observation's presence bit is exactly uri.is_some().
    #[tokio::test]
    async fn spawn_observation_presence_matches_uri_decision() {
        let backend = MapBlobBackend::new();
        let empty = observe_agent_nwc_for_spawn(PUBKEY, &backend);
        assert_eq!(empty.provisioned, empty.uri.is_some());
        assert!(!empty.provisioned);

        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect("provision");
        let provisioned = observe_agent_nwc_for_spawn(PUBKEY, &backend);
        assert_eq!(provisioned.provisioned, provisioned.uri.is_some());
        assert!(provisioned.provisioned);
        assert_eq!(provisioned.uri.as_deref(), Some(URI));

        // Fail-closed: load Err → no URI and provisioned=false (same decision).
        struct FailBlob;
        impl BlobBackend for FailBlob {
            fn load(&self, _: &str) -> Result<Option<String>, String> {
                Err("boom".into())
            }
            fn store(&self, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let failed = observe_agent_nwc_for_spawn(PUBKEY, &FailBlob);
        assert_eq!(failed.provisioned, failed.uri.is_some());
        assert!(!failed.provisioned);
        assert!(failed.uri.is_none());
    }

    /// managed-agents.json must never gain the URI after provisioning.
    #[tokio::test]
    async fn provision_never_touches_managed_agents_json_shape() {
        use crate::managed_agents::{ManagedAgentRecord, RespondTo};
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc(PUBKEY, URI, &probe, &backend)
            .await
            .expect("provision");

        // Simulate a persisted agent record (what save_managed_agents writes).
        let record = ManagedAgentRecord {
            pubkey: PUBKEY.to_string(),
            name: "test-agent".into(),
            persona_id: None,
            private_key_nsec: String::new(),
            auth_tag: None,
            relay_url: "ws://localhost:3000".into(),
            avatar_url: None,
            acp_command: "buzz-acp".into(),
            agent_command: "goose".into(),
            agent_command_override: None,
            agent_args: vec![],
            mcp_command: String::new(),
            turn_timeout_seconds: 320,
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
            parallelism: 1,
            system_prompt: None,
            model: None,
            provider: None,
            persona_source_version: None,
            env_vars: std::collections::BTreeMap::new(),
            start_on_app_launch: false,
            auto_restart_on_config_change: true,
            runtime_pid: None,
            backend: Default::default(),
            backend_agent_id: None,
            provider_binary_path: None,
            team_id: None,
            persona_team_dir: None,
            persona_name_in_team: None,
            created_at: "now".into(),
            updated_at: "now".into(),
            last_started_at: None,
            last_stopped_at: None,
            last_exit_code: None,
            last_error: None,
            last_error_code: None,
            respond_to: RespondTo::OwnerOnly,
            respond_to_allowlist: vec![],
            display_name: None,
            slug: None,
            runtime: None,
            name_pool: Vec::new(),
            is_builtin: false,
            is_active: true,
            source_team: None,
            source_team_persona_slug: None,
            definition_respond_to: None,
            definition_respond_to_allowlist: Vec::new(),
            definition_parallelism: None,
            relay_mesh: None,
        };
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(
            !json.contains("nostr+walletconnect"),
            "URI leaked into agent JSON"
        );
        assert!(!json.contains("deadbeef"), "secret leaked into agent JSON");
        assert!(!json.contains(URI), "full URI leaked into agent JSON");
        // Secret lives only in the keyring backend.
        assert_eq!(
            backend.get_raw(&agent_nwc_blob_key(PUBKEY)).as_deref(),
            Some(URI)
        );
    }
}

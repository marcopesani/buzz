use tauri::{AppHandle, Manager};

use crate::{
    app_state::AppState,
    managed_agents::{
        load_managed_agents, managed_agent_log_path, read_log_tail, redact_secrets_with,
        BackendKind, ManagedAgentLogResponse,
    },
};

/// Redact an agent log tail: scheme-prefix scrubbing plus the exact stored
/// NWC URI / bare secret for this agent (when provisioned).
pub(crate) fn redact_agent_log_tail(pubkey: &str, raw: &str) -> String {
    let owned_extras = crate::wallet::agent_nwc_redaction_extras_for_pubkey(pubkey);
    let refs: Vec<&str> = owned_extras.iter().map(String::as_str).collect();
    redact_secrets_with(raw, &refs)
}

#[tauri::command]
pub async fn get_managed_agent_log(
    pubkey: String,
    line_count: Option<u32>,
    app: AppHandle,
) -> Result<ManagedAgentLogResponse, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _store_guard = state
            .managed_agents_store_lock
            .lock()
            .map_err(|error| error.to_string())?;
        let records = load_managed_agents(&app)?;
        let record = records
            .iter()
            .find(|record| record.pubkey == pubkey)
            .ok_or_else(|| format!("agent {pubkey} not found"))?;
        if record.backend != BackendKind::Local {
            return Err("logs are not available for remote agents".to_string());
        }

        let log_path = managed_agent_log_path(&app, &pubkey)?;
        let raw = read_log_tail(&log_path, line_count.unwrap_or(120) as usize)?;
        Ok(ManagedAgentLogResponse {
            content: redact_agent_log_tail(&pubkey, &raw),
            log_path: log_path.display().to_string(),
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::{
        agent_nwc_blob_key, agent_nwc_redaction_extras, provision_agent_nwc, MapBlobBackend,
    };
    use buzz_wallet_pkg::fakes::FakeAdvertisementProbe;
    use buzz_wallet_pkg::{Capabilities, WalletAdvertisement};

    const URI: &str =
        "nostr+walletconnect://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?relay=wss://relay.example&secret=deadbeefcafebabe0123456789abcdef";

    fn receive_only_ads() -> WalletAdvertisement {
        Capabilities::from_advertisement_pair(
            ["make_invoice", "lookup_invoice"],
            ["make_invoice", "lookup_invoice"],
        )
    }

    #[test]
    fn read_tail_path_redacts_uri_via_prefix() {
        let raw = format!("wallet uri={URI} connected");
        // No keyring load in this unit — prefix scrub alone must catch the URI.
        let redacted = redact_secrets_with(&raw, &[]);
        assert!(!redacted.contains("nostr+walletconnect"));
        assert!(!redacted.contains("deadbeefcafebabe0123456789abcdef"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn read_tail_path_redacts_bare_secret_via_extras() {
        let secret = "deadbeefcafebabe0123456789abcdef";
        let raw = format!("secret only: {secret}");
        let extras = agent_nwc_redaction_extras(URI);
        let refs: Vec<&str> = extras.iter().map(String::as_str).collect();
        let redacted = redact_secrets_with(&raw, &refs);
        assert!(!redacted.contains(secret));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn backend_and_read_tail_share_redaction() {
        // Same function, both paths: backend deploy stderr + log read-tail.
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc("agentpk", URI, &probe, &backend)
            .await
            .expect("provision");
        let stored = backend
            .get_raw(&agent_nwc_blob_key("agentpk"))
            .expect("stored");
        let extras = agent_nwc_redaction_extras(&stored);
        let refs: Vec<&str> = extras.iter().map(String::as_str).collect();

        let line_uri = format!("NWC={URI}");
        let line_bare = "bare=deadbeefcafebabe0123456789abcdef";
        let from_backend = redact_secrets_with(&line_uri, &refs);
        let from_tail = redact_secrets_with(line_bare, &refs);
        assert!(!from_backend.contains("nostr+walletconnect"));
        assert!(!from_tail.contains("deadbeefcafebabe0123456789abcdef"));
    }
}

//! Managed-agent NWC wallet provisioning — receive-only URI store/remove.
//!
//! Split from `agents.rs` (file-size guard). Agent lifecycle stays in
//! `agents.rs`; this module owns the wallet provision/unprovision Tauri
//! commands that probe NWC capabilities and store URIs in the keyring.

use tauri::{AppHandle, State};

use crate::{app_state::AppState, managed_agents::load_managed_agents};

/// Provision a receive-only NWC wallet for a managed agent.
///
/// Probes capabilities via NWC; refuses (and stores nothing) if any spend
/// method is advertised on either info surface, or if `make_invoice` /
/// `lookup_invoice` are missing. On success stores the URI at
/// `agent-nwc:{pubkey}` in the keyring — never in `managed-agents.json`.
#[tauri::command]
pub async fn provision_managed_agent_wallet(
    pubkey: String,
    uri: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let pubkey = pubkey.trim().to_string();
    if pubkey.is_empty() {
        return Err("agent pubkey is required".to_string());
    }
    let uri = uri.trim().to_string();
    if uri.is_empty() {
        return Err("nwc uri is required".to_string());
    }

    // Confirm the agent exists before probing/storing.
    {
        let _store_guard = state
            .managed_agents_store_lock
            .lock()
            .map_err(|error| error.to_string())?;
        let records = load_managed_agents(&app)?;
        if !records.iter().any(|r| r.pubkey == pubkey) {
            return Err(format!("agent {pubkey} not found"));
        }
    }

    let connector = buzz_wallet_pkg::NwcWalletConnector::new(crate::wallet::agent_nwc_timeouts());
    let backend = crate::wallet::agent_nwc_os_backend();
    crate::wallet::provision_agent_nwc(&pubkey, &uri, &connector, &backend)
        .await
        .map(|_| ())
}

/// Remove a managed agent's NWC URI from the keyring.
#[tauri::command]
pub async fn unprovision_managed_agent_wallet(
    pubkey: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let pubkey = pubkey.trim().to_string();
    if pubkey.is_empty() {
        return Err("agent pubkey is required".to_string());
    }
    {
        let _store_guard = state
            .managed_agents_store_lock
            .lock()
            .map_err(|error| error.to_string())?;
        let records = load_managed_agents(&app)?;
        if !records.iter().any(|r| r.pubkey == pubkey) {
            return Err(format!("agent {pubkey} not found"));
        }
    }
    let backend = crate::wallet::agent_nwc_os_backend();
    crate::wallet::unprovision_agent_nwc(&pubkey, &backend)
}

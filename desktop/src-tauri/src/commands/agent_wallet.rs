//! Managed-agent NWC wallet provisioning — receive-only URI store/remove.
//!
//! Split from `agents.rs` (file-size guard). Agent lifecycle stays in
//! `agents.rs`; this module owns the wallet provision/unprovision/status Tauri
//! commands that probe NWC capabilities and store URIs in the keyring.

use tauri::{AppHandle, State};

use crate::{app_state::AppState, managed_agents::load_managed_agents, wallet::AgentWalletStatus};

/// Provision a receive-only NWC wallet for a managed agent.
///
/// Probes capabilities via NWC; refuses (and stores nothing) if any spend
/// method is advertised on either info surface, or if `make_invoice` /
/// `lookup_invoice` are missing. On success stores the URI at
/// `agent-nwc:{pubkey}` in the keyring — never in `managed-agents.json`.
///
/// Serialized per pubkey with [`AppState::agent_nwc_op_gates`] so concurrent
/// provision/unprovision for the same agent cannot interleave.
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

    let _gate = state.agent_nwc_op_gates.lock(&pubkey).await;
    let connector = buzz_wallet_pkg::NwcWalletConnector::new(crate::wallet::agent_nwc_timeouts());
    let backend = crate::wallet::agent_nwc_os_backend();
    crate::wallet::provision_agent_nwc(&pubkey, &uri, &connector, &backend)
        .await
        .map(|_| ())
}

/// Remove a managed agent's NWC URI from the keyring.
///
/// Serialized per pubkey with [`AppState::agent_nwc_op_gates`].
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
    let _gate = state.agent_nwc_op_gates.lock(&pubkey).await;
    let backend = crate::wallet::agent_nwc_os_backend();
    crate::wallet::unprovision_agent_nwc(&pubkey, &backend)
}

/// Keyring/config presence for a managed agent's NWC wallet.
///
/// Does **not** probe NWC — presence against the keyring only. Unknown or
/// unprovisioned agents return `{ "provisioned": false }`. Keyring load
/// failure returns `Err("agent_wallet_secret_unavailable")` (not a false
/// negative). Never returns the URI.
#[tauri::command]
pub async fn agent_wallet_status(pubkey: String) -> Result<AgentWalletStatus, String> {
    let pubkey = pubkey.trim().to_string();
    if pubkey.is_empty() {
        return Err("agent pubkey is required".to_string());
    }
    let backend = crate::wallet::agent_nwc_os_backend();
    crate::wallet::agent_wallet_status_for(&pubkey, &backend)
}

#[cfg(test)]
mod tests {
    use crate::wallet::{
        agent_nwc_blob_key, agent_wallet_status_for, provision_agent_nwc, unprovision_agent_nwc,
        AdvertisementProbe, AgentNwcOpGates, AgentWalletStatus, MapBlobBackend,
    };
    use async_trait::async_trait;
    use buzz_wallet_pkg::fakes::FakeAdvertisementProbe;
    use buzz_wallet_pkg::{Capabilities, WalletAdvertisement, WalletError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::{Mutex, Notify};

    const URI: &str =
        "nostr+walletconnect://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?relay=wss://relay.example&secret=deadbeefcafebabe0123456789abcdef";

    fn receive_only_ads() -> WalletAdvertisement {
        Capabilities::from_advertisement_pair(
            ["make_invoice", "lookup_invoice", "get_balance"],
            ["make_invoice", "lookup_invoice", "get_balance"],
        )
    }

    #[test]
    fn status_false_for_unknown_and_unprovisioned() {
        let backend = MapBlobBackend::new();
        assert_eq!(
            agent_wallet_status_for("unknown-agent", &backend).expect("status"),
            AgentWalletStatus { provisioned: false }
        );
        assert_eq!(
            agent_wallet_status_for("agentpk", &backend).expect("status"),
            AgentWalletStatus { provisioned: false }
        );
    }

    #[tokio::test]
    async fn status_true_after_provisioning() {
        let backend = MapBlobBackend::new();
        let probe = FakeAdvertisementProbe::new();
        probe.script_ok(receive_only_ads());
        provision_agent_nwc("agentpk", URI, &probe, &backend)
            .await
            .expect("provision");
        assert_eq!(
            agent_wallet_status_for("agentpk", &backend).expect("status"),
            AgentWalletStatus { provisioned: true }
        );
        unprovision_agent_nwc("agentpk", &backend).expect("unprovision");
        assert_eq!(
            agent_wallet_status_for("agentpk", &backend).expect("status"),
            AgentWalletStatus { provisioned: false }
        );
    }

    #[test]
    fn status_ipc_surfaces_keyring_unavailable() {
        struct FailBlob;
        impl crate::wallet::BlobBackend for FailBlob {
            fn load(&self, _: &str) -> Result<Option<String>, String> {
                Err("locked".into())
            }
            fn store(&self, _: &str, _: &str) -> Result<(), String> {
                Err("locked".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Err("locked".into())
            }
        }
        let err = agent_wallet_status_for("agentpk", &FailBlob).expect_err("Err");
        assert_eq!(err, "agent_wallet_secret_unavailable");
    }

    #[test]
    fn status_json_shape_is_provisioned_bool() {
        let json =
            serde_json::to_string(&AgentWalletStatus { provisioned: true }).expect("serialize");
        assert_eq!(json, r#"{"provisioned":true}"#);
    }

    /// Concurrent provision/unprovision for one pubkey must not interleave:
    /// the second op cannot acquire the gate until the first exits.
    #[tokio::test]
    async fn same_agent_ops_serialize_without_interleaving() {
        let gates = Arc::new(AgentNwcOpGates::new());
        let backend = Arc::new(MapBlobBackend::new());
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let release_first = Arc::new(Notify::new());
        let first_entered = Arc::new(Notify::new());

        /// Probe that parks inside the critical section until released.
        struct ParkingProbe {
            ads: WalletAdvertisement,
            order: Arc<Mutex<Vec<&'static str>>>,
            in_flight: Arc<AtomicUsize>,
            max_in_flight: Arc<AtomicUsize>,
            entered: Arc<Notify>,
            release: Arc<Notify>,
        }

        #[async_trait]
        impl AdvertisementProbe for ParkingProbe {
            async fn probe(&self, _uri: &str) -> Result<WalletAdvertisement, WalletError> {
                {
                    let mut log = self.order.lock().await;
                    log.push("probe_enter");
                }
                let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight.fetch_max(now, Ordering::SeqCst);
                self.entered.notify_one();
                self.release.notified().await;
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                {
                    let mut log = self.order.lock().await;
                    log.push("probe_exit");
                }
                Ok(self.ads.clone())
            }
        }

        let pubkey = "serialize-agent";
        let gates_a = Arc::clone(&gates);
        let backend_a = Arc::clone(&backend);
        let order_a = Arc::clone(&order);
        let in_flight_a = Arc::clone(&in_flight);
        let max_a = Arc::clone(&max_in_flight);
        let entered_a = Arc::clone(&first_entered);
        let release_a = Arc::clone(&release_first);
        let ads = receive_only_ads();

        let t1 = tokio::spawn(async move {
            let _gate = gates_a.lock(pubkey).await;
            let probe = ParkingProbe {
                ads,
                order: order_a,
                in_flight: in_flight_a,
                max_in_flight: max_a,
                entered: entered_a,
                release: release_a,
            };
            provision_agent_nwc(pubkey, URI, &probe, backend_a.as_ref())
                .await
                .expect("t1 provision");
        });

        first_entered.notified().await;

        let gates_b = Arc::clone(&gates);
        let backend_b = Arc::clone(&backend);
        let order_b = Arc::clone(&order);
        let t2 = tokio::spawn(async move {
            let _gate = gates_b.lock(pubkey).await;
            {
                let mut log = order_b.lock().await;
                log.push("t2_gate_acquired");
            }
            unprovision_agent_nwc(pubkey, backend_b.as_ref()).expect("t2 unprovision");
        });

        // While t1 is parked inside probe, t2 must not have acquired the gate.
        {
            let log = order.lock().await;
            assert_eq!(log.as_slice(), &["probe_enter"]);
            assert!(!log.contains(&"t2_gate_acquired"), "t2 must wait: {log:?}");
        }
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);

        release_first.notify_one();
        t1.await.expect("t1 join");
        t2.await.expect("t2 join");

        let log = order.lock().await;
        assert_eq!(
            log.as_slice(),
            &["probe_enter", "probe_exit", "t2_gate_acquired"],
            "ops must be strictly ordered: {log:?}"
        );
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
        assert!(
            backend.get_raw(&agent_nwc_blob_key(pubkey)).is_none(),
            "final state: unprovisioned"
        );
    }
}

//! [`ProfilePublisher`](buzz_wallet_pkg::ProfilePublisher) that merge-publishes
//! `lud16` into kind:0 via the existing relay query/submit path.

use async_trait::async_trait;
use buzz_wallet_pkg::{Kind0Fields, ProfilePublisher, WalletError};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::app_state::AppState;
use crate::events;
use crate::relay::{query_relay, submit_event};

/// Relay-backed profile publisher for wallet link/unlink.
pub struct RelayProfilePublisher {
    app: AppHandle,
}

impl RelayProfilePublisher {
    /// Create a publisher bound to `app`.
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn state(&self) -> Result<tauri::State<'_, AppState>, WalletError> {
        self.app
            .try_state::<AppState>()
            .ok_or(WalletError::Unreachable)
    }

    async fn read_kind0_content(&self) -> Result<Value, WalletError> {
        let state = self.state()?;
        let my_pubkey = {
            let keys = state.keys.lock().map_err(|_| WalletError::Unreachable)?;
            keys.public_key().to_hex()
        };
        let events = query_relay(
            &state,
            &[serde_json::json!({
                "kinds": [0],
                "authors": [my_pubkey],
                "limit": 1
            })],
        )
        .await
        .map_err(|_| WalletError::Unreachable)?;
        Ok(events
            .first()
            .and_then(|ev| serde_json::from_str::<Value>(&ev.content).ok())
            .unwrap_or(Value::Null))
    }

    /// Merge-publish kind:0, optionally setting or clearing `lud16`.
    ///
    /// When `clear_lud16` is true the field is omitted (relay absolute-state
    /// clears the column). When false, `fields.lud16` is applied if `Some`.
    pub async fn merge_publish_with_clear(
        &self,
        fields: Kind0Fields,
        clear_lud16: bool,
    ) -> Result<(), WalletError> {
        let state = self.state()?;
        let current = self.read_kind0_content().await?;

        let dn = current.get("display_name").and_then(Value::as_str);
        let name = current.get("name").and_then(Value::as_str);
        let picture = current.get("picture").and_then(Value::as_str);
        let about = current.get("about").and_then(Value::as_str);
        let nip05 = current.get("nip05").and_then(Value::as_str);

        let lud16: Option<&str> = if clear_lud16 {
            None
        } else if let Some(ref address) = fields.lud16 {
            Some(address.as_str())
        } else {
            current.get("lud16").and_then(Value::as_str)
        };

        let builder = events::build_profile(dn, name, picture, about, nip05, lud16)
            .map_err(|_| WalletError::Unreachable)?;
        submit_event(builder, &state)
            .await
            .map_err(|_| WalletError::Unreachable)?;
        Ok(())
    }

    /// Clear `lud16` from kind:0 (unlink path).
    pub async fn clear_lud16(&self) -> Result<(), WalletError> {
        self.merge_publish_with_clear(Kind0Fields::default(), true)
            .await
    }
}

#[async_trait]
impl ProfilePublisher for RelayProfilePublisher {
    async fn current_lud16(&self) -> Result<Option<String>, WalletError> {
        let current = self.read_kind0_content().await?;
        Ok(current
            .get("lud16")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    async fn merge_publish(&self, fields: Kind0Fields) -> Result<(), WalletError> {
        self.merge_publish_with_clear(fields, false).await
    }
}

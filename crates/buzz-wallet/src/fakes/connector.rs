//! Scriptable [`WalletConnector`](crate::ports::WalletConnector).

use crate::error::WalletError;
use crate::ports::{WalletConnector, WalletService};
use crate::types::Capabilities;
use async_trait::async_trait;
use std::fmt;
use std::future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Scripted outcome for one `connect` call.
pub enum ConnectorScript {
    /// Succeed with a service, capabilities, and optional lud16.
    Succeed {
        /// Service handed to the caller (usually a scripted [`crate::fakes::FakeWalletService`]).
        service: Arc<dyn WalletService>,
        /// Link-time capability set (`13194 ∩ get_info.methods`).
        capabilities: Capabilities,
        /// Lightning Address from the NWC URI query param, if any.
        lud16: Option<String>,
    },
    /// Fail immediately with this error (`InvalidUri` / `Unreachable` / …).
    Fail(WalletError),
    /// Sleep then continue (honours `tokio::time::pause`).
    Delay {
        /// How long to wait before continuing.
        duration: Duration,
        /// Outcome after the delay.
        then: Box<ConnectorScript>,
    },
    /// Pending future that never resolves (connect timeout scenarios).
    NeverRespond,
}

impl fmt::Debug for ConnectorScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Succeed {
                capabilities,
                lud16,
                ..
            } => f
                .debug_struct("Succeed")
                .field("capabilities", capabilities)
                .field("lud16", lud16)
                .finish_non_exhaustive(),
            Self::Fail(err) => f.debug_tuple("Fail").field(err).finish(),
            Self::Delay { duration, then } => f
                .debug_struct("Delay")
                .field("duration", duration)
                .field("then", then)
                .finish(),
            Self::NeverRespond => f.write_str("NeverRespond"),
        }
    }
}

/// Fake connector — Link scenarios drive this before a service exists.
#[derive(Debug, Default)]
pub struct FakeWalletConnector {
    scripts: Mutex<Vec<ConnectorScript>>,
    calls: Mutex<Vec<String>>,
}

impl FakeWalletConnector {
    /// Empty connector — each `connect` must be scripted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a connect outcome (FIFO).
    pub fn script(&self, script: ConnectorScript) {
        self.scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(script);
    }

    /// URIs passed to `connect`, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn pop(&self) -> ConnectorScript {
        let mut scripts = self.scripts.lock().unwrap_or_else(|e| e.into_inner());
        if scripts.is_empty() {
            panic!("FakeWalletConnector: connect called with no script queued");
        }
        scripts.remove(0)
    }
}

async fn run(
    script: ConnectorScript,
) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError> {
    match script {
        ConnectorScript::Succeed {
            service,
            capabilities,
            lud16,
        } => Ok((service, capabilities, lud16)),
        ConnectorScript::Fail(err) => Err(err),
        ConnectorScript::Delay { duration, then } => {
            tokio::time::sleep(duration).await;
            Box::pin(run(*then)).await
        }
        ConnectorScript::NeverRespond => {
            future::pending::<()>().await;
            unreachable!("NeverRespond pending resolved")
        }
    }
}

#[async_trait]
impl WalletConnector for FakeWalletConnector {
    async fn connect(
        &self,
        uri: &str,
    ) -> Result<(Arc<dyn WalletService>, Capabilities, Option<String>), WalletError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(uri.to_string());
        let script = self.pop();
        run(script).await
    }
}

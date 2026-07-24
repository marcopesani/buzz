//! Recording [`ProfilePublisher`](crate::ports::ProfilePublisher).

use crate::error::WalletError;
use crate::ports::ProfilePublisher;
use crate::types::Kind0Fields;
use async_trait::async_trait;
use std::sync::Mutex;

/// Seeded kind:0 snapshot used to prove merge-publish preserves other fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeededKind0 {
    /// Existing Lightning Address, if any.
    pub lud16: Option<String>,
    /// Example non-payment field — must survive a lud16 merge.
    pub display_name: Option<String>,
}

/// Records every merge-publish call for assertions.
///
/// Holds a seeded kind:0 so tests can prove `merge_publish` preserves
/// non-`lud16` fields (the port contract drivers must honour).
#[derive(Debug, Default)]
pub struct RecordingProfilePublisher {
    profile: Mutex<SeededKind0>,
    published: Mutex<Vec<Kind0Fields>>,
}

impl RecordingProfilePublisher {
    /// Empty recorder (no existing kind:0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed an existing kind:0 before link (for skip / preservation scenarios).
    pub fn seed(&self, profile: SeededKind0) {
        *self.profile.lock().unwrap_or_else(|e| e.into_inner()) = profile;
    }

    /// All published kind:0 field sets, in order.
    pub fn published(&self) -> Vec<Kind0Fields> {
        self.published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Current full kind:0 snapshot after merges (preservation assertions).
    pub fn profile(&self) -> SeededKind0 {
        self.profile
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl ProfilePublisher for RecordingProfilePublisher {
    async fn current_lud16(&self) -> Result<Option<String>, WalletError> {
        Ok(self
            .profile
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lud16
            .clone())
    }

    async fn merge_publish(&self, fields: Kind0Fields) -> Result<(), WalletError> {
        self.published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(fields.clone());
        // Merge semantics: only overwrite fields present in `fields`; keep the rest.
        let mut profile = self.profile.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lud16) = fields.lud16 {
            profile.lud16 = Some(lud16);
        }
        Ok(())
    }
}

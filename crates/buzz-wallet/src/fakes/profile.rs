//! Recording [`ProfilePublisher`](crate::ports::ProfilePublisher).

use crate::error::WalletError;
use crate::ports::ProfilePublisher;
use crate::types::Kind0Fields;
use async_trait::async_trait;
use std::sync::Mutex;

/// Records every merge-publish call for assertions.
#[derive(Debug, Default)]
pub struct RecordingProfilePublisher {
    published: Mutex<Vec<Kind0Fields>>,
}

impl RecordingProfilePublisher {
    /// Empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// All published kind:0 field sets, in order.
    pub fn published(&self) -> Vec<Kind0Fields> {
        self.published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl ProfilePublisher for RecordingProfilePublisher {
    async fn merge_publish(&self, fields: Kind0Fields) -> Result<(), WalletError> {
        self.published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(fields);
        Ok(())
    }
}

//! Production [`Clock`](crate::ports::Clock) backed by the system wall clock.

use crate::ports::Clock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall clock using [`SystemTime`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

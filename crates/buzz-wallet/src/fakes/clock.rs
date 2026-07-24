//! Controllable clock for expiry / timeout scenarios.

use crate::ports::Clock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Advanceable unix clock — nothing sleeps for expiry scenarios.
#[derive(Debug)]
pub struct FakeClock {
    now: AtomicU64,
}

impl FakeClock {
    /// Start at the given unix seconds.
    pub fn new(now_unix: u64) -> Self {
        Self {
            now: AtomicU64::new(now_unix),
        }
    }

    /// Advance the clock by `secs` seconds.
    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    /// Jump to an absolute unix timestamp.
    pub fn set(&self, now_unix: u64) {
        self.now.store(now_unix, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(1_700_000_000)
    }
}

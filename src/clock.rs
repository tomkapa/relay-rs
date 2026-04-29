//! Injectable clock per CLAUDE.md §11. Production code never calls `Instant::now` /
//! `SystemTime::now` directly — it goes through this trait so tests stay deterministic.

use std::fmt;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
    fn now_wall(&self) -> SystemTime;
}

/// Real wall-clock implementation. The only place in the codebase that should construct
/// this is `main` (or the integration-test harness wiring a full app).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn now_wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Shared, dynamically-dispatched clock handle so subsystems can hold one without taking
/// a generic parameter (which would cascade through every trait that touches time).
#[derive(Clone)]
pub struct SharedClock(Arc<dyn Clock>);

impl SharedClock {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self(clock)
    }

    #[must_use]
    pub fn system() -> Self {
        Self(Arc::new(SystemClock))
    }

    #[must_use]
    pub fn now(&self) -> Instant {
        self.0.now()
    }

    #[must_use]
    pub fn now_wall(&self) -> SystemTime {
        self.0.now_wall()
    }
}

impl fmt::Debug for SharedClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SharedClock").finish()
    }
}

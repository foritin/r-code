use chrono::{DateTime, Utc};

/// UTC clock injected into all scheduling and lease decisions.
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
}

/// Production clock. Tests and recovery simulations provide deterministic implementations.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

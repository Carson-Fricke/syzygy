//! Lease validity and heartbeat policy over caller-owned storage operations.

use std::time::{Duration, Instant};

pub fn lease_active(expires_at: f64, now: f64) -> bool {
    expires_at > now
}

/// Retry a busy mutation guard for a bounded interval. No attempt runs unless
/// the store returns a claim, and faults from an enclosing claim stop the wait.
pub fn acquire_mutation<T, E>(
    mut acquire: impl FnMut() -> Result<Option<T>, E>,
    mut wait: impl FnMut(Duration),
) -> Result<Option<T>, E> {
    let started = Instant::now();
    loop {
        if let Some(claim) = acquire()? {
            return Ok(Some(claim));
        }
        if started.elapsed() >= Duration::from_secs(30) {
            return Ok(None);
        }
        wait(Duration::from_millis(25));
    }
}

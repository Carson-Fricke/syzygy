//! Retry and dispatch decisions for one bounded agent session.
//!
//! The session host owns model invocations, trusted dispatch checks, durable
//! output and delivery. This state records repeated failure signatures and
//! chooses whether an already authorized operation should begin.

use std::collections::HashSet;

mod context;
mod output;

pub use context::{ContextEntry, group_ranges, retiring_boundary, select_ranked_groups};
pub use output::{Batch, OutputBuffer};

/// Wait cooperatively without holding a stale lease or replaying an operation.
pub fn wait_for_recovery<E>(
    mut remaining: f64,
    mut assert_owned: impl FnMut() -> Result<(), E>,
    mut stop: impl FnMut() -> Result<bool, E>,
    mut pause: impl FnMut(f64) -> Result<(), E>,
) -> Result<(), E> {
    while remaining > 0.0 {
        assert_owned()?;
        if stop()? {
            return Ok(());
        }
        let interval = remaining.min(0.25);
        pause(interval)?;
        remaining -= interval;
    }
    assert_owned()
}

/// Run the same bounded slice for root, helper, and phase conversations.
/// A settled attempt returns its one result. An open attempt re-enters the
/// same host boundary with the next iteration index.
pub fn run_slice<T, E>(
    limit: usize,
    mut run_iteration: impl FnMut(usize, bool) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    for iteration in 0..limit {
        if let Some(result) = run_iteration(iteration, iteration + 1 < limit)? {
            return Ok(Some(result));
        }
    }
    Ok(None)
}

/// The invocation's durable output commit precedes any input acknowledgement.
/// A sealed interruption retains its evidence and leaves the inputs pending.
pub fn seal_output<E>(
    interrupted: bool,
    persist: impl FnOnce() -> Result<(), E>,
    acknowledge: impl FnOnce() -> Result<(), E>,
) -> Result<bool, E> {
    persist()?;
    if interrupted {
        return Ok(false);
    }
    acknowledge()?;
    Ok(true)
}

/// Every requested call receives a receipt, even after completion or
/// cancellation disables dispatch. Accepted completion survives later
/// cancellation while the host is pairing the remaining requests.
pub fn pair_tool_results<R, T, E>(
    calls: impl IntoIterator<Item = R>,
    mut cancelled: bool,
    mut pair: impl FnMut(R, bool, bool) -> Result<(bool, Option<T>), E>,
) -> Result<(bool, Option<T>), E> {
    let mut completion = None;
    for request in calls {
        let (stopped, accepted) = pair(request, completion.is_some(), cancelled)?;
        cancelled |= stopped;
        if !cancelled && let Some(accepted) = accepted {
            completion = Some(accepted);
        }
    }
    Ok((cancelled, completion))
}

const RECOVERY_DELAYS: [f64; 3] = [1.0, 2.0, 4.0];
const MAX_RECOVERY_DELAY: f64 = 60.0;
const MAX_IDENTICAL_FAILURES: usize = 3;

#[derive(Default)]
pub struct SessionPolicy {
    provider_failures: usize,
    seen_operator_inputs: HashSet<String>,
    last_tool_failures: Vec<(String, String)>,
    repeated_tool_failures: usize,
}

impl SessionPolicy {
    pub fn observe_operator_inputs(&mut self, identities: Vec<String>) {
        if identities.is_empty() {
            return;
        }
        for identity in identities {
            if self.seen_operator_inputs.insert(identity) {
                self.provider_succeeded();
            }
        }
        self.last_tool_failures.clear();
        self.repeated_tool_failures = 0;
    }

    pub fn provider_succeeded(&mut self) {
        self.provider_failures = 0;
    }

    /// A provider failure can schedule a fresh request within the remaining slice.
    pub fn provider_recovery(
        &self,
        retry_after: Option<f64>,
        recoverable: bool,
        has_remaining_iteration: bool,
    ) -> Option<(f64, usize, usize)> {
        let retry_after = retry_after.unwrap_or(0.0);
        if !recoverable
            || retry_after.is_nan()
            || retry_after > MAX_RECOVERY_DELAY
            || !has_remaining_iteration
        {
            return None;
        }
        let delay = RECOVERY_DELAYS
            .get(self.provider_failures)?
            .max(retry_after);
        Some((delay, self.provider_failures + 1, RECOVERY_DELAYS.len()))
    }

    pub fn record_provider_recovery(&mut self) {
        self.provider_failures += 1;
    }

    pub fn observe_tool_failures(&mut self, signature: Vec<(String, String)>) -> bool {
        if signature.is_empty() {
            self.repeated_tool_failures = 0;
        } else if signature == self.last_tool_failures {
            self.repeated_tool_failures += 1;
        } else {
            self.repeated_tool_failures = 1;
        }
        self.last_tool_failures = signature;
        self.repeated_tool_failures >= MAX_IDENTICAL_FAILURES
    }
}

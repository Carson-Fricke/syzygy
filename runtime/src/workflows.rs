//! Drive an authored deterministic phase through its host ports.

pub use thesis_workflow_graph::{
    AcceptedCompletion, Destination, Graph, GraphError, Node, Outcome, PendingInputs, Position,
    Topology, accept_transition, replay, validate_topology,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseExecution {
    Cancelled,
    Accepted,
}

/// Invoke the phase once, confirm it can still publish, then accept its completion.
///
/// The host retains the attempt and owns durable acceptance. Retrying a revision
/// conflict belongs inside `accept`, after the callback has returned.
pub fn execute_deterministic<Completion, Error>(
    invoke: impl FnOnce() -> Result<Completion, Error>,
    cancelled_or_deleted: impl FnOnce() -> Result<bool, Error>,
    accept: impl FnOnce(Completion) -> Result<(), Error>,
) -> Result<PhaseExecution, Error> {
    let completion = invoke()?;
    if cancelled_or_deleted()? {
        return Ok(PhaseExecution::Cancelled);
    }
    accept(completion)?;
    Ok(PhaseExecution::Accepted)
}

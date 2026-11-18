//! Deterministic workflow topology validation and activation replay.
//!
//! The host owns accepted history, payloads and effects. The graph consumes
//! structural receipts and refers to payloads by their accepted sequence.

mod acceptance;
mod contracts;
mod replay;
mod topology;

pub use acceptance::accept_transition;
pub use contracts::{AcceptedCompletion, Destination, GraphError, Node, Outcome, Topology};
pub use replay::{Graph, PendingInputs, Position, replay};
pub use topology::validate_topology;

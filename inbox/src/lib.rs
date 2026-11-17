//! Canonical session ordering and pending input state.
//!
//! The host owns message content, synchronization with the persistence port,
//! and authority to acknowledge a successful invocation. This state receives
//! only references to already admitted arrivals.

use std::collections::{HashMap, HashSet};

mod admission;
mod turns;

pub use admission::{IncomingError, validate_incoming};
pub use turns::{TurnState, Wake};

#[derive(Default)]
pub struct Inbox {
    arrival_ids: Vec<String>,
    incoming_positions: HashMap<String, String>,
    pending_ids: HashSet<String>,
}

impl Inbox {
    /// Install the caller's validated initial ordering and pending identities.
    pub fn new(
        arrival_ids: Vec<String>,
        incoming_positions: HashMap<String, String>,
        pending_ids: HashSet<String>,
    ) -> Self {
        Self {
            arrival_ids,
            incoming_positions,
            pending_ids,
        }
    }

    /// Insert committed input among incoming peers without crossing a model output.
    pub fn admit(&mut self, identity: String, position: String) {
        let mut insertion = self.arrival_ids.len();
        if position != "0" {
            while let Some(previous) = insertion.checked_sub(1) {
                let Some(previous_position) =
                    self.incoming_positions.get(&self.arrival_ids[previous])
                else {
                    break;
                };
                if previous_position
                    .len()
                    .cmp(&position.len())
                    .then_with(|| previous_position.cmp(&position))
                    .is_le()
                {
                    break;
                }
                insertion = previous;
            }
        }
        self.incoming_positions.insert(identity.clone(), position);
        self.pending_ids.insert(identity.clone());
        self.arrival_ids.insert(insertion, identity);
    }

    pub fn record_output(&mut self, identity: String) {
        self.arrival_ids.push(identity);
    }

    /// Reorder the supplied identities while keeping omitted messages in their slots.
    pub fn reconcile(&mut self, ordered: &[String]) {
        let included: HashSet<_> = ordered.iter().map(String::as_str).collect();
        let selected = self
            .arrival_ids
            .iter_mut()
            .filter(|id| included.contains(id.as_str()));
        for (slot, identity) in selected.zip(ordered) {
            slot.clone_from(identity);
        }
    }

    pub fn arrivals(&self) -> &[String] {
        &self.arrival_ids
    }

    pub fn pending(&self) -> impl Iterator<Item = &String> {
        self.arrival_ids
            .iter()
            .filter(|id| self.pending_ids.contains(*id))
    }

    pub fn acknowledge(&mut self, identities: &[String]) {
        for identity in identities {
            self.pending_ids.remove(identity);
        }
    }

    pub fn retains_pending(&self, identities: &HashSet<String>) -> bool {
        self.pending_ids.is_subset(identities)
    }

    pub fn compact(&mut self, identities: Vec<String>) {
        self.arrival_ids = identities;
    }
}

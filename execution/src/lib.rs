//! Local dispatch ordering for durable work references.
//!
//! This queue may be rebuilt from durable notifications and snapshots. The
//! worker's ownership port remains authoritative for claims and state changes.

use std::collections::HashSet;

use indexmap::IndexMap;

type Identity = (String, String);
pub type Resource = (String, String);

/// An expired running attempt can retain interruption evidence but cannot run again.
pub fn attempt_action(state: &str) -> AttemptAction {
    match state {
        "pending" => AttemptAction::Run,
        "running" => AttemptAction::Interrupt,
        _ => AttemptAction::LeaveRetained,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptAction {
    Run,
    Interrupt,
    LeaveRetained,
}

pub struct Candidate<T> {
    pub workspace_id: String,
    pub key: String,
    pub resource: String,
    pub created_at: String,
    pub payload: T,
}

impl<T> Candidate<T> {
    fn resource(&self) -> Resource {
        (self.workspace_id.clone(), self.resource.clone())
    }
}

struct Queued<T> {
    candidate: Candidate<T>,
    retry_at: f64,
}

pub struct ReadyQueue<T> {
    pending: IndexMap<Identity, Queued<T>>,
}

impl<T> Default for ReadyQueue<T> {
    fn default() -> Self {
        Self {
            pending: IndexMap::new(),
        }
    }
}

impl<T> ReadyQueue<T> {
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Update a notification without resetting an existing contention delay.
    pub fn observe(&mut self, candidate: Candidate<T>) {
        let identity = (candidate.workspace_id.clone(), candidate.key.clone());
        if let Some(queued) = self.pending.get_mut(&identity) {
            queued.candidate = candidate;
        } else {
            self.pending.insert(
                identity,
                Queued {
                    candidate,
                    retry_at: 0.0,
                },
            );
        }
    }

    pub fn retry(&mut self, candidate: Candidate<T>, retry_at: f64) {
        let identity = (candidate.workspace_id.clone(), candidate.key.clone());
        self.pending.insert(
            identity,
            Queued {
                candidate,
                retry_at,
            },
        );
    }

    /// Reserve available local slots in creation order, one per resource.
    pub fn take_ready(
        &mut self,
        now: f64,
        mut active: HashSet<Resource>,
        capacity: usize,
    ) -> Vec<(Resource, T)> {
        if active.len() >= capacity {
            return Vec::new();
        }
        let mut eligible: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, item)| now >= item.retry_at)
            .collect();
        eligible.sort_by_key(|(_, item)| (&item.candidate.created_at, &item.candidate.key));
        let mut ready = Vec::new();
        for (identity, item) in eligible {
            let resource = item.candidate.resource();
            if active.insert(resource.clone()) {
                ready.push((identity.clone(), resource));
                if active.len() == capacity {
                    break;
                }
            }
        }
        ready
            .into_iter()
            .filter_map(|(identity, resource)| {
                self.pending
                    .shift_remove(&identity)
                    .map(|item| (resource, item.candidate.payload))
            })
            .collect()
    }
}

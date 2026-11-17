//! Coalesce local wakes until a model snapshot consumes the observed input.
//! A durable acknowledgement is separate from the in-flight cursor.

use std::collections::VecDeque;

const RECENT_INPUTS: usize = 256;

#[derive(Default)]
pub struct TurnState {
    busy: bool,
    generation: i64,
    consumed_generation: i64,
    recent: VecDeque<String>,
    consumed_position: i64,
    durable_position: i64,
    requested_position: i64,
    requested_generation: i64,
}

pub enum Wake {
    Ignored,
    Coalesced,
    Start,
}

impl TurnState {
    pub fn load(&mut self, position: i64) {
        self.consumed_position = position;
        self.durable_position = position;
    }

    pub fn schedule(&mut self, identity: Option<&str>, position: i64) -> Wake {
        if identity.is_some_and(|id| self.recent.iter().any(|recent| recent == id))
            || (position > 0 && position <= self.consumed_position)
        {
            return Wake::Ignored;
        }
        self.generation += 1;
        if position > 0 {
            self.requested_position = self.requested_position.max(position);
            self.requested_generation = self.generation;
        }
        if self.busy {
            return Wake::Coalesced;
        }
        self.busy = true;
        self.consumed_generation = self.generation;
        Wake::Start
    }

    pub fn observe(&mut self, identities: impl IntoIterator<Item = String>) {
        for identity in identities {
            if !self.recent.contains(&identity) {
                if self.recent.len() == RECENT_INPUTS {
                    self.recent.pop_front();
                }
                self.recent.push_back(identity);
            }
        }
    }

    pub fn consume(&mut self, generation: i64, position: i64) -> i64 {
        self.consumed_generation = self
            .consumed_generation
            .max(generation.min(self.generation));
        self.consumed_position = self.consumed_position.max(position);
        if self.requested_position <= self.consumed_position {
            self.consumed_generation = self.consumed_generation.max(self.requested_generation);
        }
        self.consumed_position
    }

    pub fn acknowledge(&mut self, position: i64) {
        self.durable_position = self.durable_position.max(position);
    }

    pub fn can_retire(&self) -> bool {
        !self.busy && self.requested_position <= self.durable_position
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn stop(&mut self) {
        self.busy = false;
    }

    pub fn next_turn(&mut self) -> bool {
        if self.generation <= self.consumed_generation {
            self.stop();
            false
        } else {
            self.consumed_generation = self.generation;
            true
        }
    }
}

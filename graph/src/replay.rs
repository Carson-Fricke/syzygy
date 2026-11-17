//! Replay accepted receipts into an ordered frontier and scoped handoffs.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{AcceptedCompletion, Destination, GraphError, Node, Outcome, Topology};

type Branches = BTreeSet<String>;
type BranchCoverage = IndexMap<String, Branches>;

#[derive(Clone, Debug)]
struct Activation {
    position: Position,
    coverage: BranchCoverage,
}

#[derive(Clone, Debug, Serialize)]
pub struct Position {
    pub attempt_id: String,
    /// Input ports in declaration order. Sequence zero refers to launch input.
    pub inputs: IndexMap<String, u64>,
    pub history: Vec<u64>,
    pub source: Option<String>,
    pub dependencies: Vec<u64>,
}

#[derive(Debug)]
struct FanOut {
    parent: BranchCoverage,
    branches: Branches,
    finished: Branches,
}

#[derive(Debug)]
struct Arrival {
    source: Activation,
    sequence: u64,
}

#[derive(Debug, Serialize)]
pub struct PendingInputs {
    pub required: Vec<String>,
    pub arrived: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Graph {
    pub positions: IndexMap<String, Position>,
    pub has_open_fanout: bool,
    pub terminal_ready: bool,
    pub pending: IndexMap<String, PendingInputs>,
    pub sequence: u64,
}

/// Compute the current frontier from accepted, contiguous history.
pub fn replay(
    execution_id: &str,
    entry: &str,
    nodes: &Topology,
    history: &[AcceptedCompletion],
) -> Result<Graph, GraphError> {
    let Some([port]) = nodes.get(entry).map(|node| node.inputs.as_slice()) else {
        return Err(GraphError::new(
            "the entry node requires one workflow input port",
        ));
    };
    let mut replay = Replay {
        execution_id,
        nodes,
        positions: IndexMap::from([(
            entry.to_owned(),
            Activation {
                position: Position {
                    attempt_id: attempt_identity(execution_id, entry, 0),
                    inputs: IndexMap::from([(port.clone(), 0)]),
                    history: Vec::new(),
                    source: None,
                    dependencies: Vec::new(),
                },
                coverage: BranchCoverage::new(),
            },
        )]),
        fanouts: IndexMap::new(),
        pending: IndexMap::new(),
        deliveries: HashMap::new(),
        sequence: 0,
    };
    for accepted in history {
        replay.apply(accepted)?;
    }
    Ok(replay.finish())
}

fn attempt_identity(execution_id: &str, phase: &str, sequence: u64) -> String {
    let hash = Sha256::digest(format!("{execution_id}|{phase}|{sequence}"));
    format!("attempt-{}", &format!("{hash:x}")[..32])
}

struct Replay<'a> {
    execution_id: &'a str,
    nodes: &'a Topology,
    positions: IndexMap<String, Activation>,
    fanouts: IndexMap<String, FanOut>,
    pending: IndexMap<String, IndexMap<String, Arrival>>,
    deliveries: HashMap<Destination, Vec<BranchCoverage>>,
    sequence: u64,
}

impl Replay<'_> {
    fn apply(&mut self, accepted: &AcceptedCompletion) -> Result<(), GraphError> {
        if self.sequence.checked_add(1) != Some(accepted.sequence) {
            return Err(GraphError::new("accepted history is not contiguous"));
        }
        let mut source = self
            .positions
            .shift_remove(&accepted.from_phase)
            .filter(|source| source.position.attempt_id == accepted.attempt_id)
            .ok_or_else(|| {
                GraphError::new("completion has a stale or missing source activation")
            })?;
        let destinations = self.nodes[&accepted.from_phase]
            .routes
            .get(&accepted.route)
            .filter(|targets| !targets.is_empty())
            .ok_or_else(|| GraphError::new("completion selects an undeclared route"))?;
        self.sequence = accepted.sequence;
        source.position.history.push(self.sequence);

        if let [target] = destinations.as_slice() {
            self.deliver(target, source, &accepted.from_phase)?;
            return Ok(());
        }

        let key = format!("{}:{}", source.position.attempt_id, self.sequence);
        self.fanouts.insert(
            key.clone(),
            FanOut {
                parent: source.coverage.clone(),
                branches: destinations.iter().map(ToString::to_string).collect(),
                finished: Branches::new(),
            },
        );
        for target in destinations {
            let mut branch = source.clone();
            branch
                .coverage
                .insert(key.clone(), Branches::from([target.to_string()]));
            if self.deliver(target, branch, &accepted.from_phase)? == Some(Outcome::Failed) {
                break;
            }
        }
        Ok(())
    }

    fn deliver(
        &mut self,
        target: &Destination,
        source: Activation,
        source_phase: &str,
    ) -> Result<Option<Outcome>, GraphError> {
        let node = self
            .nodes
            .get(&target.node)
            .filter(|node| node.inputs.contains(&target.port))
            .ok_or_else(|| GraphError::new(format!("undeclared input {target}")))?;
        if self.positions.contains_key(&target.node) {
            return Err(GraphError::new(format!(
                "two live activations at node {:?}",
                target.node
            )));
        }
        let deliveries = self.deliveries.entry(target.clone()).or_default();
        let conflicting = deliveries.iter().any(|earlier| {
            source.coverage.iter().any(|(fanout, branches)| {
                earlier
                    .get(fanout)
                    .is_some_and(|prior| prior.is_disjoint(branches))
            })
        });
        if conflicting {
            return Err(GraphError::new(format!("conflicting delivery to {target}")));
        }
        deliveries.push(source.coverage.clone());

        let attempt_id = attempt_identity(self.execution_id, &target.node, self.sequence);
        let mut next = if let [port] = node.inputs.as_slice() {
            Activation {
                position: Position {
                    attempt_id,
                    inputs: IndexMap::from([(port.clone(), self.sequence)]),
                    ..source.position
                },
                coverage: source.coverage,
            }
        } else {
            let arrivals = self.pending.entry(target.node.clone()).or_default();
            if arrivals.contains_key(&target.port) {
                return Err(GraphError::new(format!("conflicting delivery to {target}")));
            }
            arrivals.insert(
                target.port.clone(),
                Arrival {
                    source,
                    sequence: self.sequence,
                },
            );
            if arrivals.len() != node.inputs.len() {
                return Ok(node.outcome);
            }
            let arrivals = self
                .pending
                .shift_remove(&target.node)
                .ok_or_else(|| GraphError::new("required inputs disappeared before activation"))?;
            self.join(node, &arrivals, attempt_id)?
        };
        next.position.source = Some(source_phase.to_owned());

        match node.outcome {
            Some(Outcome::Failed) => {
                self.positions.clear();
                self.fanouts.clear();
                self.pending.clear();
            }
            Some(Outcome::Completed) => {
                for (key, branches) in &next.coverage {
                    let fanout = self.fanouts.get_mut(key).ok_or_else(|| {
                        GraphError::new("terminal completion references a closed fan-out")
                    })?;
                    fanout.finished.extend(branches.iter().cloned());
                }
                self.close_finished_fanouts();
            }
            None => {}
        }
        self.positions.insert(target.node.clone(), next);
        Ok(node.outcome)
    }

    fn join(
        &mut self,
        node: &Node,
        arrivals: &IndexMap<String, Arrival>,
        attempt_id: String,
    ) -> Result<Activation, GraphError> {
        let sources: Vec<_> = node
            .inputs
            .iter()
            .map(|port| &arrivals[port].source)
            .collect();
        let coverage = self.join_coverage(&sources)?;
        let history = self.common_history(&sources);
        let dependencies = sources
            .iter()
            .flat_map(|source| {
                source
                    .position
                    .history
                    .iter()
                    .chain(&source.position.dependencies)
            })
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(Activation {
            position: Position {
                attempt_id,
                inputs: node
                    .inputs
                    .iter()
                    .map(|port| (port.clone(), arrivals[port].sequence))
                    .collect(),
                source: None,
                history,
                dependencies,
            },
            coverage,
        })
    }

    fn common_history(&self, sources: &[&Activation]) -> Vec<u64> {
        let [first, rest @ ..] = sources else {
            return Vec::new();
        };
        let histories: Vec<HashSet<_>> = rest
            .iter()
            .map(|source| source.position.history.iter().copied().collect())
            .collect();
        first
            .position
            .history
            .iter()
            .copied()
            .filter(|sequence| histories.iter().all(|history| history.contains(sequence)))
            .collect()
    }

    fn join_coverage(&mut self, sources: &[&Activation]) -> Result<BranchCoverage, GraphError> {
        if !sources[0].coverage.keys().any(|key| {
            sources
                .iter()
                .all(|source| source.coverage.contains_key(key))
        }) {
            return Err(GraphError::new("inputs belong to different activations"));
        }

        // Separate visits to a nested fan-out cannot exchange their inputs.
        for (index, left) in sources.iter().enumerate() {
            for right in &sources[index + 1..] {
                for left_key in left
                    .coverage
                    .keys()
                    .filter(|key| !right.coverage.contains_key(*key))
                {
                    for right_key in right
                        .coverage
                        .keys()
                        .filter(|key| !left.coverage.contains_key(*key))
                    {
                        let parent = |key: &str| {
                            self.fanouts
                                .get(key)
                                .map(|fanout| &fanout.parent)
                                .ok_or_else(|| {
                                    GraphError::new("input delivery references a closed fan-out")
                                })
                        };
                        if parent(left_key)? == parent(right_key)? {
                            return Err(GraphError::new(
                                "inputs belong to different fan-out occurrences",
                            ));
                        }
                    }
                }
            }
        }

        let mut coverage = BranchCoverage::new();
        for source in sources {
            for (key, branches) in &source.coverage {
                coverage
                    .entry(key.clone())
                    .or_default()
                    .extend(branches.iter().cloned());
            }
        }
        // A fully reunited occurrence stops contributing coverage once its
        // nested occurrences have closed.
        for key in self.fanout_keys_in_reverse() {
            if coverage.get(&key) == Some(&self.fanouts[&key].branches) && !self.has_children(&key)
            {
                self.fanouts.shift_remove(&key);
                coverage.shift_remove(&key);
            }
        }
        Ok(self
            .fanouts
            .keys()
            .filter_map(|key| coverage.shift_remove_entry(key))
            .collect())
    }

    fn has_children(&self, key: &str) -> bool {
        self.fanouts
            .values()
            .any(|fanout| fanout.parent.contains_key(key))
    }

    fn fanout_keys_in_reverse(&self) -> Vec<String> {
        self.fanouts.keys().rev().cloned().collect()
    }

    fn close_finished_fanouts(&mut self) {
        for key in self.fanout_keys_in_reverse() {
            let fanout = &self.fanouts[&key];
            if fanout.finished == fanout.branches && !self.has_children(&key) {
                self.fanouts.shift_remove(&key);
            }
        }
    }

    fn finish(self) -> Graph {
        let terminal_ready = self.fanouts.is_empty()
            && self.pending.is_empty()
            && self
                .positions
                .keys()
                .all(|phase| self.nodes[phase].outcome.is_some());
        Graph {
            positions: self
                .positions
                .into_iter()
                .map(|(name, activation)| (name, activation.position))
                .collect(),
            has_open_fanout: !self.fanouts.is_empty(),
            terminal_ready,
            pending: self
                .pending
                .into_iter()
                .map(|(name, arrivals)| {
                    let required = self.nodes[&name].inputs.clone();
                    let arrived = arrivals.into_keys().collect();
                    (name, PendingInputs { required, arrived })
                })
                .collect(),
            sequence: self.sequence,
        }
    }
}

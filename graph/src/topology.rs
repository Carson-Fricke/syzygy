//! Validate declared destinations, required inputs and entry reachability.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::{GraphError, Topology};

pub fn validate_topology(nodes: &Topology, entry: &str) -> Result<(), GraphError> {
    let entry_node = nodes
        .get(entry)
        .ok_or_else(|| GraphError::new("entry must name a declared node"))?;
    if entry_node.inputs.len() != 1 {
        return Err(GraphError::new("entry requires one workflow input port"));
    }

    let mut supplied: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (name, node) in nodes {
        if name != &node.name {
            return Err(GraphError::new("node keys must match their names"));
        }
        for target in node.routes.values().flatten() {
            if !nodes
                .get(&target.node)
                .is_some_and(|destination| destination.inputs.contains(&target.port))
            {
                return Err(GraphError::new(format!("undeclared destination {target}")));
            }
            supplied
                .entry(&target.node)
                .or_default()
                .insert(&target.port);
        }
    }
    supplied
        .entry(entry)
        .or_default()
        .insert(&entry_node.inputs[0]);

    let reachable = reachable_nodes(nodes, entry);
    let mut unreachable = BTreeSet::new();
    for (name, node) in nodes {
        let is_reachable = reachable.contains(name.as_str());
        let missing: BTreeSet<_> = node
            .inputs
            .iter()
            .filter(|port| {
                !supplied
                    .get(name.as_str())
                    .is_some_and(|ports| ports.contains(port.as_str()))
            })
            .collect();
        if !missing.is_empty() && (node.outcome.is_none() || is_reachable) {
            return Err(GraphError::new(format!(
                "node {name:?} has unconnected inputs: {:?}",
                missing.into_iter().collect::<Vec<_>>()
            )));
        }
        if !is_reachable && node.outcome.is_none() {
            unreachable.insert(name);
        }
    }
    if !unreachable.is_empty() {
        return Err(GraphError::new(format!(
            "unreachable graph nodes: {:?}",
            unreachable.into_iter().collect::<Vec<_>>()
        )));
    }
    Ok(())
}

fn reachable_nodes<'a>(nodes: &'a Topology, entry: &'a str) -> HashSet<&'a str> {
    let mut visited = HashSet::new();
    let mut pending = VecDeque::from([entry]);
    while let Some(name) = pending.pop_front() {
        if visited.insert(name) {
            pending.extend(
                nodes[name]
                    .routes
                    .values()
                    .flatten()
                    .map(|port| port.node.as_str()),
            );
        }
    }
    visited
}

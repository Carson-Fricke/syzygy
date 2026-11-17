//! Traverse the durable subagent parent links supplied by the host.
//!
//! The caller resolves scope and obtains durable records. Traversal only
//! returns identities in root-first order, preserving sibling order.

use std::collections::{HashMap, HashSet};

mod authority;

pub use authority::{AgentScope, PhaseDelivery, accepts_unsolicited_input, phase_delivery};

pub fn subtree_order<'a>(
    records: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    roots: impl DoubleEndedIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut known = HashSet::new();
    for (identity, parent) in records {
        known.insert(identity);
        if let Some(parent) = parent {
            children.entry(parent).or_default().push(identity);
        }
    }
    let mut pending: Vec<_> = roots.rev().filter(|root| known.contains(root)).collect();
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    while let Some(identity) = pending.pop() {
        if visited.insert(identity) {
            ordered.push(identity);
            if let Some(children) = children.get(identity) {
                pending.extend(children.iter().rev().copied());
            }
        }
    }
    ordered
}

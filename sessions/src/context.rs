//! Context window planning over neutral message references and token estimates.

use std::collections::{BTreeSet, HashMap};

pub type ToolLink = (String, String, String);
pub type ContextEntry = (Vec<ToolLink>, Option<ToolLink>);

/// Each group includes its correlated results and any interleaved records.
pub fn group_ranges(entries: &[ContextEntry]) -> Vec<(usize, usize)> {
    let last_receipt: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (_, receipt))| Some((receipt.as_ref()?, index)))
        .collect();
    let end: Vec<_> = entries
        .iter()
        .enumerate()
        .map(|(index, (calls, _))| {
            calls
                .iter()
                .filter_map(|call| last_receipt.get(call).copied())
                .fold(index, usize::max)
        })
        .collect();

    let mut groups = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let mut stop = end[start] + 1;
        let mut index = start + 1;
        while index < stop {
            stop = stop.max(end[index] + 1);
            index += 1;
        }
        groups.push((start, stop));
        start = stop;
    }
    groups
}

/// Preserve every unconsumed record and, when possible, the requested tail.
pub fn retiring_boundary(token_counts: &[usize], pending_boundary: usize, keep: usize) -> usize {
    let mut boundary = pending_boundary.min(token_counts.len());
    let keep = keep.min(token_counts.iter().sum::<usize>() / 4);
    let mut retained: usize = token_counts[boundary..].iter().sum();
    while boundary > 0 && retained < keep {
        boundary -= 1;
        retained += token_counts[boundary];
    }
    boundary
}

pub fn select_ranked_groups(
    costs: &[usize],
    ranked: &[usize],
    mut budget: usize,
) -> Option<Vec<usize>> {
    let mut selected = BTreeSet::new();
    for &index in ranked {
        let cost = *costs.get(index)?;
        if cost <= budget && selected.insert(index) {
            budget -= cost;
        }
    }
    Some(selected.into_iter().collect())
}

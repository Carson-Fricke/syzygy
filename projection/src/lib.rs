//! Plan the provider-visible order from an already scoped neutral transcript.
//!
//! Only exact call correlations can create groups. The plan references the
//! original messages, leaving opaque data and wire rendering at the boundary.

use std::collections::{HashMap, HashSet};
use std::fmt;

use syzygy_runtime_contracts::ToolCorrelation;

mod completion;
mod reasoning;
mod sse;

pub use completion::{CompletionStream, StreamCompletion, StreamError};
pub use reasoning::{REASONING_DETAILS, SCALAR_REASONING_FIELDS, readable_block_field};
pub use sse::{FrameError, SseDecoder};

#[derive(Debug)]
pub struct Message {
    pub message_id: String,
    pub requests: Vec<Request>,
    pub correlation: Option<ToolCorrelation>,
    pub continuation: Continuation,
}

#[derive(Debug)]
pub struct Request {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub enum Continuation {
    Local,
    Foreign,
    Invalid(String),
}

#[derive(Debug)]
pub enum Item {
    Ordinary {
        index: usize,
    },
    Incomplete {
        index: usize,
        reason: IncompleteReason,
    },
    Group {
        index: usize,
        results: Vec<Vec<usize>>,
    },
}

#[derive(Debug)]
pub enum IncompleteReason {
    ReusedIds,
    MissingResults,
    ForeignOrigin,
}

#[derive(Debug)]
pub struct ProjectionError(String);

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProjectionError {}

type CallKey<'a> = (&'a str, &'a str, &'a str);

impl Message {
    fn call_key<'a>(&'a self, request: &'a Request) -> CallKey<'a> {
        (&self.message_id, &request.id, &request.name)
    }
}

/// Produce one item per ordinary message or complete native call group.
pub fn project(messages: &[Message]) -> Result<Vec<Item>, ProjectionError> {
    let mut identities = HashSet::new();
    let mut request_counts: HashMap<&str, usize> = HashMap::new();
    let mut requests = HashMap::new();
    let mut results: HashMap<CallKey<'_>, Vec<usize>> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if !identities.insert(&message.message_id) {
            return Err(ProjectionError(
                "session message IDs must be unique within projection scope".into(),
            ));
        }
        for request in &message.requests {
            *request_counts.entry(&request.id).or_default() += 1;
            requests.insert(message.call_key(request), index);
        }
        if let Some(correlation) = &message.correlation {
            let key = correlation.key();
            if requests
                .get(&key)
                .is_some_and(|request_index| *request_index < index)
            {
                results.entry(key).or_default().push(index);
            }
        }
    }

    let mut consumed = HashSet::new();
    let mut items = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        let item = plan_message(index, message, &request_counts, &results)?;
        if let Item::Group { results, .. } = &item {
            consumed.extend(results.iter().flatten().copied());
        }
        items.push(item);
    }

    let mut projected = Vec::new();
    let mut deferred = Vec::new();
    let mut defer_until = 0;
    for (index, item) in items.into_iter().enumerate() {
        if !consumed.contains(&index) {
            if let Item::Group { results, .. } = &item {
                if let Some(last) = results.iter().filter_map(|group| group.last()).max() {
                    defer_until = defer_until.max(*last);
                }
                if deferred.is_empty() {
                    projected.push(item);
                } else {
                    deferred.push(item);
                }
            } else {
                deferred.push(item);
            }
        }
        if index >= defer_until {
            projected.append(&mut deferred);
        }
    }
    projected.append(&mut deferred);
    Ok(projected)
}

fn plan_message(
    index: usize,
    message: &Message,
    counts: &HashMap<&str, usize>,
    results: &HashMap<CallKey<'_>, Vec<usize>>,
) -> Result<Item, ProjectionError> {
    if message.requests.is_empty() {
        return Ok(Item::Ordinary { index });
    }
    let incomplete = |reason| Ok(Item::Incomplete { index, reason });
    if message
        .requests
        .iter()
        .any(|request| counts[request.id.as_str()] != 1)
    {
        return incomplete(IncompleteReason::ReusedIds);
    }
    let matching: Option<Vec<_>> = message
        .requests
        .iter()
        .map(|request| results.get(&message.call_key(request)).cloned())
        .collect();
    let Some(results) = matching else {
        return incomplete(IncompleteReason::MissingResults);
    };
    match &message.continuation {
        Continuation::Local => Ok(Item::Group { index, results }),
        Continuation::Foreign => incomplete(IncompleteReason::ForeignOrigin),
        Continuation::Invalid(message) => Err(ProjectionError(message.clone())),
    }
}

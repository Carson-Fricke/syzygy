//! Durable, provider-independent conversation turns. Hosts supply a store, a model
//! provider, and an authorized tool executor. The runtime owns admission and replay.

mod catalog;
#[cfg(feature = "sqlite")]
mod sqlite;
mod timeline;
mod tools;
pub use tools::{FunctionTool, Tool, ToolNamespace, valid_tool_name, validate_tools};

use std::{error::Error as StdError, fmt};

pub use catalog::{AgentCatalog, ModelProfile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;
pub use syzygy_runtime_contracts::OutputDelta;
pub use timeline::{Status, Timeline};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub namespace: Option<String>,
    pub name: String,
    pub input: ToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolInput {
    Function { arguments: String },
    Custom { input: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Text(String),
    Content(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: ToolOutput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelReply {
    pub id: String,
    pub text: String,
    pub calls: Vec<ToolCall>,
    /// Native replay evidence under the adapter's serialization contract.
    pub replay: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelItem {
    Developer(String),
    User(String),
    Assistant {
        reply: ModelReply,
        results: Vec<ToolResult>,
    },
}

/// Check both the provider's current batch and identities present in earlier
/// history before exposing executable calls. A store must still fence the
/// snapshot at commitment to exclude concurrently accepted answers.
pub fn validate_calls<'a>(
    calls: &[ToolCall],
    previous: impl IntoIterator<Item = &'a str>,
) -> Result<(), Error> {
    validate_call_names_and_ids(
        calls
            .iter()
            .map(|call| (call.name.as_str(), call.call_id.as_str())),
        previous,
    )
}

/// Validate model-generated (tool_name, call_id) pairs against all previously
/// accepted call IDs in the host's authoritative conversation snapshot.
pub fn validate_call_names_and_ids<'a, 'b>(
    calls: impl IntoIterator<Item = (&'a str, &'a str)>,
    previous: impl IntoIterator<Item = &'b str>,
) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for (name, id) in calls {
        if name.trim().is_empty() || id.trim().is_empty() {
            return Err(Error::Invalid(
                "tool call identity and name must be nonempty".into(),
            ));
        }
        if !seen.insert(id) {
            return Err(Error::Invalid(
                "provider reused a tool call identity".into(),
            ));
        }
    }
    if !seen.is_empty() && previous.into_iter().any(|id| seen.contains(id)) {
        return Err(Error::Invalid(
            "provider reused a tool call identity".into(),
        ));
    }
    Ok(())
}

pub trait Provider {
    type Error: StdError + Send + Sync + 'static;
    /// The history is complete through the last settled tool group. Deltas are
    /// provisional; only the returned reply can become a durable model event.
    fn respond(
        &mut self,
        history: &[ModelItem],
        emit: &mut dyn FnMut(OutputDelta),
    ) -> Result<ModelReply, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Bound {
        binding: Value,
    },
    /// Durably accepting this item makes the turn runnable. Store implementations
    /// with a separate worker queue must enqueue that work in the same transaction.
    User {
        text: String,
    },
    Assistant {
        reply: ModelReply,
    },
    ToolStarted {
        call_id: String,
    },
    ToolFinished {
        result: ToolResult,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub sequence: u64,
    pub turn_id: String,
    pub event: Event,
}

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Conflict,
    Store(Box<dyn StdError + Send + Sync>),
    Provider(Box<dyn StdError + Send + Sync>),
    StepLimit,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Conflict => f.write_str("conversation changed during this operation"),
            Self::Store(error) => write!(f, "conversation storage failed: {error}"),
            Self::Provider(error) => write!(f, "model request failed: {error}"),
            Self::StepLimit => f.write_str("model step limit reached; resume to continue the turn"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Provider(error) => Some(error.as_ref()),
            Self::Store(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// A conversation-scoped, contiguous, append-only log. Zero denotes an empty log.
/// A successful compare_and_append must mean this caller newly committed one item.
/// Return Conflict on a stale cursor, including an identical previously committed item.
/// Return a storage error when commitment is uncertain. In particular, a retry
/// can never acquire permission to execute a previously claimed tool.
pub trait Store {
    fn compare_and_append(
        &mut self,
        chat: &str,
        expected: u64,
        turn_id: &str,
        event: &Event,
    ) -> Result<u64, Error>;
    /// Return records after the exclusive cursor in ascending sequence order.
    fn read_after(&self, chat: &str, after: u64, limit: usize) -> Result<Vec<Record>, Error>;
    fn chats(&self) -> Result<Vec<String>, Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Complete,
    /// A durable claim has no outcome. Another worker may be executing it, or
    /// its process may have stopped. Only an externally verified receipt or an
    /// explicit operator decision may settle it.
    Uncertain {
        call_id: String,
    },
}

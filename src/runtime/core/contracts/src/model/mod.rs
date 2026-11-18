//! Provider-neutral model input, output, streaming, and tool contracts.

mod tools;

use std::{error::Error as StdError, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::OutputDelta;
pub use tools::{FunctionTool, Tool, ToolNamespace, valid_tool_name, validate_tools};

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

/// A provider call needs a name and a unique identity in the conversation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallIdentityError {
    MissingIdentity,
    ReusedIdentity,
}

impl fmt::Display for ToolCallIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingIdentity => "tool call identity and name must be nonempty",
            Self::ReusedIdentity => "provider reused a tool call identity",
        })
    }
}

impl StdError for ToolCallIdentityError {}

/// Check both the provider's current batch and identities present in earlier
/// history before exposing executable calls. A store must still fence the
/// snapshot at commitment to exclude concurrently accepted answers.
pub fn validate_calls<'a>(
    calls: &[ToolCall],
    previous: impl IntoIterator<Item = &'a str>,
) -> Result<(), ToolCallIdentityError> {
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
) -> Result<(), ToolCallIdentityError> {
    let mut seen = std::collections::HashSet::new();
    for (name, id) in calls {
        if name.trim().is_empty() || id.trim().is_empty() {
            return Err(ToolCallIdentityError::MissingIdentity);
        }
        if !seen.insert(id) {
            return Err(ToolCallIdentityError::ReusedIdentity);
        }
    }
    if !seen.is_empty() && previous.into_iter().any(|id| seen.contains(id)) {
        return Err(ToolCallIdentityError::ReusedIdentity);
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

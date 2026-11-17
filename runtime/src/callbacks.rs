//! Versioned workflow callbacks over in-process and subprocess transports.

mod journal;
mod process;

use std::{error::Error, fmt, io};

use serde::Serialize;
use serde_json::{Map, Value};

pub use journal::{
    Admission, FileInvocationJournal, InvocationJournal, JournaledCallback, RecordedReply,
};
pub use process::{CallbackCommand, ProcessCallback};

pub const PROTOCOL_VERSION: u32 = 2;
pub type JsonObject = Map<String, Value>;

/// `invocation_id` is supplied by the host. Hosts must retain the identity of
/// effectful callbacks before dispatch and reconcile uncertain outcomes by it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackRequest {
    invocation_id: String,
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase_attempt_id: Option<String>,
    payload: JsonObject,
}

impl CallbackRequest {
    pub fn new(
        invocation_id: impl Into<String>,
        operation: impl Into<String>,
        phase_attempt_id: Option<String>,
        mut payload: JsonObject,
    ) -> Result<Self, CallbackProtocolError> {
        let invocation_id = invocation_id.into();
        let operation = operation.into();
        for (name, value) in [("invocationId", &invocation_id), ("operation", &operation)] {
            if value.trim().is_empty() {
                return Err(CallbackProtocolError::EmptyField(name));
            }
        }
        if phase_attempt_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(CallbackProtocolError::EmptyField("phaseAttemptId"));
        }
        payload.insert("protocolVersion".into(), PROTOCOL_VERSION.into());
        Ok(Self {
            invocation_id,
            operation,
            phase_attempt_id,
            payload,
        })
    }

    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn payload(&self) -> &JsonObject {
        &self.payload
    }

    pub fn phase_attempt_id(&self) -> Option<&str> {
        self.phase_attempt_id.as_deref()
    }
}

pub trait WorkflowCallback {
    type Error;

    fn invoke(&self, request: &CallbackRequest) -> Result<JsonObject, Self::Error>;
}

impl<F, E> WorkflowCallback for F
where
    F: Fn(&CallbackRequest) -> Result<JsonObject, E>,
{
    type Error = E;

    fn invoke(&self, request: &CallbackRequest) -> Result<JsonObject, Self::Error> {
        self(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallbackProtocolError {
    EmptyField(&'static str),
    MismatchedInvocation,
    InvalidOutcome,
    InvocationConflict,
}

impl fmt::Display for CallbackProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(f, "workflow callback {field} must be nonempty"),
            Self::MismatchedInvocation => {
                f.write_str("workflow callback returned the wrong invocation ID")
            }
            Self::InvalidOutcome => {
                f.write_str("workflow callback must return exactly one result or error")
            }
            Self::InvocationConflict => {
                f.write_str("workflow callback identity is already retained with different inputs")
            }
        }
    }
}

impl Error for CallbackProtocolError {}

/// Outcomes for a single transport invocation. The transport never retries.
#[derive(Debug)]
pub enum CallbackError {
    NotStarted(io::Error),
    Unknown(UnknownOutcome),
    Rejected { message: String },
}

#[derive(Debug)]
pub enum UnknownOutcome {
    RetainedInvocation,
    Io(io::Error),
    TimedOut,
    OutputLimit,
    Exit { code: Option<i32> },
    InvalidReply(serde_json::Error),
    Protocol(CallbackProtocolError),
}

impl fmt::Display for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted(error) => write!(f, "workflow callback could not start: {error}"),
            Self::Unknown(error) => write!(f, "workflow callback outcome is unknown: {error}"),
            Self::Rejected { message } => {
                write!(f, "workflow callback rejected the request: {message}")
            }
        }
    }
}

impl fmt::Display for UnknownOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RetainedInvocation => {
                f.write_str("the retained invocation has no terminal reply")
            }
            Self::Io(error) => write!(f, "{error}"),
            Self::TimedOut => f.write_str("the invocation exceeded its deadline"),
            Self::OutputLimit => f.write_str("the reply exceeded its byte limit"),
            Self::Exit { code: Some(code) } => write!(f, "the process exited with status {code}"),
            Self::Exit { code: None } => f.write_str("the process terminated"),
            Self::InvalidReply(error) => write!(f, "invalid JSON reply: {error}"),
            Self::Protocol(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CallbackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotStarted(error) | Self::Unknown(UnknownOutcome::Io(error)) => Some(error),
            Self::Unknown(UnknownOutcome::InvalidReply(error)) => Some(error),
            Self::Unknown(UnknownOutcome::Protocol(error)) => Some(error),
            _ => None,
        }
    }
}

//! Retain effectful invocation identities before dispatch and join durable replies.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use syzygy_runtime_storage::{create_projection, document_identity, replace_projection};

use super::{
    CallbackError, CallbackProtocolError, CallbackRequest, JsonObject, UnknownOutcome,
    WorkflowCallback,
};

#[derive(Debug)]
pub enum Admission {
    New,
    Pending,
    Conflict,
    Finished(RecordedReply),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordedReply {
    Completed { result: JsonObject },
    Rejected { message: String },
    NotStarted { message: String },
}

impl RecordedReply {
    fn into_result(self) -> Result<JsonObject, CallbackError> {
        match self {
            Self::Completed { result } => Ok(result),
            Self::Rejected { message } => Err(CallbackError::Rejected { message }),
            Self::NotStarted { message } => {
                Err(CallbackError::NotStarted(io::Error::other(message)))
            }
        }
    }
}

pub trait InvocationJournal {
    /// Atomically reserve the identity before the callback can cause effects.
    fn admit(&self, request: &CallbackRequest) -> io::Result<Admission>;

    /// Only the caller admitted as `New` may store the terminal reply.
    fn finish(&self, request: &CallbackRequest, reply: &RecordedReply) -> io::Result<()>;
}

impl<J: InvocationJournal> InvocationJournal for &J {
    fn admit(&self, request: &CallbackRequest) -> io::Result<Admission> {
        (*self).admit(request)
    }

    fn finish(&self, request: &CallbackRequest, reply: &RecordedReply) -> io::Result<()> {
        (*self).finish(request, reply)
    }
}

pub struct JournaledCallback<C, J> {
    callback: C,
    journal: J,
}

impl<C, J> JournaledCallback<C, J> {
    pub fn new(callback: C, journal: J) -> Self {
        Self { callback, journal }
    }
}

impl<C, J> WorkflowCallback for JournaledCallback<C, J>
where
    C: WorkflowCallback<Error = CallbackError>,
    J: InvocationJournal,
{
    type Error = CallbackError;

    fn invoke(&self, request: &CallbackRequest) -> Result<JsonObject, CallbackError> {
        let admission = self
            .journal
            .admit(request)
            .map_err(|error| CallbackError::Unknown(UnknownOutcome::Io(error)))?;
        match admission {
            Admission::New => {}
            Admission::Pending => {
                return Err(CallbackError::Unknown(UnknownOutcome::RetainedInvocation));
            }
            Admission::Conflict => {
                return Err(CallbackError::Unknown(UnknownOutcome::Protocol(
                    CallbackProtocolError::InvocationConflict,
                )));
            }
            Admission::Finished(reply) => return reply.into_result(),
        }

        let reply = match self.callback.invoke(request) {
            Ok(result) => RecordedReply::Completed { result },
            Err(CallbackError::Rejected { message }) => RecordedReply::Rejected { message },
            Err(CallbackError::NotStarted(error)) => RecordedReply::NotStarted {
                message: error.to_string(),
            },
            Err(error @ CallbackError::Unknown(_)) => return Err(error),
        };
        self.journal
            .finish(request, &reply)
            .map_err(|error| CallbackError::Unknown(UnknownOutcome::Io(error)))?;
        reply.into_result()
    }
}

/// Filesystem journal for local hosts. Replicas must compose a journal that
/// shares their durable admission boundary.
#[derive(Clone, Debug)]
pub struct FileInvocationJournal {
    directory: PathBuf,
}

impl FileInvocationJournal {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn path(&self, request: &CallbackRequest, extension: &str) -> PathBuf {
        let key = document_identity("callback-", request.invocation_id());
        self.directory.join(key).with_extension(extension)
    }

    fn admission(&self, request: &CallbackRequest, claim: &Path) -> io::Result<Admission> {
        let retained: Value = match serde_json::from_slice(&fs::read(claim)?) {
            Ok(retained) => retained,
            // Older writers created the public file before writing its bytes.
            // Its input and any external outcome must be reconciled by the host.
            Err(error) if error.is_eof() => return Ok(Admission::Pending),
            Err(error) => return Err(error.into()),
        };
        if retained != serde_json::to_value(request)? {
            return Ok(Admission::Conflict);
        }
        match fs::read(self.path(request, "reply")) {
            Ok(data) => Ok(Admission::Finished(serde_json::from_slice(&data)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Admission::Pending),
            Err(error) => Err(error),
        }
    }
}

impl InvocationJournal for FileInvocationJournal {
    fn admit(&self, request: &CallbackRequest) -> io::Result<Admission> {
        fs::create_dir_all(&self.directory)?;
        let claim = self.path(request, "claim");
        let payload = serde_json::to_vec(request)?;
        match create_projection(&claim, &payload) {
            Ok(()) => Ok(Admission::New),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.admission(request, &claim)
            }
            Err(error) => Err(error),
        }
    }

    fn finish(&self, request: &CallbackRequest, reply: &RecordedReply) -> io::Result<()> {
        replace_projection(&self.path(request, "reply"), &serde_json::to_vec(reply)?)
    }
}

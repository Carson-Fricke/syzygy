//! Invoke a callback command once and validate its correlated JSON reply.

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

use super::{
    CallbackError, CallbackProtocolError, CallbackRequest, JsonObject, UnknownOutcome,
    WorkflowCallback,
};

const REPLY_LIMIT: usize = 4 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub struct CallbackCommand {
    program: OsString,
    arguments: Vec<OsString>,
    directory: Option<PathBuf>,
    timeout: Duration,
}

impl CallbackCommand {
    pub fn new(
        program: impl Into<OsString>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
        timeout: Duration,
    ) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            directory: None,
            timeout,
        }
    }

    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.directory = Some(path.into());
        self
    }

    fn spawn(&self) -> io::Result<Child> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(directory) = &self.directory {
            command.current_dir(directory);
        }
        command.spawn()
    }
}

#[derive(Clone, Debug)]
pub struct ProcessCallback {
    command: CallbackCommand,
}

impl ProcessCallback {
    pub fn new(command: CallbackCommand) -> Self {
        Self { command }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reply {
    invocation_id: String,
    result: Option<JsonObject>,
    error: Option<Rejection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rejection {
    message: String,
}

impl Reply {
    fn result(request: &CallbackRequest, bytes: &[u8]) -> Result<JsonObject, CallbackError> {
        let reply: Self = serde_json::from_slice(bytes)
            .map_err(|error| CallbackError::Unknown(UnknownOutcome::InvalidReply(error)))?;
        if reply.invocation_id != request.invocation_id() {
            return Err(protocol_error(CallbackProtocolError::MismatchedInvocation));
        }
        match (reply.result, reply.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) if !error.message.trim().is_empty() => {
                Err(CallbackError::Rejected {
                    message: error.message,
                })
            }
            (None, Some(_)) => Err(protocol_error(CallbackProtocolError::EmptyField(
                "error.message",
            ))),
            _ => Err(protocol_error(CallbackProtocolError::InvalidOutcome)),
        }
    }
}

fn protocol_error(error: CallbackProtocolError) -> CallbackError {
    CallbackError::Unknown(UnknownOutcome::Protocol(error))
}

fn unknown_io(error: io::Error) -> CallbackError {
    CallbackError::Unknown(UnknownOutcome::Io(error))
}

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

enum PipeResult {
    Written(io::Result<()>),
    Read(io::Result<Vec<u8>>),
}

/// Read and write concurrently so large requests and replies cannot deadlock
/// each other. The deadline covers request transfer, response transfer and exit.
fn exchange(
    child: &mut Child,
    request: Vec<u8>,
    timeout: Duration,
) -> Result<Vec<u8>, CallbackError> {
    let started = Instant::now();
    let mut stdin = child.stdin.take().expect("callback stdin is piped");
    let stdout = child.stdout.take().expect("callback stdout is piped");
    let (output, inputs) = mpsc::channel();
    let writer = output.clone();
    thread::Builder::new()
        .name("callback-stdin".into())
        .spawn(move || {
            let result = stdin.write_all(&request);
            drop(stdin);
            let _ = writer.send(PipeResult::Written(result));
        })
        .map_err(unknown_io)?;
    thread::Builder::new()
        .name("callback-stdout".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take((REPLY_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = output.send(PipeResult::Read(result));
        })
        .map_err(unknown_io)?;

    let mut request_written = false;
    let mut response = None;
    loop {
        if let Some(status) = child.try_wait().map_err(unknown_io)? {
            if !status.success() {
                return Err(CallbackError::Unknown(UnknownOutcome::Exit {
                    code: status.code(),
                }));
            }
            if request_written && let Some(bytes) = response.take() {
                return Ok(bytes);
            }
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(CallbackError::Unknown(UnknownOutcome::TimedOut));
        }
        let poll = POLL_INTERVAL.min(remaining);
        match inputs.recv_timeout(poll) {
            Ok(PipeResult::Written(result)) => {
                result.map_err(unknown_io)?;
                request_written = true;
            }
            Ok(PipeResult::Read(result)) => {
                let bytes = result.map_err(unknown_io)?;
                if bytes.len() > REPLY_LIMIT {
                    return Err(CallbackError::Unknown(UnknownOutcome::OutputLimit));
                }
                response = Some(bytes);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if request_written && response.is_some() => {
                thread::sleep(poll);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(unknown_io(io::Error::other(
                    "callback pipe closed without reporting a result",
                )));
            }
        }
    }
}

impl WorkflowCallback for ProcessCallback {
    type Error = CallbackError;

    fn invoke(&self, request: &CallbackRequest) -> Result<JsonObject, CallbackError> {
        let body = serde_json::to_vec(request)
            .map_err(|error| CallbackError::NotStarted(io::Error::other(error)))?;
        let mut child = RunningChild(self.command.spawn().map_err(CallbackError::NotStarted)?);
        let output = exchange(&mut child.0, body, self.command.timeout)?;
        Reply::result(request, &output)
    }
}

impl WorkflowCallback for &ProcessCallback {
    type Error = CallbackError;

    fn invoke(&self, request: &CallbackRequest) -> Result<JsonObject, CallbackError> {
        (*self).invoke(request)
    }
}

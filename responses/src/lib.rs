//! ChatGPT plan authorization and the public Responses API over HTTP and SSE.
//!
//! The conversation runtime owns durable tool admission. This adapter renders
//! settled history and preserves native items for the selected account and model.

mod account;
mod auth;
mod stream;
mod wire;

use std::{fmt, io::Read, ops::ControlFlow, time::Duration};

use reqwest::{Url, blocking::Client, header};
use serde::de::DeserializeOwned;

pub use account::AccountStore;
pub use auth::{HostId, Session};
pub use stream::{ResponseFrame, ResponseStream, StreamEvent};
pub use syzygy_conversation::{FunctionTool, Tool, ToolNamespace};
use syzygy_conversation::{ModelItem, ModelReply, Provider};
use syzygy_runtime_contracts::OutputDelta;
pub use wire::{PreparedRequest, RequestOptions, ToolChoice};

/// Encodes and streams Responses for a selected ChatGPT session. The turn runtime
/// admits completed replies and authorizes dispatch against its durable timeline.
pub struct ResponsesProvider<'a> {
    client: &'a ChatGpt,
    session: &'a Session,
    model: &'a str,
    options: &'a RequestOptions,
    instructions: Option<&'a str>,
}

impl<'a> ResponsesProvider<'a> {
    pub fn new(
        client: &'a ChatGpt,
        session: &'a Session,
        model: &'a str,
        options: &'a RequestOptions,
    ) -> Self {
        Self {
            client,
            session,
            model,
            options,
            instructions: None,
        }
    }

    pub fn with_instructions(mut self, instructions: &'a str) -> Self {
        self.instructions = Some(instructions);
        self
    }
}

impl Provider for ResponsesProvider<'_> {
    type Error = Error;

    fn respond(
        &mut self,
        history: &[ModelItem],
        emit: &mut dyn FnMut(OutputDelta),
    ) -> Result<ModelReply, Error> {
        self.client
            .respond(self.session, &self.prepare(history)?, |event| {
                if let StreamEvent::Output(delta) = event {
                    emit(delta);
                }
                ControlFlow::Continue(())
            })
    }
}

const API: &str = "https://api.openai.com/v1";
const CLIENT_NAME: &str = concat!("Syzygy/", env!("CARGO_PKG_VERSION"));
const JSON_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Transport,
    Http { status: u16, code: Option<String> },
    Provider { event: String, code: Option<String> },
    Interrupted,
    Cancelled,
    SignIn(&'static str),
    Reconnect,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::SignIn(message) => f.write_str(message),
            Self::Transport => f.write_str("could not communicate with OpenAI"),
            Self::Http { status, code } => write!(
                f,
                "OpenAI HTTP {status} ({})",
                code.as_deref().unwrap_or("unknown")
            ),
            Self::Provider { event, code } => write!(
                f,
                "OpenAI {event} ({})",
                code.as_deref().unwrap_or("unknown")
            ),
            Self::Interrupted => f.write_str("Responses stream ended before completion"),
            Self::Cancelled => f.write_str("Responses stream was cancelled"),
            Self::Reconnect => f.write_str("ChatGPT plan access needs to be renewed"),
        }
    }
}

impl std::error::Error for Error {}

/// Public ChatGPT plan endpoints. Requests use only the selected OAuth grant.
pub struct ChatGpt {
    http: Client,
    api: String,
    oauth: auth::Endpoints,
}

impl ChatGpt {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            http: Client::builder()
                .user_agent(CLIENT_NAME)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| Error::Transport)?,
            api: API.into(),
            oauth: auth::Endpoints::openai(),
        })
    }

    /// Starts the loopback listener before calling the host's browser launcher.
    /// Keep `host` stable across sign-ins. A returning session pins the account
    /// identity and issued client ID until OpenAI verifies the replacement.
    pub fn sign_in(
        &self,
        host: &HostId,
        returning: Option<&Session>,
        launch: impl FnOnce(&Url) -> std::io::Result<()>,
    ) -> Result<Session, Error> {
        auth::sign_in(self, host, returning, launch)
    }

    /// Rotate the selected grant. Hosts must serialize refresh for a stored
    /// account and save the replacement before releasing their storage lock.
    pub fn refresh(&self, session: &mut Session) -> Result<(), Error> {
        auth::refresh(self, session)
    }

    /// Return the models this account can select, in the order OpenAI returns.
    pub fn models(&self, session: &Session) -> Result<Vec<Model>, Error> {
        #[derive(serde::Deserialize)]
        struct Models {
            models: Vec<Model>,
        }
        let list: Models = read_json(checked(
            self.http
                .get(format!("{}/models", self.api))
                .timeout(Duration::from_secs(30))
                .bearer_auth(session.bearer()?)
                .send()
                .map_err(|_| Error::Transport)?,
        )?)?;
        Ok(list
            .models
            .into_iter()
            .filter(|model| model.visibility == "list")
            .collect())
    }

    /// Send the prepared bytes as-is. Only a successful terminal event returns
    /// candidate reply for admission by the turn runtime. Deltas are provisional.
    pub fn respond(
        &self,
        session: &Session,
        request: &PreparedRequest,
        emit: impl FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ModelReply, Error> {
        self.stream(session, request.clone())?.finish(emit)
    }

    /// Lazily read SSE frames from one request, closing the HTTP response when the stream is dropped.
    pub fn stream(
        &self,
        session: &Session,
        request: PreparedRequest,
    ) -> Result<ResponseStream, Error> {
        let scope = session.replay_scope();
        request.check_connection(&scope)?;
        let response = checked(
            self.http
                .post(format!("{}/responses", self.api))
                .timeout(Duration::from_secs(600))
                .bearer_auth(session.bearer()?)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "text/event-stream")
                .body(request.body.clone())
                .send()
                .map_err(|_| Error::Transport)?,
        )?;
        let media_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !media_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        {
            return Err(Error::Invalid("OpenAI returned an unexpected content type"));
        }
        Ok(ResponseStream::new(response, request, scope))
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct Model {
    pub slug: String,
    pub display_name: String,
    #[serde(default)]
    visibility: String,
}

pub(crate) fn read_json<T: DeserializeOwned>(response: impl Read) -> Result<T, Error> {
    let mut bytes = Vec::new();
    response
        .take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Transport)?;
    if bytes.len() as u64 > JSON_LIMIT {
        return Err(Error::Invalid(
            "OpenAI JSON response exceeded its size limit",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("OpenAI returned invalid JSON"))
}

pub(crate) fn error_code(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
        .map(str::to_owned)
}

pub(crate) fn checked(
    response: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let code = read_json::<serde_json::Value>(response)
        .ok()
        .and_then(|value| {
            error_code(&value["error"]["code"]).or_else(|| error_code(&value["error"]))
        });
    Err(Error::Http {
        status: status.as_u16(),
        code,
    })
}

#[cfg(test)]
#[path = "../tests/unit/chatgpt.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod runtime_tests;

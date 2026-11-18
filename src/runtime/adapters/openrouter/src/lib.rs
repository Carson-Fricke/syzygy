//! OpenRouter chat completions for the portable conversation runtime.

use std::{
    collections::HashMap,
    io::{self, Read},
    time::Duration,
};

use reqwest::{blocking::Client, header};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use syzygy_provider_projection::{
    CompletionStream, REASONING_DETAILS, SCALAR_REASONING_FIELDS, SseDecoder, StreamCompletion,
};
use syzygy_runtime_contracts::model::{
    ModelItem, ModelReply, OutputDelta, Provider, Tool, ToolCall, ToolInput, ToolNamespace,
    ToolOutput, validate_calls, validate_tools,
};

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouter {
    http: Client,
    endpoint: String,
    scope: String,
    model: String,
    instructions: String,
    reasoning_effort: Option<String>,
    tools: Vec<Value>,
    names: HashMap<String, (String, String)>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Map namespaces onto chat completions' single function name. Preserve readable
/// names when unambiguous; keep longer or ambiguous names stable across runs.
fn wire_name(namespace: &str, name: &str) -> String {
    let readable = format!("{namespace}__{name}");
    if readable.len() <= 64
        && !namespace.contains("__")
        && !name.contains("__")
        && !namespace.ends_with('_')
        && !name.starts_with('_')
    {
        return readable;
    }
    let digest = format!("{:x}", Sha256::digest(format!("{namespace}/{name}")));
    format!("tool_{}", &digest[..59])
}

impl OpenRouter {
    /// A local, credential-free binding for resumable chats. Rotating the
    /// key changes the binding even if both keys belong to the same account.
    pub fn replay_scope(api_key: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(b"syzygy:openrouter:api-key:v1\0");
        digest.update(api_key.as_bytes());
        format!("{:x}", digest.finalize())
    }

    pub fn new(
        api_key: &str,
        model: &str,
        tools: &[ToolNamespace],
        instructions: &str,
        reasoning_effort: Option<&str>,
    ) -> io::Result<Self> {
        if api_key.trim().is_empty() || model.trim().is_empty() {
            return Err(invalid("OpenRouter needs an API key and a model"));
        }
        validate_tools(tools).map_err(invalid)?;
        let mut schemas = Vec::new();
        let mut names = HashMap::new();
        for group in tools {
            for tool in &group.tools {
                let Tool::Function(function) = tool else {
                    return Err(invalid(
                        "OpenRouter chat completions requires JSON function tools",
                    ));
                };
                let alias = wire_name(&group.name, &function.name);
                if names
                    .insert(alias.clone(), (group.name.clone(), function.name.clone()))
                    .is_some()
                {
                    return Err(invalid("OpenRouter tool names conflict"));
                }
                schemas.push(json!({"type":"function","function":{
                    "name":alias,"description":function.description,"parameters":function.parameters,"strict":function.strict
                }}));
            }
        }
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| invalid("invalid OpenRouter API key"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("text/event-stream"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            http,
            endpoint: ENDPOINT.into(),
            scope: Self::replay_scope(api_key),
            model: model.into(),
            instructions: instructions.into(),
            reasoning_effort: reasoning_effort.map(str::to_owned),
            tools: schemas,
            names,
        })
    }

    fn request(&self, history: &[ModelItem]) -> io::Result<Value> {
        if history.is_empty() {
            return Err(invalid("empty conversation"));
        }
        let mut messages = Vec::new();
        let mut previous = Vec::new();
        if !self.instructions.is_empty() {
            messages.push(json!({"role":"system","content":self.instructions}));
        }
        for item in history {
            match item {
                ModelItem::Developer(text) => {
                    messages.push(json!({"role":"system","content":text}))
                }
                ModelItem::User(text) => messages.push(json!({"role":"user","content":text})),
                ModelItem::Assistant { reply, results } => {
                    validate_calls(&reply.calls, previous.iter().copied())
                        .map_err(io::Error::other)?;
                    previous.extend(reply.calls.iter().map(|c| c.call_id.as_str()));
                    let mut message = json!({"role":"assistant","content":reply.text});
                    if !reply.replay.is_null() {
                        if reply.replay["provider"] != "openrouter"
                            || reply.replay["connection"] != self.scope
                            || reply.replay["model"] != self.model
                            || reply.replay["id"] != reply.id
                        {
                            return Err(invalid(
                                "OpenRouter replay scope does not match the selected key, provider, model or reply",
                            ));
                        }
                        for key in SCALAR_REASONING_FIELDS
                            .into_iter()
                            .chain([REASONING_DETAILS])
                        {
                            if let Some(value) = reply.replay.get(key) {
                                message[key] = value.clone();
                            }
                        }
                    } else if !reply.calls.is_empty() {
                        return Err(invalid(
                            "tool calls require native OpenRouter replay evidence",
                        ));
                    }
                    if reply.calls.len() != results.len() {
                        return Err(invalid("incomplete OpenRouter tool result group"));
                    }
                    let mut wire_calls = Vec::new();
                    let mut outputs = Vec::new();
                    for (call, result) in reply.calls.iter().zip(results) {
                        let ToolInput::Function { arguments } = &call.input else {
                            return Err(invalid("unsupported OpenRouter tool input"));
                        };
                        let namespace = call
                            .namespace
                            .as_deref()
                            .ok_or_else(|| invalid("missing OpenRouter tool namespace"))?;
                        if result.call_id != call.call_id {
                            return Err(invalid("OpenRouter result differs from its call"));
                        }
                        wire_calls.push(json!({"type":"function","id":call.call_id,"function":{"name":wire_name(namespace,&call.name),"arguments":arguments}}));
                        let content = match &result.output {
                            ToolOutput::Text(text) => text.clone(),
                            ToolOutput::Content(parts) => serde_json::to_string(parts)?,
                        };
                        outputs.push(
                            json!({"role":"tool","tool_call_id":call.call_id,"content":content}),
                        );
                    }
                    if !wire_calls.is_empty() {
                        message["tool_calls"] = wire_calls.into();
                    }
                    messages.push(message);
                    messages.extend(outputs);
                }
            }
        }
        let mut request = json!({"model":self.model,"messages":messages,"stream":true});
        if !self.tools.is_empty() {
            request["tools"] = json!(self.tools);
        }
        if let Some(effort) = &self.reasoning_effort {
            request["reasoning"] = json!({"effort":effort});
        }
        Ok(request)
    }

    fn reply(&self, completion: StreamCompletion) -> io::Result<ModelReply> {
        let body = completion.body;
        let id = body["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("missing OpenRouter completion identity"))?;
        let choice = &body["choices"][0];
        let native = &choice["message"];
        let text = native["content"].as_str().unwrap_or_default();
        let mut calls = Vec::new();
        for call in native["tool_calls"]
            .as_array()
            .ok_or_else(|| invalid("invalid OpenRouter tool calls"))?
        {
            let field = |value: &Value, key| {
                value[key]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("incomplete OpenRouter tool call"))
            };
            let function = &call["function"];
            let alias = field(function, "name")?;
            let (namespace, name) = self
                .names
                .get(&alias)
                .ok_or_else(|| invalid("OpenRouter requested an unadvertised tool"))?;
            let arguments = field(function, "arguments")?;
            if !serde_json::from_str::<Value>(&arguments).is_ok_and(|v| v.is_object()) {
                return Err(invalid(
                    "OpenRouter function arguments must encode an object",
                ));
            }
            calls.push(ToolCall {
                call_id: field(call, "id")?,
                namespace: Some(namespace.clone()),
                name: name.clone(),
                input: ToolInput::Function { arguments },
            });
        }
        match choice["finish_reason"].as_str() {
            Some("stop") if !text.is_empty() || !calls.is_empty() => (),
            Some("tool_calls") if !calls.is_empty() => (),
            _ => {
                return Err(invalid(
                    "OpenRouter stream ended without a complete answer or tool call",
                ));
            }
        }
        let mut replay = Map::from_iter([
            ("provider".into(), json!("openrouter")),
            ("connection".into(), json!(self.scope)),
            ("model".into(), json!(self.model)),
            ("id".into(), json!(id)),
        ]);
        if let Some(usage) = body.get("usage") {
            replay.insert("usage".into(), usage.clone());
        }
        for key in SCALAR_REASONING_FIELDS
            .into_iter()
            .chain([REASONING_DETAILS])
        {
            if let Some(value) = native.get(key) {
                replay.insert(key.into(), value.clone());
            }
        }
        Ok(ModelReply {
            id: id.to_owned(),
            text: text.to_owned(),
            calls,
            replay: replay.into(),
        })
    }
}

#[cfg(test)]
#[path = "../tests/unit/provider.rs"]
mod tests;

impl Provider for OpenRouter {
    type Error = io::Error;

    fn respond(
        &mut self,
        history: &[ModelItem],
        emit: &mut dyn FnMut(OutputDelta),
    ) -> io::Result<ModelReply> {
        let request = self.request(history)?;
        let mut response = self
            .http
            .post(&self.endpoint)
            .json(&request)
            .send()
            .map_err(io::Error::other)?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "OpenRouter returned HTTP {}",
                response.status()
            )));
        }
        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !media
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/event-stream")
        {
            return Err(invalid("OpenRouter response is not an SSE stream"));
        }
        let mut decoder = SseDecoder::default();
        let mut stream = CompletionStream::default();
        let mut buffer = [0u8; 8192];
        let mut bytes = 0;
        loop {
            let read = response.read(&mut buffer)?;
            bytes += read;
            if bytes > 32 * 1024 * 1024 {
                return Err(invalid("OpenRouter stream exceeds its byte limit"));
            }
            decoder
                .push(&buffer[..read])
                .map_err(|error| invalid(error.to_string()))?;
            while let Some(frame) = decoder
                .next_event(read == 0)
                .map_err(|error| invalid(error.to_string()))?
            {
                if let Some(completion) =
                    stream.push(&frame, &mut *emit).map_err(io::Error::other)?
                {
                    let reply = self.reply(completion)?;
                    let previous = history
                        .iter()
                        .filter_map(|item| match item {
                            ModelItem::Assistant { reply, .. } => Some(&reply.calls),
                            _ => None,
                        })
                        .flatten()
                        .map(|call| call.call_id.as_str());
                    validate_calls(&reply.calls, previous).map_err(io::Error::other)?;
                    return Ok(reply);
                }
            }
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "OpenRouter stream ended before [DONE]",
                ));
            }
        }
    }
}

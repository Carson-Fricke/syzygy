//! Ordered Responses items, request encoding, and correlated tool replay.

use serde::Serialize;
use serde_json::{Value, json};
use syzygy_runtime_contracts::model::{ModelItem, ModelReply, ToolCall, ToolInput};

use crate::{Error, ResponsesProvider};
use syzygy_runtime_contracts::model::{ToolNamespace, valid_tool_name, validate_tools};

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
}

#[derive(Clone, Debug, Default)]
pub struct RequestOptions {
    pub tools: Vec<ToolNamespace>,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub text: Option<Value>,
    pub prompt_cache_key: Option<String>,
}

/// Exact, credential-free request bytes available before dispatch for auditing.
#[derive(Clone)]
pub struct PreparedRequest {
    pub(crate) body: String,
    pub(crate) model: String,
    pub(crate) connection: Option<String>,
    prior_calls: Vec<String>,
}

impl PreparedRequest {
    pub fn body(&self) -> &str {
        &self.body
    }

    pub(crate) fn reply(
        &self,
        connection: &str,
        id: String,
        output: Vec<Value>,
        usage: Option<Value>,
    ) -> Result<ModelReply, Error> {
        let calls = decode_calls(&output)?;
        syzygy_runtime_contracts::model::validate_calls(
            &calls,
            self.prior_calls.iter().map(String::as_str),
        )
        .map_err(|_| Error::Invalid("duplicate, reused or invalid Responses tool call"))?;
        let text = output_text(&output);
        let replay = json!({
            "provider": "openai-responses",
            "id": id,
            "model": self.model,
            "connection": connection,
            "output": output,
            "usage": usage,
        });
        Ok(ModelReply {
            id,
            calls,
            text,
            replay,
        })
    }
}

fn text_message(role: &str, text: &str) -> Value {
    json!({
        "type": "message", "role": role,
        "content": [{"type": "input_text", "text": text}]
    })
}

impl ResponsesProvider<'_> {
    /// Render history already settled by the turn runtime. The runtime supplies results in call order.
    pub fn prepare(&self, history: &[ModelItem]) -> Result<PreparedRequest, Error> {
        let mut request =
            PreparedRequest::from_history(self.model, self.instructions, history, self.options)?;
        let scope = self.session.replay_scope();
        request.check_connection(&scope)?;
        request.connection = Some(scope);
        Ok(request)
    }
}

impl PreparedRequest {
    /// Check retained native input against the selected subscription account before audit or send.
    pub fn check_connection(&self, scope: &str) -> Result<(), Error> {
        if self
            .connection
            .as_deref()
            .is_some_and(|expected| expected != scope)
        {
            return Err(Error::Invalid(
                "request belongs to another ChatGPT account registration",
            ));
        }
        Ok(())
    }

    /// Build the exact wire body from canonical history without reading credentials or making requests.
    pub fn from_history(
        model: &str,
        instructions: Option<&str>,
        history: &[ModelItem],
        options: &RequestOptions,
    ) -> Result<Self, Error> {
        if model.trim().is_empty() || history.is_empty() {
            return Err(Error::Invalid(
                "a model and conversation input are required",
            ));
        }
        let mut connection = None;
        let mut prior_calls = Vec::new();
        let mut input = Vec::new();
        if let Some(instructions) = instructions {
            input.push(text_message("developer", instructions));
        }
        for item in history {
            match item {
                ModelItem::Developer(text) => input.push(text_message("developer", text)),
                ModelItem::User(text) => input.push(text_message("user", text)),
                ModelItem::Assistant { reply, results } => {
                    let native = &reply.replay;
                    if native.is_null() && reply.calls.is_empty() && results.is_empty() {
                        input.push(json!({"role":"assistant","content":reply.text}));
                        continue;
                    }
                    let scope = native["connection"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or(Error::Invalid(
                            "native Responses history lacks an account registration",
                        ))?;
                    if native["provider"] != "openai-responses"
                        || native["model"] != model
                        || connection.as_deref().is_some_and(|prior| prior != scope)
                    {
                        return Err(Error::Invalid(
                            "native Responses history belongs to another provider, account or model",
                        ));
                    }
                    let output = native["output"]
                        .as_array()
                        .ok_or(Error::Invalid("invalid native Responses replay"))?;
                    connection.get_or_insert_with(|| scope.to_owned());
                    if native["id"] != reply.id || decode_calls(output)? != reply.calls {
                        return Err(Error::Invalid(
                            "native Responses reply differs from retained history",
                        ));
                    }
                    syzygy_runtime_contracts::model::validate_calls(
                        &reply.calls,
                        prior_calls.iter().map(String::as_str),
                    )
                    .map_err(|_| Error::Invalid("invalid retained Responses tool call"))?;
                    prior_calls.extend(reply.calls.iter().map(|call| call.call_id.clone()));
                    input.extend(output.iter().cloned());
                    if reply.calls.len() != results.len() {
                        return Err(Error::Invalid("incomplete tool results in replay"));
                    }
                    for (call, result) in reply.calls.iter().zip(results) {
                        if call.call_id != result.call_id {
                            return Err(Error::Invalid("tool result out of order in replay"));
                        }
                        let kind = match call.input {
                            ToolInput::Function { .. } => "function_call_output",
                            ToolInput::Custom { .. } => "custom_tool_call_output",
                        };
                        input.push(
                            json!({"type":kind, "call_id":call.call_id, "output":result.output}),
                        );
                    }
                }
            }
        }
        validate_tools(&options.tools).map_err(Error::Invalid)?;
        let tools: Vec<_> = options
            .tools
            .iter()
            .map(|namespace| {
                json!({
                    "type": "namespace",
                    "name": namespace.name,
                    "description": namespace.description,
                    "tools": namespace.tools,
                })
            })
            .collect();
        let mut request = json!({
            "model": model,
            "input": input,
            "tools": tools,
            "tool_choice": options.tool_choice,
            "parallel_tool_calls": options.parallel_tool_calls,
            "stream": true,
            "store": false,
        });
        if let Some(effort) = &options.reasoning_effort {
            request["reasoning"]["effort"] = json!(effort);
        }
        if let Some(summary) = &options.reasoning_summary {
            request["reasoning"]["summary"] = json!(summary);
        }
        if let Some(text) = &options.text {
            if !text.is_object() {
                return Err(Error::Invalid("Responses text controls must be an object"));
            }
            request["text"] = text.clone();
        }
        if let Some(key) = &options.prompt_cache_key {
            request["prompt_cache_key"] = json!(key);
        }
        Ok(PreparedRequest {
            body: serde_json::to_string(&request)
                .map_err(|_| Error::Invalid("could not encode Responses request"))?,
            connection,
            prior_calls,
            model: model.to_owned(),
        })
    }
}

fn output_text(output: &[Value]) -> String {
    output
        .iter()
        .filter(|item| item["type"] == "message")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|content| match content["type"].as_str() {
            Some("output_text") => content["text"].as_str(),
            Some("refusal") => content["refusal"].as_str(),
            _ => None,
        })
        .collect()
}

fn decode_calls(output: &[Value]) -> Result<Vec<ToolCall>, Error> {
    let mut calls = Vec::new();
    for item in output {
        let input = match item["type"].as_str() {
            Some("function_call") => {
                let arguments = field(item, "arguments")?.to_owned();
                if !serde_json::from_str::<Value>(&arguments).is_ok_and(|value| value.is_object()) {
                    return Err(Error::Invalid(
                        "function arguments must encode a JSON object",
                    ));
                }
                ToolInput::Function { arguments }
            }
            Some("custom_tool_call") => ToolInput::Custom {
                input: field(item, "input")?.to_owned(),
            },
            _ => continue,
        };
        let call_id = field(item, "call_id")?;
        let name = field(item, "name")?;
        if call_id.is_empty() || !valid_tool_name(name) {
            return Err(Error::Invalid("invalid Responses tool call"));
        }
        let namespace = item
            .get("namespace")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| valid_tool_name(name))
                    .ok_or(Error::Invalid("invalid Responses tool namespace"))
            })
            .transpose()?
            .map(str::to_owned);
        calls.push(ToolCall {
            call_id: call_id.to_owned(),
            namespace,
            name: name.to_owned(),
            input,
        });
    }
    Ok(calls)
}

fn field<'a>(item: &'a Value, name: &str) -> Result<&'a str, Error> {
    item[name].as_str().ok_or(Error::Invalid(
        "Responses tool call is missing a string field",
    ))
}

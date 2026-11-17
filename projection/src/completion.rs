//! Assemble an OpenAI-compatible completion from incremental provider events.

use std::{collections::BTreeMap, fmt, mem};

use serde_json::{Map, Value, json};
use syzygy_runtime_contracts::{OutputChannel, OutputDelta};

use crate::reasoning::{Reasoning, display_text};

const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOL_CALLS: usize = 128;

#[derive(Clone, Debug)]
pub struct StreamError {
    pub message: &'static str,
    pub status: Option<String>,
    pub transient: bool,
}

impl StreamError {
    pub(crate) fn invalid(message: &'static str) -> Self {
        Self {
            message,
            status: None,
            transient: false,
        }
    }

    pub fn interrupted(message: &'static str) -> Self {
        Self {
            transient: true,
            ..Self::invalid(message)
        }
    }

    fn upstream(detail: Option<&Value>, message: &'static str) -> Self {
        let status = detail
            .and_then(|detail| detail.get("code"))
            .and_then(integer_text);
        Self {
            message,
            transient: status.is_none(),
            status,
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for StreamError {}

pub struct StreamCompletion {
    pub body: Value,
    pub display_reasoning: String,
}

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCall {
    fn into_value(self) -> Value {
        json!({
            "id": self.id,
            "type": "function",
            "function": {"name": self.name, "arguments": self.arguments}
        })
    }
}

type ToolIndex = (usize, String);

#[derive(Default)]
pub struct CompletionStream {
    answer: String,
    reasoning: Reasoning,
    display_reasoning: String,
    calls: BTreeMap<ToolIndex, ToolCall>,
    metadata: Map<String, Value>,
    finish_reason: Option<Value>,
    size: usize,
}

impl CompletionStream {
    /// Preserve already-emitted deltas if validation fails later in this event.
    pub fn push(
        &mut self,
        raw: &str,
        mut emit: impl FnMut(OutputDelta),
    ) -> Result<Option<StreamCompletion>, StreamError> {
        if raw == "[DONE]" {
            return self.finish().map(Some);
        }
        let data: Value = serde_json::from_str(raw)
            .map_err(|_| StreamError::invalid("provider emitted malformed stream JSON"))?;
        let data = data.as_object().ok_or(StreamError::invalid(
            "provider emitted malformed stream data",
        ))?;
        if let Some(error) = data.get("error").filter(|value| !value.is_null()) {
            return Err(StreamError::upstream(
                Some(error),
                "provider reported a stream error",
            ));
        }
        for key in ["id", "model", "provider", "usage"] {
            if let Some(value) = data.get(key).filter(|value| !value.is_null()) {
                self.metadata.insert(key.to_owned(), value.clone());
            }
        }
        if let Some(choices) = data.get("choices") {
            let choices = choices.as_array().ok_or(StreamError::invalid(
                "provider emitted malformed stream choices",
            ))?;
            for choice in choices {
                self.choice(choice, &mut emit)?;
            }
        }
        Ok(None)
    }

    fn finish(&mut self) -> Result<StreamCompletion, StreamError> {
        let finish_reason = self.finish_reason.take().ok_or(StreamError::invalid(
            "provider stream omitted its finish reason",
        ))?;
        let mut message = mem::take(&mut self.reasoning).into_fields();
        message.insert(
            "content".to_owned(),
            Value::String(mem::take(&mut self.answer)),
        );
        let calls = mem::take(&mut self.calls)
            .into_values()
            .map(ToolCall::into_value)
            .collect();
        message.insert("tool_calls".to_owned(), Value::Array(calls));
        let mut body = mem::take(&mut self.metadata);
        body.insert(
            "choices".to_owned(),
            json!([{"message": message, "finish_reason": finish_reason}]),
        );
        Ok(StreamCompletion {
            body: Value::Object(body),
            display_reasoning: mem::take(&mut self.display_reasoning),
        })
    }

    fn choice(
        &mut self,
        choice: &Value,
        emit: &mut impl FnMut(OutputDelta),
    ) -> Result<(), StreamError> {
        let unexpected = || StreamError::invalid("provider emitted an unexpected stream choice");
        let choice = choice.as_object().ok_or_else(unexpected)?;
        if choice
            .get("index")
            .is_some_and(|index| !json_equal(index, &json!(0)))
        {
            return Err(unexpected());
        }
        let finish = choice.get("finish_reason").filter(|value| !value.is_null());
        let error = choice.get("error").filter(|value| !value.is_null());
        if finish.and_then(Value::as_str) == Some("error") || error.is_some() {
            return Err(StreamError::upstream(
                error,
                "provider reported an interrupted completion",
            ));
        }
        let empty = Map::new();
        let delta = match choice.get("delta") {
            Some(value) => value
                .as_object()
                .ok_or(StreamError::invalid("provider emitted a malformed delta"))?,
            None => &empty,
        };
        let answer = delta.get("content").filter(|value| !value.is_null());
        let thought = display_text(delta);
        let tools = delta.get("tool_calls").filter(|value| truthy(value));
        let details = delta.get("reasoning_details");
        if self.finish_reason.is_some()
            && (answer.is_some_and(truthy)
                || !thought.is_empty()
                || tools.is_some()
                || details.is_some_and(truthy))
        {
            return Err(StreamError::invalid(
                "provider emitted content after its finish reason",
            ));
        }
        self.count(details.map_or(2, json_ascii_length))?;
        self.reasoning.add(delta)?;
        if !thought.is_empty() {
            self.count(thought.len())?;
            self.display_reasoning.push_str(&thought);
            emit(OutputDelta {
                channel: OutputChannel::Reasoning,
                text: thought,
            });
        }
        if let Some(answer) = answer {
            let answer = answer
                .as_str()
                .ok_or(StreamError::invalid("provider emitted non-text output"))?;
            self.count(answer.len())?;
            if !answer.is_empty() {
                self.answer.push_str(answer);
                emit(OutputDelta {
                    channel: OutputChannel::Text,
                    text: answer.to_owned(),
                });
            }
        }
        if let Some(tools) = tools {
            let tools = tools.as_array().ok_or(StreamError::invalid(
                "provider emitted malformed tool calls",
            ))?;
            for tool in tools {
                self.tool(tool)?;
            }
        }
        if let Some(finish) = finish {
            self.finish_reason = Some(finish.clone());
        }
        Ok(())
    }

    fn tool(&mut self, call: &Value) -> Result<(), StreamError> {
        let index = call
            .get("index")
            .and_then(tool_index)
            .ok_or(StreamError::invalid(
                "provider tool delta requires a valid index",
            ))?;
        if let Some(kind) = call.get("type").filter(|value| !value.is_null())
            && kind.as_str() != Some("function")
        {
            return Err(StreamError::invalid(
                "provider emitted an unsupported tool type",
            ));
        }
        if self.calls.len() >= MAX_TOOL_CALLS && !self.calls.contains_key(&index) {
            return Err(StreamError::invalid("provider emitted too many tool calls"));
        }
        let call = call.as_object().ok_or(StreamError::invalid(
            "provider tool delta requires a valid index",
        ))?;
        let empty = Map::new();
        let function = match call.get("function") {
            Some(value) => value.as_object().ok_or(StreamError::invalid(
                "provider emitted malformed tool arguments",
            ))?,
            None => &empty,
        };
        // Validate each fragment in the order it contributes to the output cap.
        let id = self.fragment(call, "id")?;
        let tool = self.calls.entry(index.clone()).or_default();
        if tool.id.is_empty() {
            tool.id.push_str(id);
        } else if !id.is_empty() && id != tool.id {
            return Err(StreamError::invalid(
                "provider changed a tool call identity",
            ));
        }
        let name = self.fragment(function, "name")?;
        let arguments = self.fragment(function, "arguments")?;
        if let Some(tool) = self.calls.get_mut(&index) {
            tool.name.push_str(name);
            tool.arguments.push_str(arguments);
        }
        Ok(())
    }

    fn fragment<'a>(
        &mut self,
        source: &'a Map<String, Value>,
        key: &str,
    ) -> Result<&'a str, StreamError> {
        let value = match source.get(key) {
            None | Some(Value::Null) => "",
            Some(Value::String(value)) => value,
            _ => {
                return Err(StreamError::invalid(
                    "provider emitted a malformed tool fragment",
                ));
            }
        };
        self.count(value.len())?;
        Ok(value)
    }

    fn count(&mut self, bytes: usize) -> Result<(), StreamError> {
        self.size = self.size.saturating_add(bytes);
        if self.size > MAX_OUTPUT_BYTES {
            return Err(StreamError::invalid(
                "provider completion exceeded the output limit",
            ));
        }
        Ok(())
    }
}

fn integer_text(value: &Value) -> Option<String> {
    let Value::Number(number) = value else {
        return None;
    };
    let text = number.to_string();
    text.trim_start_matches('-')
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(text)
}

pub(crate) fn tool_index(value: &Value) -> Option<ToolIndex> {
    let number = integer_text(value)?;
    let number = if number == "-0" {
        "0".to_owned()
    } else {
        number
    };
    (!number.starts_with('-')).then_some((number.len(), number))
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(crate) fn json_equal(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    fn scalar_number(value: &Value) -> Option<f64> {
        match value {
            Value::Number(number) => number.as_f64(),
            Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    match (scalar_number(left), scalar_number(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

// Match the byte accounting used for detached ASCII reasoning JSON, including
// separators and supplementary Unicode encoded as a surrogate pair.
fn json_ascii_length(value: &Value) -> usize {
    fn string(value: &str) -> usize {
        2 + value
            .chars()
            .map(|ch| match ch {
                '"' | '\\' | '\u{8}' | '\u{9}' | '\u{a}' | '\u{c}' | '\u{d}' => 2,
                '\u{20}'..='\u{7e}' => 1,
                '\u{0}'..='\u{ffff}' => 6,
                _ => 12,
            })
            .sum::<usize>()
    }
    match value {
        Value::Null => 4,
        Value::Bool(value) => {
            if *value {
                4
            } else {
                5
            }
        }
        Value::Number(value) => value.to_string().len(),
        Value::String(text) => string(text),
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) * 2
                + items.iter().map(json_ascii_length).sum::<usize>()
        }
        Value::Object(items) => {
            2 + items.len().saturating_sub(1) * 2
                + items
                    .iter()
                    .map(|(key, value)| string(key) + 2 + json_ascii_length(value))
                    .sum::<usize>()
        }
    }
}

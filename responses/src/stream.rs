//! Decode completed Responses items separately from visible and argument deltas.

use std::{collections::BTreeMap, io::Read, ops::ControlFlow};

use serde_json::Value;
use syzygy_provider_projection::SseDecoder;
use syzygy_runtime_contracts::{OutputChannel, OutputDelta};

use syzygy_conversation::ModelReply;

use crate::{Error, PreparedRequest, error_code};

const STREAM_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Output(OutputDelta),
    ToolInputDelta { item_id: String, delta: String },
}

/// A terminal reply is ready for durable admission. An update may be displayed immediately.
pub enum ResponseFrame {
    Delta(StreamEvent),
    Completed(ModelReply),
}

/// An owned HTTP stream. Dropping it closes the response without spawning a background reader.
pub struct ResponseStream {
    source: Option<Box<dyn Read + Send>>,
    decoder: SseDecoder,
    items: BTreeMap<u64, Value>,
    response_id: Option<String>,
    size: usize,
    eof: bool,
    request: PreparedRequest,
    connection: String,
}

impl ResponseStream {
    pub(crate) fn new(
        source: impl Read + Send + 'static,
        request: PreparedRequest,
        connection: String,
    ) -> Self {
        Self {
            source: Some(Box::new(source)),
            decoder: SseDecoder::default(),
            items: BTreeMap::new(),
            response_id: None,
            size: 0,
            eof: false,
            request,
            connection,
        }
    }

    pub fn next_frame(&mut self) -> Result<Option<ResponseFrame>, Error> {
        if self.source.is_none() {
            return Ok(None);
        }
        match self.read_frame() {
            Ok(delta @ ResponseFrame::Delta(_)) => Ok(Some(delta)),
            terminal => {
                self.source.take();
                terminal.map(Some)
            }
        }
    }

    pub fn finish(
        mut self,
        mut emit: impl FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ModelReply, Error> {
        while let Some(frame) = self.next_frame()? {
            match frame {
                ResponseFrame::Delta(delta) => {
                    if emit(delta).is_break() {
                        return Err(Error::Cancelled);
                    }
                }
                ResponseFrame::Completed(reply) => return Ok(reply),
            }
        }
        Err(Error::Interrupted)
    }

    fn read_frame(&mut self) -> Result<ResponseFrame, Error> {
        loop {
            let Some(data) = self
                .decoder
                .next_event(self.eof)
                .map_err(|_| Error::Invalid("invalid Responses SSE frame"))?
            else {
                if self.eof {
                    return Err(Error::Interrupted);
                }
                let mut buffer = [0; 8192];
                let count = self
                    .source
                    .as_mut()
                    .ok_or(Error::Interrupted)?
                    .read(&mut buffer)
                    .map_err(|_| Error::Interrupted)?;
                self.eof = count == 0;
                self.size += count;
                if self.size > STREAM_LIMIT {
                    return Err(Error::Invalid("Responses stream exceeded its size limit"));
                }
                self.decoder
                    .push(&buffer[..count])
                    .map_err(|_| Error::Invalid("invalid Responses SSE frame"))?;
                continue;
            };
            let event: Value = serde_json::from_str(&data)
                .map_err(|_| Error::Invalid("invalid Responses event JSON"))?;
            let kind = event["type"]
                .as_str()
                .ok_or(Error::Invalid("Responses event is missing its type"))?;
            let id = if kind == "response.created" || kind == "response.completed" {
                event["response"]["id"].as_str()
            } else {
                event["response_id"].as_str()
            };
            if let Some(id) = id {
                if self
                    .response_id
                    .as_deref()
                    .is_some_and(|previous| previous != id)
                {
                    return Err(Error::Invalid("Responses stream mixed response IDs"));
                }
                self.response_id.get_or_insert_with(|| id.to_owned());
            }
            let update = match kind {
                "response.output_text.delta" | "response.refusal.delta" => {
                    Some(output_delta(&event, OutputChannel::Text)?)
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    Some(output_delta(&event, OutputChannel::Reasoning)?)
                }
                "response.function_call_arguments.delta"
                | "response.custom_tool_call_input.delta" => Some(StreamEvent::ToolInputDelta {
                    item_id: string(&event["item_id"])?.to_owned(),
                    delta: string(&event["delta"])?.to_owned(),
                }),
                "response.output_item.done" => {
                    let index = event["output_index"].as_u64().ok_or(Error::Invalid(
                        "completed Responses item is missing its index",
                    ))?;
                    if !event["item"].is_object()
                        || self.items.insert(index, event["item"].clone()).is_some()
                    {
                        return Err(Error::Invalid(
                            "invalid or duplicate completed Responses item",
                        ));
                    }
                    None
                }
                "response.completed" => {
                    let response = &event["response"];
                    let id = string(&response["id"])?;
                    let output = match response.get("output") {
                        Some(Value::Array(output)) => {
                            for (index, item) in &self.items {
                                let terminal = usize::try_from(*index)
                                    .ok()
                                    .and_then(|index| output.get(index));
                                if !terminal.is_some_and(|terminal| {
                                    [
                                        "type",
                                        "id",
                                        "call_id",
                                        "name",
                                        "namespace",
                                        "arguments",
                                        "input",
                                    ]
                                    .iter()
                                    .all(|field| {
                                        item.get(field)
                                            .is_none_or(|value| terminal.get(field) == Some(value))
                                    })
                                }) {
                                    return Err(Error::Invalid(
                                        "terminal Responses output differed from the streamed item",
                                    ));
                                }
                            }
                            output.clone()
                        }
                        Some(_) => return Err(Error::Invalid("invalid terminal Responses output")),
                        None => {
                            if self.items.keys().copied().ne(0..self.items.len() as u64) {
                                return Err(Error::Invalid(
                                    "Responses stream skipped a completed item",
                                ));
                            }
                            std::mem::take(&mut self.items).into_values().collect()
                        }
                    };
                    if id.is_empty()
                        || output.is_empty()
                        || output.iter().any(|item| !item["type"].is_string())
                    {
                        return Err(Error::Invalid(
                            "completed Responses stream is missing output or its identity",
                        ));
                    }
                    let usage = response
                        .get("usage")
                        .filter(|usage| !usage.is_null())
                        .cloned();
                    return self
                        .request
                        .reply(&self.connection, id.to_owned(), output, usage)
                        .map(ResponseFrame::Completed);
                }
                "response.failed" | "response.incomplete" | "error" => {
                    let code = error_code(&event["response"]["error"]["code"])
                        .or_else(|| error_code(&event["response"]["incomplete_details"]["reason"]))
                        .or_else(|| error_code(&event["error"]["code"]))
                        .or_else(|| error_code(&event["code"]));
                    return Err(Error::Provider {
                        event: kind.to_owned(),
                        code,
                    });
                }
                _ => None,
            };
            if let Some(update) = update {
                return Ok(ResponseFrame::Delta(update));
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn decode(
    source: &[u8],
    request: &PreparedRequest,
    connection: &str,
    emit: impl FnMut(StreamEvent) -> ControlFlow<()>,
) -> Result<ModelReply, Error> {
    ResponseStream::new(
        std::io::Cursor::new(source.to_vec()),
        request.clone(),
        connection.into(),
    )
    .finish(emit)
}

fn string(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .ok_or(Error::Invalid("Responses event is missing a string field"))
}

fn output_delta(event: &Value, channel: OutputChannel) -> Result<StreamEvent, Error> {
    Ok(StreamEvent::Output(OutputDelta {
        channel,
        text: string(&event["delta"])?.to_owned(),
    }))
}

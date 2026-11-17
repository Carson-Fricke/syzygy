//! Assemble readable reasoning aliases and opaque continuation blocks.

use serde_json::{Map, Value};

use crate::completion::StreamError;

pub const SCALAR_REASONING_FIELDS: [&str; 2] = ["reasoning", "reasoning_content"];
pub const REASONING_DETAILS: &str = "reasoning_details";

pub fn readable_block_field(kind: &str) -> Option<&'static str> {
    match kind {
        "reasoning.text" => Some("text"),
        "reasoning.summary" => Some("summary"),
        _ => None,
    }
}

fn display_field(block: &Map<String, Value>) -> Option<&'static str> {
    readable_block_field(block.get("type")?.as_str()?)
}

fn finite_values(value: &Value) -> bool {
    match value {
        Value::Number(value) => {
            value.as_f64().is_some_and(f64::is_finite)
                || !value.to_string().contains(['.', 'e', 'E'])
        }
        Value::Array(values) => values.iter().all(finite_values),
        Value::Object(values) => values.values().all(finite_values),
        _ => true,
    }
}

pub(crate) fn display_text(fields: &Map<String, Value>) -> String {
    if let Some(text) = SCALAR_REASONING_FIELDS
        .iter()
        .filter_map(|name| fields.get(*name).and_then(Value::as_str))
        .find(|text| !text.is_empty())
    {
        return text.to_owned();
    }
    fields
        .get(REASONING_DETAILS)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|block| block.get(display_field(block)?))
        .filter_map(Value::as_str)
        .collect()
}

fn text_part<'a>(block: &'a Map<String, Value>, field: &str) -> Result<&'a str, StreamError> {
    match block.get(field) {
        None => Ok(""),
        Some(Value::String(value)) => Ok(value),
        _ => Err(StreamError::invalid(
            "provider emitted a non-text reasoning fragment",
        )),
    }
}

fn merge(
    previous: &mut Map<String, Value>,
    current: &Map<String, Value>,
) -> Result<bool, StreamError> {
    let Some(field) = display_field(current) else {
        return Ok(false);
    };
    if previous.get("type") != current.get("type")
        || ["id", "index", "format"].iter().any(|key| {
            matches!(
                (previous.get(*key), current.get(*key)),
                (Some(before), Some(after)) if !before.is_null() && !after.is_null()
                    && !crate::completion::json_equal(before, after)
            )
        })
    {
        return Ok(false);
    }
    let signatures = (previous.get("signature"), current.get("signature"));
    if let (Some(before), Some(after)) = signatures
        && crate::completion::truthy(before)
        && crate::completion::truthy(after)
        && !crate::completion::json_equal(before, after)
    {
        return Ok(false);
    }
    let text = format!(
        "{}{}",
        text_part(previous, field)?,
        text_part(current, field)?
    );
    let preserved: Vec<_> = ["signature", "format"]
        .into_iter()
        .filter(|key| previous.contains_key(*key) || current.contains_key(*key))
        .map(|key| {
            let value = previous
                .get(key)
                .filter(|value| crate::completion::truthy(value))
                .or_else(|| current.get(key))
                .cloned()
                .unwrap_or(Value::Null);
            (key.to_owned(), value)
        })
        .collect();
    previous.extend(current.clone());
    previous.extend(preserved);
    previous.insert(field.to_owned(), Value::String(text));
    Ok(true)
}

#[derive(Default)]
pub(crate) struct Reasoning {
    scalar_name: Option<&'static str>,
    scalar: String,
    blocks: Option<Vec<Map<String, Value>>>,
}

impl Reasoning {
    pub(crate) fn add(&mut self, delta: &Map<String, Value>) -> Result<(), StreamError> {
        let mut scalar = None;
        for name in SCALAR_REASONING_FIELDS {
            if let Some(value) = delta.get(name).filter(|value| !value.is_null()) {
                let part = value
                    .as_str()
                    .ok_or(StreamError::invalid("provider emitted non-text reasoning"))?;
                if scalar.is_none()
                    || (!part.is_empty()
                        && scalar.is_some_and(|(_, selected): (_, &str)| selected.is_empty()))
                {
                    scalar = Some((name, part));
                }
            }
        }
        let details = delta
            .get(REASONING_DETAILS)
            .filter(|value| !value.is_null())
            .map(|value| {
                let malformed =
                    StreamError::invalid("provider emitted malformed reasoning details");
                let blocks = value.as_array().ok_or(malformed.clone())?;
                if !blocks.iter().all(Value::is_object) {
                    return Err(malformed);
                }
                if !blocks.iter().all(finite_values) {
                    return Err(StreamError::invalid(
                        "provider continuation must contain finite JSON values",
                    ));
                }
                Ok(blocks)
            })
            .transpose()?;
        if let Some((name, part)) = scalar {
            self.scalar_name.get_or_insert(name);
            self.scalar.push_str(part);
        }
        if let Some(details) = details {
            let blocks = self.blocks.get_or_insert_with(Vec::new);
            for block in details.iter().filter_map(Value::as_object) {
                if let Some(index) = block.get("index").filter(|value| !value.is_null())
                    && crate::completion::tool_index(index).is_none()
                {
                    return Err(StreamError::invalid(
                        "provider reasoning block requires a valid index",
                    ));
                }
                if let Some(previous) = blocks.last_mut()
                    && merge(previous, block)?
                {
                    continue;
                }
                blocks.push(block.clone());
            }
        }
        Ok(())
    }

    pub(crate) fn into_fields(self) -> Map<String, Value> {
        let mut fields = Map::new();
        if let Some(alias) = self.scalar_name {
            fields.insert(alias.to_owned(), Value::String(self.scalar));
        }
        if let Some(blocks) = self.blocks {
            let blocks = blocks
                .into_iter()
                .map(|mut block| {
                    block.shift_remove("index");
                    Value::Object(block)
                })
                .collect();
            fields.insert(REASONING_DETAILS.to_owned(), Value::Array(blocks));
        }
        fields
    }
}

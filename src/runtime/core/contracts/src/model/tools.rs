//! Shared schemas for the model's advertised tool catalogue.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tool {
    Function(FunctionTool),
    Custom {
        name: String,
        description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<Value>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolNamespace {
    pub name: String,
    pub description: String,
    pub tools: Vec<Tool>,
}

pub fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn validate_tools(tools: &[ToolNamespace]) -> Result<(), &'static str> {
    let mut namespaces = HashSet::new();
    for namespace in tools {
        if !valid_tool_name(&namespace.name)
            || namespace.tools.is_empty()
            || !namespaces.insert(&namespace.name)
        {
            return Err("tool namespaces must be nonempty and uniquely named");
        }
        let mut names = HashSet::new();
        for tool in &namespace.tools {
            let name = match tool {
                Tool::Function(function) => {
                    if !function.parameters.is_object() {
                        return Err("function parameters must be a JSON schema object");
                    }
                    &function.name
                }
                Tool::Custom { name, .. } => name,
            };
            if !valid_tool_name(name) || !names.insert(name) {
                return Err("tool names must be valid and unique in their namespace");
            }
        }
    }
    Ok(())
}

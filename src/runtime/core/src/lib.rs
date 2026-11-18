//! Portable workflow execution and conversation runtime policies.

pub mod callbacks;
pub mod workflows;

/// Turn execution and the types needed by existing conversation hosts.
pub mod conversation {
    #[cfg(feature = "sqlite")]
    pub use crate::sqlite::SqliteStore;
    pub use syzygy_agent_config::{AgentCatalog, ModelProfile};
    pub use syzygy_conversation::*;
    pub use syzygy_runtime_contracts::model::{
        FunctionTool, Tool, ToolNamespace, valid_tool_name, validate_tools,
    };
}

#[cfg(feature = "responses")]
pub use syzygy_adapter_openai_responses as responses;
#[cfg(feature = "openrouter")]
pub use syzygy_adapter_openrouter as openrouter;
#[cfg(feature = "sqlite")]
pub use syzygy_adapter_sqlite as sqlite;
#[cfg(feature = "workspace-tools")]
pub use syzygy_adapter_workspace as workspace;
pub use syzygy_agent_config as config;
pub use syzygy_provider_projection as projection;
pub use syzygy_runtime_contracts as contracts;
pub use syzygy_runtime_storage as storage;
pub use syzygy_session_inbox as inbox;
pub use syzygy_session_policy as sessions;
pub use syzygy_subagent_tree as subagents;
pub use syzygy_tool_dispatch as tools;
pub use syzygy_worker_execution as execution;

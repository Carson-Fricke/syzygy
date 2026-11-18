//! Neutral model output and exact correlation across a scoped session log.

mod message;
pub mod model;
mod settlement;

pub use message::MessageDirection;
pub use settlement::{AsyncReturns, PendingRequests, ReturnKind, ToolCorrelation};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OutputChannel {
    Reasoning,
    Text,
}

impl OutputChannel {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "reasoning" => Some(Self::Reasoning),
            "text" => Some(Self::Text),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Text => "text",
        }
    }
}

/// A visible append to one invocation channel, separate from canonical messages.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OutputDelta {
    pub channel: OutputChannel,
    pub text: String,
}

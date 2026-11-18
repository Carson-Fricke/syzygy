//! Canonical pre-admission rules. Presentation fields carry no authority.

use std::fmt;

#[derive(Debug)]
pub enum IncomingError {
    WrongDirection,
    OutgoingPresentation,
}

impl fmt::Display for IncomingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WrongDirection => "session delivery requires an incoming message",
            Self::OutgoingPresentation => "incoming presentation must not be assistant output",
        })
    }
}

pub fn validate_incoming(direction: &str, role: &str) -> Result<(), IncomingError> {
    if direction != "incoming" {
        return Err(IncomingError::WrongDirection);
    }
    if !matches!(role, "user" | "tool" | "system") {
        return Err(IncomingError::OutgoingPresentation);
    }
    Ok(())
}

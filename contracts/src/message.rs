//! Session directions and envelope invariants independent of transport.

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MessageDirection {
    Incoming,
    Outgoing,
    Instruction,
}

impl MessageDirection {
    pub fn from_name(direction: &str) -> Result<Self, &'static str> {
        match direction {
            "incoming" => Ok(Self::Incoming),
            "outgoing" => Ok(Self::Outgoing),
            "instruction" => Ok(Self::Instruction),
            _ => Err("unsupported session direction"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::Instruction => "instruction",
        }
    }

    pub fn validate(self, requests: bool, correlation: bool) -> Result<(), &'static str> {
        if requests && self != Self::Outgoing {
            return Err("only outgoing messages may contain requests");
        }
        if correlation && self != Self::Incoming {
            return Err("only incoming messages may contain correlations");
        }
        Ok(())
    }
}

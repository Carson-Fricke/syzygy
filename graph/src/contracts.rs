//! Named workflow ports, routes, and accepted completion references.

use std::{collections::HashSet, fmt};

use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Destination {
    pub node: String,
    pub port: String,
}

impl Destination {
    pub fn parse(value: &str) -> Result<Self, GraphError> {
        let (node, port) = value
            .split_once('.')
            .filter(|(node, port)| {
                !node.trim().is_empty() && !port.trim().is_empty() && !port.contains('.')
            })
            .ok_or_else(|| GraphError::new("a route destination must be 'node.port'"))?;
        Ok(Self {
            node: node.to_owned(),
            port: port.to_owned(),
        })
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed,
}

impl Outcome {
    pub fn from_name(name: &str) -> Result<Self, GraphError> {
        match name {
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(GraphError::new("unknown terminal outcome")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Node {
    pub name: String,
    pub inputs: Vec<String>,
    pub routes: IndexMap<String, Vec<Destination>>,
    pub outcome: Option<Outcome>,
}

impl Node {
    pub fn validate(&self) -> Result<(), GraphError> {
        let inputs: HashSet<_> = self.inputs.iter().collect();
        if self.name.is_empty() || inputs.is_empty() || inputs.len() != self.inputs.len() {
            return Err(GraphError::new(
                "a node requires a name and distinct named input ports",
            ));
        }
        if inputs
            .iter()
            .any(|port| port.trim().is_empty() || port.contains('.'))
        {
            return Err(GraphError::new(
                "input ports must be nonempty names without dots",
            ));
        }
        if self.routes.is_empty() == self.outcome.is_none() {
            return Err(GraphError::new("only terminal nodes have no routes"));
        }
        for (route, destinations) in &self.routes {
            if route.trim().is_empty() || destinations.is_empty() {
                return Err(GraphError::new(
                    "each route requires a name and destinations",
                ));
            }
            if destinations.iter().collect::<HashSet<_>>().len() != destinations.len() {
                return Err(GraphError::new(
                    "a route cannot deliver twice to the same input",
                ));
            }
        }
        Ok(())
    }
}

pub type Topology = IndexMap<String, Node>;

#[derive(Debug, Deserialize)]
pub struct AcceptedCompletion {
    pub sequence: u64,
    pub from_phase: String,
    pub route: String,
    pub attempt_id: String,
}

#[derive(Debug)]
pub struct GraphError(String);

impl GraphError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GraphError {}

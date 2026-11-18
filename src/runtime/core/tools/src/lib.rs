//! A session tool catalog and a single authorized validation-to-execution path.
//!
//! The tool host supplies the schema interpreter and operation callbacks.
//! Catalog registration and caller policy constrain which handler may run.

use std::collections::BTreeSet;

use indexmap::IndexMap;

mod json_recovery;
mod reading;

pub use json_recovery::{balanced_objects, first_document};
pub use reading::{DEFAULT_READ_TOKENS, MAX_READ_TOKENS, TOKENIZER, count_tokens, text_page};

pub struct Catalog<T> {
    entries: IndexMap<String, T>,
    policy: Option<BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessDenied {
    UnknownTool,
    Denied,
}

impl<T> Catalog<T> {
    pub fn new(policy: Option<BTreeSet<String>>) -> Self {
        Self {
            entries: IndexMap::new(),
            policy,
        }
    }

    pub fn policy(&self) -> Option<&BTreeSet<String>> {
        self.policy.as_ref()
    }

    pub fn insert(&mut self, name: String, tool: T) -> bool {
        match self.entries.entry(name) {
            indexmap::map::Entry::Vacant(entry) => {
                entry.insert(tool);
                true
            }
            indexmap::map::Entry::Occupied(_) => false,
        }
    }

    pub fn get(&self, name: &str) -> Option<&T> {
        self.entries.get(name)
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn permits(&self, name: &str) -> bool {
        self.policy
            .as_ref()
            .is_none_or(|policy| policy.contains(name))
    }

    pub fn advertised(&self) -> impl Iterator<Item = &T> {
        self.entries
            .iter()
            .filter_map(|(name, tool)| self.permits(name).then_some(tool))
    }

    pub fn authorized<'a>(
        &'a self,
        name: &str,
        allows: impl FnOnce() -> bool,
    ) -> Result<&'a T, AccessDenied> {
        let tool = self.get(name).ok_or(AccessDenied::UnknownTool)?;
        if self.permits(name) && allows() {
            Ok(tool)
        } else {
            Err(AccessDenied::Denied)
        }
    }
}

pub enum Invocation<T, D, E> {
    Finished(T),
    Rejected(D),
    Failed { error: E, started: bool },
}

/// The operation cannot run until the injected schema validator accepts.
pub fn invoke<T, D, E>(
    validate: impl FnOnce() -> Result<Option<D>, E>,
    handle: impl FnOnce() -> Result<T, E>,
) -> Invocation<T, D, E> {
    match validate() {
        Err(error) => Invocation::Failed {
            error,
            started: false,
        },
        Ok(Some(diagnostics)) => Invocation::Rejected(diagnostics),
        Ok(None) => match handle() {
            Ok(outcome) => Invocation::Finished(outcome),
            Err(error) => Invocation::Failed {
                error,
                started: true,
            },
        },
    }
}

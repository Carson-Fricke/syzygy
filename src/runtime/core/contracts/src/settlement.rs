//! Derive pending tool requests and asynchronous returns from neutral identities.

use std::collections::HashSet;

use indexmap::IndexSet;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct ToolCorrelation {
    pub request_message_id: String,
    pub request_id: String,
    pub tool_name: String,
}

impl ToolCorrelation {
    pub fn key(&self) -> (&str, &str, &str) {
        (&self.request_message_id, &self.request_id, &self.tool_name)
    }
}

#[derive(Default)]
pub struct PendingRequests(IndexSet<ToolCorrelation>);

impl PendingRequests {
    pub fn requested(&mut self, request: ToolCorrelation) {
        self.0.insert(request);
    }

    pub fn delivered(&mut self, result: &ToolCorrelation) {
        self.0.shift_remove(result);
    }

    pub fn outstanding(self) -> impl ExactSizeIterator<Item = ToolCorrelation> {
        self.0.into_iter()
    }
}

#[derive(Clone, Copy)]
pub enum ReturnKind {
    Job,
    Child,
    Workflow,
}

#[derive(Default)]
struct Progress {
    started: HashSet<String>,
    finished: HashSet<String>,
}

#[derive(Default)]
pub struct AsyncReturns {
    jobs: Progress,
    children: Progress,
    workflows: Progress,
}

impl AsyncReturns {
    fn progress(&mut self, kind: ReturnKind) -> &mut Progress {
        match kind {
            ReturnKind::Job => &mut self.jobs,
            ReturnKind::Child => &mut self.children,
            ReturnKind::Workflow => &mut self.workflows,
        }
    }

    pub fn started(&mut self, kind: ReturnKind, id: String) {
        self.progress(kind).started.insert(id);
    }

    pub fn finished(&mut self, kind: ReturnKind, id: String) {
        self.progress(kind).finished.insert(id);
    }

    /// The full scoped log can contain a result before its retained receipt.
    pub fn has_outstanding(&self) -> bool {
        [&self.jobs, &self.children, &self.workflows]
            .iter()
            .any(|progress| !progress.started.is_subset(&progress.finished))
    }
}

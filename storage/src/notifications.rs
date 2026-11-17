//! Process-local commit versions. Durable storage sequences own read cursors.

use std::{
    collections::HashMap,
    sync::{Condvar, Mutex, PoisonError},
    time::Duration,
};

pub type ConversationKey = (String, String);

#[derive(Default)]
pub struct CommitVersions {
    versions: Mutex<HashMap<ConversationKey, u64>>,
    changed: Condvar,
}

impl CommitVersions {
    pub fn version(&self, key: &ConversationKey) -> u64 {
        let versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        versions.get(key).copied().unwrap_or_default()
    }

    pub fn notify(&self, key: ConversationKey) {
        let mut versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        *versions.entry(key).or_default() += 1;
        self.changed.notify_all();
    }

    pub fn wait(&self, key: &ConversationKey, version: u64, timeout: Duration) -> u64 {
        let versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        let unchanged = |versions: &mut HashMap<_, _>| {
            versions.get(key).copied().unwrap_or_default() == version
        };
        let (versions, _) = self
            .changed
            .wait_timeout_while(versions, timeout, unchanged)
            .unwrap_or_else(PoisonError::into_inner);
        versions.get(key).copied().unwrap_or_default()
    }
}

//! Sequenced append and exact-identity reconciliation for a catalog CAS port.

const MAX_APPEND_ATTEMPTS: usize = 32;

/// Stable facts supplied by the producer of a timeline entry. Backend-assigned
/// timestamps, revisions and sequence numbers do not identify a retry.
#[derive(Debug, PartialEq, Eq)]
pub struct EventIntent<S, P> {
    pub workspace: S,
    pub conversation: S,
    pub thread: Option<S>,
    pub execution: Option<S>,
    pub id: S,
    pub kind: S,
    pub payload: P,
}

#[derive(Debug, Eq, PartialEq)]
pub struct EventConflict;

impl std::fmt::Display for EventConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("timeline identity was reused for different content or scope")
    }
}

impl std::error::Error for EventConflict {}

/// Join a retry only to the exact event previously accepted under its identity.
pub fn reconcile_event<S: PartialEq, P: PartialEq>(
    proposed: &EventIntent<S, P>,
    retained: &EventIntent<S, P>,
    retained_sequence: u64,
) -> Result<u64, EventConflict> {
    if proposed == retained {
        Ok(retained_sequence)
    } else {
        Err(EventConflict)
    }
}

#[derive(Debug)]
pub enum AppendFailure<E> {
    Conflict(E),
    Unknown(E),
    Rejected(E),
}

/// A reconciliation read may return an accepted document or, after a confirmed
/// conflict, allow another transaction. Unknown outcomes never replay a write.
fn after_failure<D, E>(
    failure: AppendFailure<E>,
    can_retry: bool,
    reconcile: impl FnOnce() -> Result<Option<D>, E>,
) -> Result<Option<D>, E> {
    let (conflict, error) = match failure {
        AppendFailure::Rejected(error) => return Err(error),
        AppendFailure::Conflict(error) => (true, error),
        AppendFailure::Unknown(error) => (false, error),
    };
    match reconcile()? {
        Some(document) => Ok(Some(document)),
        None if conflict && can_retry => Ok(None),
        None => Err(error),
    }
}

fn retry_append<D, E>(mut attempt: impl FnMut(bool) -> Result<Option<D>, E>) -> Result<D, E> {
    for remaining in (0..MAX_APPEND_ATTEMPTS).rev() {
        if let Some(document) = attempt(remaining > 0)? {
            return Ok(document);
        }
    }
    unreachable!("an exhausted conflict is returned by after_failure")
}

/// Append on a backend that allocates the sequence and publishes dependent
/// records inside one transaction. Reconciliation runs outside that transaction.
pub fn append_transaction<D, E>(
    mut commit: impl FnMut() -> Result<D, AppendFailure<E>>,
    mut reconcile: impl FnMut() -> Result<Option<D>, E>,
) -> Result<D, E> {
    retry_append(|can_retry| match commit() {
        Ok(document) => Ok(Some(document)),
        Err(failure) => after_failure(failure, can_retry, &mut reconcile),
    })
}

/// A preparation binds one proposed item to its counter and dependent writes.
/// Its commit either publishes that whole prepared attempt or reports an error.
pub trait TimelineAppend {
    type Attempt;
    type Document;
    type Error;

    fn prepare(&mut self) -> Result<Self::Attempt, Self::Error>;
    fn commit(&mut self, attempt: &Self::Attempt) -> Result<(), AppendFailure<Self::Error>>;
    fn reconcile(&mut self, attempt: &Self::Attempt)
    -> Result<Option<Self::Document>, Self::Error>;
    fn proposed(&self, attempt: Self::Attempt) -> Self::Document;
}

pub fn append<P: TimelineAppend>(port: &mut P) -> Result<P::Document, P::Error> {
    retry_append(|can_retry| {
        let attempt = port.prepare()?;
        match port.commit(&attempt) {
            Ok(()) => Ok(Some(port.proposed(attempt))),
            Err(failure) => after_failure(failure, can_retry, || port.reconcile(&attempt)),
        }
    })
}

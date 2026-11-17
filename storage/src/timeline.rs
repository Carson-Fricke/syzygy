//! Sequenced append and exact-identity reconciliation for a catalog CAS port.

const MAX_APPEND_ATTEMPTS: usize = 32;

#[derive(Debug)]
pub enum AppendFailure<E> {
    Conflict(E),
    Unknown(E),
    Rejected(E),
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
    let mut remaining = MAX_APPEND_ATTEMPTS;
    loop {
        remaining -= 1;
        let attempt = port.prepare()?;
        let value = match port.commit(&attempt) {
            Ok(()) => port.proposed(attempt),
            Err(AppendFailure::Rejected(error)) => return Err(error),
            Err(failure) => {
                let (retry, error) = match failure {
                    AppendFailure::Conflict(error) => (true, error),
                    AppendFailure::Unknown(error) => (false, error),
                    AppendFailure::Rejected(error) => return Err(error),
                };
                if let Some(existing) = port.reconcile(&attempt)? {
                    existing
                } else if retry && remaining > 0 {
                    continue;
                } else {
                    return Err(error);
                }
            }
        };
        return Ok(value);
    }
}

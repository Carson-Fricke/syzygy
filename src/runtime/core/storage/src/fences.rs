//! Deterministic document-lock order and contention handling for fenced writes.

const MAX_FENCE_ATTEMPTS: usize = 32;

pub fn commit_fenced<T, R, E>(
    writes: &[T],
    mut fenced: impl FnMut() -> Result<Vec<T>, E>,
    mut identity: impl FnMut(&T) -> Result<String, E>,
    mut publish: impl FnMut(Vec<&T>) -> Result<R, E>,
    conflict_index: impl Fn(&E) -> Result<Option<usize>, E>,
    exhausted: impl FnOnce() -> E,
) -> Result<R, E> {
    for _ in 0..MAX_FENCE_ATTEMPTS {
        let fences = fenced()?;
        let mut ordered: Vec<_> = fences
            .iter()
            .map(|operation| (operation, true))
            .chain(writes.iter().map(|operation| (operation, false)))
            .map(|(operation, is_fence)| Ok((identity(operation)?, is_fence, operation)))
            .collect::<Result<_, E>>()?;
        ordered.sort_by(|left, right| left.0.cmp(&right.0));
        match publish(ordered.iter().map(|(_, _, operation)| *operation).collect()) {
            Ok(result) => return Ok(result),
            Err(error) => {
                // Caller-owned record CAS failures require a fresh domain read.
                let fence_conflict = !fences.is_empty()
                    && conflict_index(&error)?
                        .and_then(|index| ordered.get(index))
                        .is_some_and(|(_, is_fence, _)| *is_fence);
                if !fence_conflict {
                    return Err(error);
                }
            }
        }
    }
    Err(exhausted())
}

//! Rebuild accepted graph candidates after revision conflicts. External
//! phase-entry effects occur only after this function returns a commit.

const MAX_CONTENTION_ATTEMPTS: usize = 8;

pub fn accept_transition<T, E>(
    mut prepare: impl FnMut() -> Result<T, E>,
    mut commit: impl FnMut(&T) -> Result<bool, E>,
) -> Result<Option<T>, E> {
    for _ in 0..MAX_CONTENTION_ATTEMPTS {
        let candidate = prepare()?;
        if commit(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

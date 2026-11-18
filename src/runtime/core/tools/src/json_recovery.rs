//! Recover a JSON candidate from prose, markdown or nested object slices.

use std::sync::LazyLock;

use regex::Regex;

static FENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)```(?:json)?[\s\x1c-\x1f]*(.*?)```").expect("valid JSON fence pattern")
});

/// Yield balanced object spans in source order, including nested candidates.
pub fn balanced_objects(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices('{').filter_map(|(start, _)| {
        let candidate = &text[start..];
        let mut depth = 0;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in candidate.bytes().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&candidate[..=offset]);
                    }
                }
                _ => {}
            }
        }
        None
    })
}

/// Parse the whole reply first, then each fence and finally each object.
/// A valid non-object document still ends recovery.
pub fn first_document<T, E>(
    text: &str,
    mut parse: impl FnMut(&str) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    let fences = FENCE
        .captures_iter(text)
        .map(|capture| capture.extract::<1>().1[0]);
    for candidate in std::iter::once(text)
        .chain(fences)
        .chain(balanced_objects(text))
    {
        if let Some(document) = parse(candidate)? {
            return Ok(Some(document));
        }
    }
    Ok(None)
}

//! Exact UTF-8 continuation on the original cl100k_base token stream.

use std::fmt;

pub const TOKENIZER: &str = "cl100k_base";
pub const DEFAULT_READ_TOKENS: usize = 16_384;
pub const MAX_READ_TOKENS: usize = 65_536;

pub fn encode(text: &str) -> Vec<u32> {
    tiktoken_rs::cl100k_base_singleton().encode_ordinary(text)
}

pub fn count_tokens(text: &str) -> usize {
    encode(text).len()
}

pub enum PageError {
    Budget,
    Offset(usize),
    Split,
    Character,
}

impl fmt::Display for PageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget => write!(
                f,
                "max_tokens must be an integer from 32 to {MAX_READ_TOKENS}"
            ),
            Self::Offset(total) => write!(
                f,
                "token_offset must be between 0 and {total}; use nextArguments"
            ),
            Self::Split => f.write_str("token_offset splits a UTF-8 character; use nextArguments"),
            Self::Character => {
                f.write_str("max_tokens cannot fit the next character; increase the budget")
            }
        }
    }
}

pub struct TextPage {
    pub content: String,
    pub offset: usize,
    pub next: Option<usize>,
    pub total: usize,
    pub cost: usize,
}

fn quoted_cost(text: &str) -> usize {
    let quoted = serde_json::to_string(text).expect("a string is serializable");
    count_tokens(&quoted)
}

pub fn text_page(
    text: &str,
    offset: Option<usize>,
    budget: Option<usize>,
) -> Result<TextPage, PageError> {
    let budget = budget
        .filter(|budget| (32..=MAX_READ_TOKENS).contains(budget))
        .ok_or(PageError::Budget)?;
    let codec = tiktoken_rs::cl100k_base_singleton();
    let tokens = codec.encode_ordinary(text);
    let total = tokens.len();
    let offset = offset
        .filter(|offset| *offset <= total)
        .ok_or(PageError::Offset(total))?;
    if codec.decode(&tokens[..offset]).is_err() {
        return Err(PageError::Split);
    }
    let mut end = total;
    let mut content = codec
        .decode(&tokens[offset..])
        .map_err(|_| PageError::Split)?;
    let mut cost = quoted_cost(&content);
    if cost > budget {
        end = end.min(offset + budget * 3 / 4);
        loop {
            if end <= offset {
                return Err(PageError::Character);
            }
            match codec.decode(&tokens[offset..end]) {
                Ok(value) => {
                    cost = quoted_cost(&value);
                    if cost <= budget {
                        content = value;
                        break;
                    }
                    end -= ((end - offset) / 2)
                        .max(1)
                        .min(((cost - budget) / 2).max(1));
                }
                Err(_) => end -= 1,
            }
        }
    }
    Ok(TextPage {
        content,
        offset,
        next: (end < total).then_some(end),
        total,
        cost,
    })
}

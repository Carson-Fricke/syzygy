//! Incrementally decode provider SSE without dropping a complete event when
//! a later line fails. This decoder owns framing and size limits.

use std::fmt;

const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    InvalidUtf8,
    EventTooLarge,
    LineTooLarge,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUtf8 => "provider SSE contained invalid UTF-8",
            Self::EventTooLarge => "provider SSE frame exceeded the size limit",
            Self::LineTooLarge => "provider SSE line exceeded the size limit",
        })
    }
}

#[derive(Default)]
pub struct SseDecoder {
    utf8_tail: Vec<u8>,
    text: String,
    consumed: usize,
    data: Vec<String>,
    data_bytes: usize,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        self.text.drain(..self.consumed);
        self.consumed = 0;

        self.utf8_tail.extend_from_slice(bytes);
        match std::str::from_utf8(&self.utf8_tail) {
            Ok(text) => {
                self.text.push_str(text);
                self.utf8_tail.clear();
            }
            Err(error) if error.error_len().is_none() => {
                let remaining = self.utf8_tail.split_off(error.valid_up_to());
                let prefix =
                    std::str::from_utf8(&self.utf8_tail).map_err(|_| FrameError::InvalidUtf8)?;
                self.text.push_str(prefix);
                self.utf8_tail = remaining;
            }
            Err(_) => return Err(FrameError::InvalidUtf8),
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<(), FrameError> {
        if self.utf8_tail.is_empty() {
            Ok(())
        } else {
            Err(FrameError::InvalidUtf8)
        }
    }

    pub fn next_event(&mut self, final_chunk: bool) -> Result<Option<String>, FrameError> {
        let remaining = &self.text[self.consumed..];
        let mut next = remaining;
        while let Some(end) = next.find(['\r', '\n']) {
            let line = &next[..end];
            let delimiter = &next[end..];
            let width = if delimiter.starts_with("\r\n") {
                2
            } else if delimiter == "\r" && !final_chunk {
                break;
            } else {
                1
            };
            self.consumed += end + width;
            next = &delimiter[width..];
            if line.is_empty() && !self.data.is_empty() {
                self.data_bytes = 0;
                return Ok(Some(std::mem::take(&mut self.data).join("\n")));
            }
            if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                self.data_bytes += value.len();
                if self.data_bytes > LIMIT {
                    return Err(FrameError::EventTooLarge);
                }
                self.data.push(value.to_owned());
            }
        }
        if next.len() > LIMIT && next.chars().count() > LIMIT {
            return Err(FrameError::LineTooLarge);
        }
        Ok(None)
    }

    pub fn is_complete(&self) -> bool {
        self.consumed == self.text.len() && self.data.is_empty()
    }
}

//! Provisional output for one invocation. The caller commits a batch to the
//! timeline before advancing its byte offset and releasing its fragments.

use std::{collections::VecDeque, sync::Arc};

use syzygy_runtime_contracts::OutputChannel;

fn index(channel: OutputChannel) -> usize {
    match channel {
        OutputChannel::Reasoning => 0,
        OutputChannel::Text => 1,
    }
}

struct Fragment {
    channel: OutputChannel,
    text: String,
}

pub struct Batch {
    pub channel: OutputChannel,
    pub offset: usize,
    pub text: String,
    fragments: usize,
    buffer: Arc<()>,
}

#[derive(Default)]
pub struct OutputBuffer {
    identity: Arc<()>,
    partial: [String; 2],
    offsets: [usize; 2],
    pending: VecDeque<Fragment>,
    pending_chars: usize,
    has_flushed: bool,
}

impl OutputBuffer {
    pub fn record_partial(&mut self, channel: OutputChannel, text: &str) {
        self.partial[index(channel)].push_str(text);
    }

    pub fn partial(&self, channel: OutputChannel) -> &str {
        &self.partial[index(channel)]
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn enqueue(&mut self, channel: OutputChannel, text: String) {
        if text.is_empty() {
            return;
        }
        self.pending_chars += text.chars().count();
        self.pending.push_back(Fragment { channel, text });
    }

    pub fn should_flush(&self) -> bool {
        !self.has_flushed || self.pending_chars >= 4096
    }

    pub fn next_batch(&self) -> Option<Batch> {
        let channel = self.pending.front()?.channel;
        let mut text = String::new();
        let mut fragments = 0;
        for fragment in self.pending.iter().take_while(|f| f.channel == channel) {
            text.push_str(&fragment.text);
            fragments += 1;
        }
        Some(Batch {
            channel,
            offset: self.offsets[index(channel)],
            text,
            fragments,
            buffer: Arc::clone(&self.identity),
        })
    }

    /// Acknowledge exactly the persisted prefix. An obsolete or modified batch
    /// leaves both the queue and the byte offset untouched.
    pub fn commit_batch(&mut self, batch: &Batch) -> bool {
        let channel = index(batch.channel);
        if !Arc::ptr_eq(&self.identity, &batch.buffer)
            || batch.offset != self.offsets[channel]
            || self.pending.len() < batch.fragments
        {
            return false;
        }
        let mut remaining = batch.text.as_str();
        for fragment in self.pending.iter().take(batch.fragments) {
            let Some(suffix) = remaining.strip_prefix(&fragment.text) else {
                return false;
            };
            if fragment.channel != batch.channel {
                return false;
            }
            remaining = suffix;
        }
        if !remaining.is_empty() {
            return false;
        }
        self.pending.drain(..batch.fragments);
        self.offsets[channel] += batch.text.len();
        self.pending_chars -= batch.text.chars().count();
        true
    }

    pub fn finish_flush(&mut self) {
        self.has_flushed = true;
    }
}

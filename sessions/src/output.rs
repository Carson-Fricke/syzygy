//! Provisional output for one invocation. The caller commits a batch to the
//! timeline before advancing its byte offset and releasing its fragments.

use std::collections::VecDeque;

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
}

#[derive(Default)]
pub struct OutputBuffer {
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
        self.pending_chars += text.chars().count();
        self.pending.push_back(Fragment { channel, text });
    }

    pub fn should_flush(&self) -> bool {
        !self.has_flushed || self.pending_chars >= 4096
    }

    pub fn next_batch(&self) -> Option<Batch> {
        let channel = self.pending.front()?.channel;
        let text = self
            .pending
            .iter()
            .take_while(|fragment| fragment.channel == channel)
            .map(|fragment| fragment.text.as_str())
            .collect();
        Some(Batch {
            channel,
            offset: self.offsets[index(channel)],
            text,
        })
    }

    /// The caller holds its publishing lock from reading the batch to this ACK.
    pub fn commit_batch(&mut self) {
        if let Some(channel) = self.pending.front().map(|fragment| fragment.channel) {
            while self
                .pending
                .front()
                .is_some_and(|fragment| fragment.channel == channel)
            {
                if let Some(fragment) = self.pending.pop_front() {
                    self.offsets[index(channel)] += fragment.text.len();
                    self.pending_chars -= fragment.text.chars().count();
                }
            }
        }
    }

    pub fn finish_flush(&mut self) {
        self.has_flushed = true;
    }
}

use std::collections::VecDeque;

use super::APPROX_BYTES_PER_TOKEN;

/// Agent-visible output drained from one atomic buffer snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalOutputSlice {
    /// Formatted output after omission and token-budget handling.
    pub(crate) output: String,
    /// Historical bytes evicted before the Agent could consume them.
    pub(crate) omitted_bytes: u64,
}

/// Bounded raw terminal output with one non-repeating Agent cursor.
#[derive(Debug, typed_builder::TypedBuilder)]
pub(crate) struct TerminalOutputBuffer {
    capacity: usize,
    bytes: VecDeque<u8>,
    start_offset: u64,
    end_offset: u64,
    agent_cursor: u64,
}

impl TerminalOutputBuffer {
    /// Creates an empty buffer with a positive raw-byte capacity.
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self::builder()
            .capacity(capacity)
            .bytes(VecDeque::with_capacity(capacity))
            .start_offset(0)
            .end_offset(0)
            .agent_cursor(0)
            .build()
    }

    /// Appends raw process bytes and evicts the oldest excess bytes.
    pub(crate) fn append(&mut self, data: &[u8]) {
        self.bytes.extend(data.iter().copied());
        self.end_offset = self
            .end_offset
            .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        let excess = self.bytes.len().saturating_sub(self.capacity);
        self.bytes.drain(..excess);
        self.start_offset = self
            .start_offset
            .saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
    }

    /// Drains one atomic unread snapshot and advances the Agent cursor once.
    pub(crate) fn drain_agent(
        &mut self,
        max_output_tokens: usize,
    ) -> TerminalOutputSlice {
        let snapshot = self.snapshot_from(self.agent_cursor, max_output_tokens);
        self.agent_cursor = self.end_offset;
        snapshot
    }

    /// Returns the current unread Agent output without advancing its cursor.
    pub(crate) fn snapshot_agent(
        &self,
        max_output_tokens: usize,
    ) -> TerminalOutputSlice {
        self.snapshot_from(self.agent_cursor, max_output_tokens)
    }

    /// Formats retained bytes visible after one absolute stream cursor.
    fn snapshot_from(
        &self,
        cursor: u64,
        max_output_tokens: usize,
    ) -> TerminalOutputSlice {
        let omitted_bytes = self.start_offset.saturating_sub(cursor);
        let readable_cursor = cursor.max(self.start_offset);
        let skip =
            usize::try_from(readable_cursor.saturating_sub(self.start_offset))
                .unwrap_or(usize::MAX);
        let unread = self.bytes.iter().skip(skip).copied().collect::<Vec<_>>();

        let decoded = String::from_utf8_lossy(&unread).into_owned();
        let mut output = Self::truncate_tail(&decoded, max_output_tokens);
        if omitted_bytes > 0 {
            output = format!(
                "…{omitted_bytes} bytes omitted from terminal buffer…\n{output}"
            );
        }
        TerminalOutputSlice {
            output,
            omitted_bytes,
        }
    }

    /// Applies an approximate token budget while preserving a UTF-8-safe tail.
    fn truncate_tail(output: &str, max_output_tokens: usize) -> String {
        let byte_budget =
            max_output_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
        if output.len() <= byte_budget {
            return output.to_string();
        }
        let mut start = output.len().saturating_sub(byte_budget);
        while start < output.len() && !output.is_char_boundary(start) {
            start = start.saturating_add(1);
        }
        let tail = output.get(start..).unwrap_or_default();
        let original_tokens = Self::approx_token_count(output.len());
        let retained_tokens = Self::approx_token_count(tail.len());
        let omitted_tokens = original_tokens.saturating_sub(retained_tokens);
        format!(
            "Warning: truncated output (original token count: {original_tokens})\n…{omitted_tokens} tokens truncated…\n{tail}"
        )
    }

    /// Converts a byte count into Codex-compatible approximate tokens.
    fn approx_token_count(bytes: usize) -> usize {
        bytes.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1))
            / APPROX_BYTES_PER_TOKEN
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalOutputBuffer;

    /// Draining output advances one non-repeating Agent cursor.
    #[test]
    fn drain_advances_agent_cursor_without_repeating_output() {
        let mut buffer = TerminalOutputBuffer::new(32);
        let (): () = buffer.append(b"first");
        assert_eq!(buffer.drain_agent(10_000).output, "first");
        assert_eq!(buffer.drain_agent(10_000).output, "");
        buffer.append(b"second");
        assert_eq!(buffer.drain_agent(10_000).output, "second");
    }

    /// Ring eviction reports the exact number of unavailable historical bytes.
    #[test]
    fn eviction_reports_exact_omitted_byte_count() {
        let mut buffer = TerminalOutputBuffer::new(8);
        buffer.append(b"abcdefghijkl");
        let slice = buffer.drain_agent(10_000);
        assert_eq!(slice.omitted_bytes, 4);
        assert!(slice.output.contains("4 bytes omitted"));
        assert!(slice.output.ends_with("efghijkl"));
    }

    /// Response truncation keeps a UTF-8-safe tail and never replays omitted bytes.
    #[test]
    fn response_budget_keeps_utf8_safe_tail_and_consumes_snapshot_once() {
        let mut buffer = TerminalOutputBuffer::new(128);
        buffer.append("前缀-abcdefghijklmnopqrstuvwxyz-结尾".as_bytes());
        let slice = buffer.drain_agent(4);
        assert!(slice.output.contains("tokens truncated"));
        assert!(slice.output.ends_with("结尾"));
        assert_eq!(buffer.drain_agent(4).output, "");
    }
}

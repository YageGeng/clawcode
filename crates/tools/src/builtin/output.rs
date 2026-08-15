use std::io::Write as _;
use std::path::PathBuf;

use protocol::{ProductIdentity, TruncationDetails};
use tokio::io::AsyncWriteExt as _;

use crate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};

/// Latest visible tail and optional durable complete-output location.
#[derive(Debug)]
pub(crate) struct OutputSnapshot {
    pub(crate) content: String,
    pub(crate) truncation: TruncationDetails,
    pub(crate) full_output_path: Option<PathBuf>,
}

/// Bounded streaming output state that spills complete bytes only after truncation.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    #[builder(default)]
    rolling: Vec<u8>,
    #[builder(default)]
    rolling_starts_at_line_boundary: bool,
    #[builder(default)]
    pending_chunks: Vec<Vec<u8>>,
    #[builder(default)]
    total_bytes: usize,
    #[builder(default)]
    completed_lines: usize,
    #[builder(default)]
    current_line_bytes: usize,
    #[builder(default)]
    has_open_line: bool,
    #[builder(default)]
    full_output_path: Option<PathBuf>,
    #[builder(default)]
    full_output_file: Option<tokio::fs::File>,
}

impl OutputAccumulator {
    /// Creates an accumulator with pi's 2000-line and 50KB visible limits.
    pub(crate) fn new() -> Self {
        Self::builder()
            .max_lines(DEFAULT_MAX_LINES)
            .max_bytes(DEFAULT_MAX_BYTES)
            .max_rolling_bytes(DEFAULT_MAX_BYTES * 2)
            .rolling_starts_at_line_boundary(true)
            .build()
    }

    /// Appends one raw stdout/stderr chunk and starts durable spooling if needed.
    pub(crate) async fn append(
        &mut self,
        data: Vec<u8>,
    ) -> std::io::Result<()> {
        self.total_bytes = self.total_bytes.saturating_add(data.len());
        if let Some(last_newline) = data.iter().rposition(|byte| *byte == b'\n')
        {
            self.completed_lines = self.completed_lines.saturating_add(
                data.iter().filter(|byte| **byte == b'\n').count(),
            );
            self.current_line_bytes =
                data.len().saturating_sub(last_newline + 1);
            self.has_open_line = self.current_line_bytes > 0;
        } else if !data.is_empty() {
            self.current_line_bytes =
                self.current_line_bytes.saturating_add(data.len());
            self.has_open_line = true;
        }
        self.rolling.extend_from_slice(&data);
        if self.rolling.len() > self.max_rolling_bytes * 2 {
            let start =
                self.rolling.len().saturating_sub(self.max_rolling_bytes);
            self.rolling_starts_at_line_boundary = start == 0
                || self.rolling.get(start.saturating_sub(1)) == Some(&b'\n');
            self.rolling.drain(..start);
        }

        if let Some(file) = &mut self.full_output_file {
            file.write_all(&data).await?;
        } else {
            self.pending_chunks.push(data);
            if self.is_truncated() {
                self.ensure_full_output_file().await?;
            }
        }
        Ok(())
    }

    /// Returns the latest visible tail without finalizing the durable spool.
    pub(crate) fn snapshot(&self) -> OutputSnapshot {
        let decoded = String::from_utf8_lossy(&self.rolling);
        let visible = if self.rolling_starts_at_line_boundary {
            decoded.as_ref()
        } else {
            decoded
                .find('\n')
                .and_then(|index| decoded.get(index + 1..))
                .unwrap_or(decoded.as_ref())
        };
        let mut truncation =
            truncate_tail(visible, self.max_lines, self.max_bytes);
        truncation.truncated = self.is_truncated();
        truncation.truncated_by = if truncation.truncated {
            truncation.truncated_by.or_else(|| {
                (self.total_bytes > self.max_bytes)
                    .then_some(protocol::TruncationLimit::Bytes)
                    .or(Some(protocol::TruncationLimit::Lines))
            })
        } else {
            None
        };
        truncation.total_lines = self.total_lines();
        truncation.total_bytes = self.total_bytes;
        truncation.max_lines = self.max_lines;
        truncation.max_bytes = self.max_bytes;
        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.full_output_path.clone(),
        }
    }

    /// Flushes and closes complete output before the final ToolResult references it.
    pub(crate) async fn finish(&mut self) -> std::io::Result<OutputSnapshot> {
        if self.is_truncated() {
            self.ensure_full_output_file().await?;
        }
        if let Some(mut file) = self.full_output_file.take() {
            file.flush().await?;
            file.shutdown().await?;
        }
        Ok(self.snapshot())
    }

    /// Returns the exact byte length of the current final source line.
    pub(crate) const fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    /// Counts complete lines plus one currently open line using pi's rule.
    fn total_lines(&self) -> usize {
        self.completed_lines + usize::from(self.has_open_line)
    }

    /// Reports whether either independent visible-output limit has been crossed.
    fn is_truncated(&self) -> bool {
        self.total_bytes > self.max_bytes || self.total_lines() > self.max_lines
    }

    /// Creates one retained temporary file and flushes every pre-threshold chunk.
    async fn ensure_full_output_file(&mut self) -> std::io::Result<()> {
        if self.full_output_file.is_some() {
            return Ok(());
        }
        let mut temporary = tempfile::Builder::new()
            .prefix(ProductIdentity::BASH_OUTPUT_TEMP_PREFIX)
            .suffix(".log")
            .tempfile()?;
        for chunk in &self.pending_chunks {
            temporary.write_all(chunk)?;
        }
        temporary.flush()?;
        let (file, path) = temporary.keep().map_err(|error| error.error)?;
        self.pending_chunks.clear();
        self.full_output_path = Some(path);
        self.full_output_file = Some(tokio::fs::File::from_std(file));
        Ok(())
    }
}

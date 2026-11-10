//! The text and details one `exec_command` or `write_stdin` call returns.

use std::path::PathBuf;
use std::time::Duration;

use protocol::{
    EXEC_FULL_OUTPUT_LABEL, EXEC_OUTPUT_HEADER, ToolCallId, ToolResultDetails,
};

use super::{APPROX_BYTES_PER_TOKEN, Captured};

/// Result of one call, formatted for the model as codex formats it.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct ExecResponse {
    /// Random identifier repeated in the header.
    pub chunk_id: String,
    /// Time the call spent waiting.
    pub wall_time: Duration,
    /// Session ID when the process is still running.
    #[builder(default)]
    pub process_id: Option<i32>,
    /// Exit code when the process ended during the call.
    #[builder(default)]
    pub exit_code: Option<i32>,
    /// Signal name when a signal ended the process during the call.
    #[builder(default)]
    pub signal: Option<String>,
    /// Output collected by this call.
    pub output: Captured,
    /// Token budget for the output section.
    pub max_output_tokens: usize,
    /// `exec_command` call that started the process.
    pub terminal_call_id: ToolCallId,
    /// Retained file holding the complete collected output after truncation.
    #[builder(default)]
    pub full_output_path: Option<PathBuf>,
}

impl ExecResponse {
    /// Estimated tokens of everything collected, including omitted bytes.
    #[must_use]
    pub fn original_token_count(&self) -> usize {
        self.output.total_bytes().div_ceil(APPROX_BYTES_PER_TOKEN)
    }

    /// Whether the output section must be cut to fit the token budget.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.output.omitted_bytes > 0
            || self.output.bytes.len()
                > self
                    .max_output_tokens
                    .saturating_mul(APPROX_BYTES_PER_TOKEN)
    }

    /// Renders the codex header followed by the bounded output.
    #[must_use]
    pub fn model_text(&self) -> String {
        let mut sections = vec![
            format!("Chunk ID: {}", self.chunk_id),
            format!("Wall time: {:.4} seconds", self.wall_time.as_secs_f64()),
        ];
        if let Some(exit_code) = self.exit_code {
            sections.push(format!("Process exited with code {exit_code}"));
        }
        if let Some(process_id) = self.process_id {
            sections
                .push(format!("Process running with session ID {process_id}"));
        }
        sections.push(format!(
            "Original token count: {}",
            self.original_token_count()
        ));
        sections.push(EXEC_OUTPUT_HEADER.to_string());
        let text = self.output.text();
        let body = match truncate_middle(&text, self.max_output_tokens) {
            Some(truncated) => format!(
                "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{truncated}",
                self.original_token_count(),
                text.lines().count()
            ),
            None => text,
        };
        let mut rendered = format!("{}\n{body}", sections.join("\n"));
        if let Some(path) = &self.full_output_path {
            rendered.push_str(&format!(
                "\n\n{EXEC_FULL_OUTPUT_LABEL}{}",
                path.display()
            ));
        }
        rendered
    }

    /// Error results are exits with a non-zero code or by a signal.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.exit_code.is_some_and(|code| code != 0) || self.signal.is_some()
    }

    /// Returns the typed details carried next to the model text.
    #[must_use]
    pub fn details(&self) -> ToolResultDetails {
        ToolResultDetails::Exec {
            process_id: self.process_id,
            exit_code: self.exit_code,
            signal: self.signal.clone(),
            chunk_id: self.chunk_id.clone(),
            wall_time_ms: u64::try_from(self.wall_time.as_millis())
                .unwrap_or(u64::MAX),
            original_token_count: self.original_token_count(),
            full_output_path: self
                .full_output_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            terminal_call_id: self.terminal_call_id.clone(),
        }
    }
}

/// Keeps the beginning and end of `text` within `max_tokens`, replacing the
/// middle with codex's token marker; returns `None` when the text fits.
fn truncate_middle(text: &str, max_tokens: usize) -> Option<String> {
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if text.len() <= max_bytes {
        return None;
    }
    let left = max_bytes / 2;
    let right = max_bytes - left;
    let prefix_end = text.floor_char_boundary(left);
    let suffix_start = text.ceil_char_boundary(text.len() - right);
    let removed = (text.len() - max_bytes).div_ceil(APPROX_BYTES_PER_TOKEN);
    Some(format!(
        "{}…{removed} tokens truncated…{}",
        text.get(..prefix_end).unwrap_or_default(),
        text.get(suffix_start..).unwrap_or_default()
    ))
}

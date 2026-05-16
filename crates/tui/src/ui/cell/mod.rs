//! Typed transcript cells for the local TUI.

use ratatui::text::Line;

mod terminal_output;
mod text;
mod tool;

pub use text::{TextCell, TextRole};
pub use tool::ToolCallCell;

/// Renderable transcript cell stored in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptCell {
    /// User, assistant, reasoning, or system text.
    Text(TextCell),
    /// ACP tool invocation and its live output state.
    ToolCall(ToolCallCell),
}

impl TranscriptCell {
    /// Creates a text transcript cell for one role.
    pub fn text_cell(role: TextRole, text: impl Into<String>) -> Self {
        Self::Text(TextCell::new(role, text))
    }

    /// Creates a tool-call transcript cell.
    pub fn tool_call(tool: ToolCallCell) -> Self {
        Self::ToolCall(tool)
    }

    /// Returns styled logical lines for this transcript cell.
    pub fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            TranscriptCell::Text(cell) => cell.display_lines(width),
            TranscriptCell::ToolCall(cell) => cell.display_lines(width),
        }
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    pub fn raw_lines(&self) -> Vec<Line<'static>> {
        match self {
            TranscriptCell::Text(cell) => cell.raw_lines(),
            TranscriptCell::ToolCall(cell) => cell.raw_lines(),
        }
    }

    /// Returns wrapped display height for the current terminal width.
    pub fn desired_height(&self, width: u16) -> usize {
        let lines = self.display_lines(width);
        if width == 0 {
            return lines.len();
        }

        let width = usize::from(width);
        lines
            .iter()
            // Empty logical lines still occupy one terminal row.
            .map(|line| line.width().max(1).div_ceil(width))
            .sum()
    }

    /// Returns the primary text payload used by state tests and simple callers.
    pub fn text(&self) -> &str {
        match self {
            TranscriptCell::Text(cell) => cell.text(),
            TranscriptCell::ToolCall(cell) => cell.output(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies text cells expose plain raw lines without display prefixes.
    #[test]
    fn transcript_cell_raw_lines_delegates_to_text_cell() {
        let cell = TranscriptCell::text_cell(TextRole::User, "hello\nworld");

        let lines = cell.raw_lines();

        assert_eq!(lines[0].to_string(), "hello");
        assert_eq!(lines[1].to_string(), "world");
    }

    /// Verifies desired height accounts for terminal wrapping width.
    #[test]
    fn transcript_cell_desired_height_wraps_display_lines() {
        let cell = TranscriptCell::text_cell(TextRole::Assistant, "abcd");

        let height = cell.desired_height(2);

        assert_eq!(height, 2);
    }
}

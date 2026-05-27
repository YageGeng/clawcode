//! Soft wrapping helpers for transcript display lines.

use ratatui::text::Line;

mod boundary;
mod chars;
mod line;

/// Wraps styled logical lines into physical terminal rows before rendering.
pub(super) fn wrap_display_lines(
    lines: Vec<Line<'static>>,
    width: u16,
) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }

    lines
        .into_iter()
        .flat_map(|line| line::wrap_display_line(line, usize::from(width)))
        .collect()
}

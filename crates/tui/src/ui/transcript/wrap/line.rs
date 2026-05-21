//! Single-line soft wrapping.

use ratatui::text::Line;

use super::boundary::next_wrap_range;
use super::chars::{styled_chars, styled_line_from_chars};

/// Wraps one styled line by character display width.
pub(super) fn wrap_display_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let chars = styled_chars(line);
    if chars.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let (end, next_start) = next_wrap_range(&chars, start, width);
        if let Some(slice) = chars.get(start..end) {
            wrapped.push(styled_line_from_chars(slice));
        }
        start = next_start;
    }
    wrapped
}

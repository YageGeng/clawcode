//! Styled character conversion for transcript wrapping.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// One terminal-width-aware styled character used by soft wrapping.
#[derive(Clone, Copy)]
pub(super) struct StyledChar {
    /// Original Unicode scalar value.
    pub(super) ch: char,
    /// Style inherited from the source span.
    pub(super) style: ratatui::style::Style,
    /// Display width used for terminal wrapping.
    pub(super) width: usize,
}

/// Flattens a styled line into per-character units for wrapping.
pub(super) fn styled_chars(line: Line<'static>) -> Vec<StyledChar> {
    line.spans
        .into_iter()
        .flat_map(|span| {
            let style = span.style;
            span.content
                .chars()
                .map(move |ch| StyledChar {
                    ch,
                    style,
                    width: UnicodeWidthChar::width(ch).unwrap_or(0),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Builds a ratatui line from styled character units.
pub(super) fn styled_line_from_chars(chars: &[StyledChar]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut current_style = None;
    let mut current_text = String::new();

    for styled in chars {
        if current_style == Some(styled.style) {
            current_text.push(styled.ch);
            continue;
        }

        if let Some(style) = current_style {
            spans.push(Span::styled(std::mem::take(&mut current_text), style));
        }
        current_style = Some(styled.style);
        current_text.push(styled.ch);
    }

    if let Some(style) = current_style {
        spans.push(Span::styled(current_text, style));
    }

    Line::from(spans)
}

//! Transcript rendering helpers for the local TUI.

use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};
use unicode_width::UnicodeWidthChar;

use crate::ui::state::AppState;
use crate::ui::view::ViewState;

#[derive(Clone, Copy)]
struct StyledChar {
    ch: char,
    style: ratatui::style::Style,
    width: usize,
}

/// Converts transcript and active tool calls into a full frame.
pub(super) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
) {
    let mut lines = Vec::new();
    for cell in state.transcript() {
        lines.extend(wrap_display_lines(
            cell.display_lines(area.width),
            area.width,
        ));
        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        lines.push(Line::from("Ready. Type a message and press Enter."));
    }

    let max_scroll = transcript_scroll_offset(lines.len(), area);
    let scroll_offset = effective_transcript_scroll(max_scroll, view);
    frame.render_widget(Paragraph::new(lines).scroll((scroll_offset, 0)), area);
}

/// Wraps styled logical lines into physical terminal rows before rendering.
fn wrap_display_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    lines
        .into_iter()
        .flat_map(|line| wrap_display_line(line, usize::from(width)))
        .collect()
}

/// Wraps one styled line by character display width.
fn wrap_display_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
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

/// Flattens a styled line into per-character units for wrapping.
fn styled_chars(line: Line<'static>) -> Vec<StyledChar> {
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

/// Returns the visible range and next start offset for one wrapped row.
fn next_wrap_range(chars: &[StyledChar], start: usize, width: usize) -> (usize, usize) {
    let mut used_width = 0usize;
    let mut end = start;
    let mut last_space = None;
    while let Some(styled) = chars.get(end) {
        let next_width = used_width + styled.width;
        if used_width > 0 && next_width > width {
            break;
        }
        used_width = next_width;
        end += 1;
        if styled.ch.is_whitespace() && end > start + 1 {
            last_space = Some(end);
        }
    }

    if end == chars.len() {
        return (end, end);
    }

    if let Some(space_end) = last_space {
        let visible_end = trim_trailing_whitespace(chars, start, space_end);
        return (
            visible_end.max(start + 1),
            skip_leading_whitespace(chars, space_end),
        );
    }

    (end.max(start + 1), end.max(start + 1))
}

/// Removes whitespace at the end of a soft-wrapped row.
fn trim_trailing_whitespace(chars: &[StyledChar], start: usize, mut end: usize) -> usize {
    while end > start
        && chars
            .get(end - 1)
            .map(|styled| styled.ch.is_whitespace())
            .unwrap_or(false)
    {
        end -= 1;
    }
    end
}

/// Skips whitespace consumed as the soft-wrap boundary.
fn skip_leading_whitespace(chars: &[StyledChar], mut start: usize) -> usize {
    while chars
        .get(start)
        .map(|styled| styled.ch.is_whitespace())
        .unwrap_or(false)
    {
        start += 1;
    }
    start
}

/// Builds a ratatui line from styled character units.
fn styled_line_from_chars(chars: &[StyledChar]) -> Line<'static> {
    Line::from(
        chars
            .iter()
            .map(|styled| Span::styled(styled.ch.to_string(), styled.style))
            .collect::<Vec<_>>(),
    )
}

/// Returns the current scroll offset after applying tail-follow/manual view state.
fn effective_transcript_scroll(max_scroll: u16, view: &ViewState) -> u16 {
    if view.is_following_tail() {
        return max_scroll;
    }

    max_scroll.saturating_sub(view.transcript_scroll())
}

/// Calculates vertical scroll offset so the transcript follows the newest content.
fn transcript_scroll_offset(line_count: usize, area: Rect) -> u16 {
    let visible_height = area.height as usize;
    if visible_height == 0 {
        return 0;
    }

    line_count.saturating_sub(visible_height) as u16
}

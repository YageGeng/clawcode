//! Transcript rendering helpers for the local TUI.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use crate::ui::state::{AppState, TranscriptCell};
use crate::ui::tool_render::append_tool_call_lines;
use crate::ui::view::ViewState;

/// Converts transcript and active tool calls into a full frame.
pub(super) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
) {
    let mut lines = Vec::new();
    for cell in state.transcript() {
        append_transcript_cell_lines(&mut lines, cell);
        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        lines.push(Line::from("Ready. Type a message and press Enter."));
    }

    let max_scroll = transcript_scroll_offset(lines.len(), area);
    let scroll_offset = effective_transcript_scroll(max_scroll, view);
    frame.render_widget(Paragraph::new(lines).scroll((scroll_offset, 0)), area);
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

/// Appends transcript text as visible lines so scrolling matches rendered output.
pub(super) fn append_transcript_cell_lines(lines: &mut Vec<Line<'static>>, cell: &TranscriptCell) {
    match cell {
        TranscriptCell::Assistant(text) => {
            append_styled_text_lines(lines, text, "", Style::default().fg(Color::Reset));
        }
        TranscriptCell::Reasoning(text) => {
            append_styled_text_lines(
                lines,
                text,
                "",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            );
        }
        TranscriptCell::User(text) => {
            append_styled_text_lines(
                lines,
                text,
                "> ",
                Style::default().add_modifier(Modifier::BOLD),
            );
        }
        TranscriptCell::System(text) => {
            append_styled_text_lines(
                lines,
                text,
                "system: ",
                Style::default().fg(Color::DarkGray),
            );
        }
        TranscriptCell::ToolCall(tool) => {
            append_tool_call_lines(lines, tool);
        }
    }
}

/// Appends one line per newline-delimited segment with an optional first-line prefix.
fn append_styled_text_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    first_prefix: &str,
    style: Style,
) {
    let mut added = false;
    for (index, segment) in text.split('\n').enumerate() {
        let prefix = if index == 0 { first_prefix } else { "" };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{segment}"),
            style,
        )));
        added = true;
    }

    if !added {
        lines.push(Line::from(Span::styled(first_prefix.to_string(), style)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies reasoning output has a distinct low-emphasis style.
    #[test]
    fn transcript_reasoning_lines_use_distinct_style() {
        let mut lines = Vec::new();

        append_transcript_cell_lines(
            &mut lines,
            &TranscriptCell::Reasoning("thinking".to_string()),
        );

        let span = lines[0].spans.first().expect("reasoning span");
        assert_eq!(span.content, "thinking");
        assert_eq!(span.style.fg, Some(Color::DarkGray));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }
}

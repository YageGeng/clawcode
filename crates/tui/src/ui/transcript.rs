//! Transcript rendering helpers for the local TUI.

use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use crate::ui::state::AppState;
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
        lines.extend(cell.display_lines(area.width));
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

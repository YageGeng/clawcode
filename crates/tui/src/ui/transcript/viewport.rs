//! Viewport selection helpers for cached transcript rows.

use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use crate::ui::view::ViewState;

/// Renders only the visible transcript rows from a cached wrapped transcript.
pub(super) fn render_transcript_lines(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    view: &ViewState,
) {
    let (start, end) = visible_row_range(lines.len(), area, view);
    let visible_lines = lines.get(start..end).unwrap_or(&[]);
    render_transcript_lines_at_top(frame, area, visible_lines);
}

/// Returns the visible transcript row range.
pub(super) fn visible_row_range(line_count: usize, area: Rect, view: &ViewState) -> (usize, usize) {
    let max_scroll = transcript_scroll_offset(line_count, area);
    let scroll_offset = effective_transcript_scroll(max_scroll, view);
    let start = usize::from(scroll_offset).min(line_count);
    let end = start
        .saturating_add(usize::from(area.height))
        .min(line_count);
    (start, end)
}

/// Renders already-sliced rows at the top of the transcript area.
pub(super) fn render_transcript_lines_at_top(
    frame: &mut Frame<'_>,
    area: Rect,
    visible_lines: &[Line<'static>],
) {
    frame.render_widget(Paragraph::new(visible_lines.to_vec()), area);
}

/// Returns the current scroll offset after applying tail-follow/manual view state.
pub(super) fn effective_transcript_scroll(max_scroll: u16, view: &ViewState) -> u16 {
    if view.is_following_tail() {
        return max_scroll;
    }

    max_scroll.saturating_sub(view.transcript_scroll())
}

/// Calculates vertical scroll offset so the transcript follows the newest content.
pub(super) fn transcript_scroll_offset(line_count: usize, area: Rect) -> u16 {
    let visible_height = area.height as usize;
    if visible_height == 0 {
        return 0;
    }

    line_count
        .saturating_sub(visible_height)
        .min(usize::from(u16::MAX)) as u16
}

//! Transcript rendering helpers for the local TUI.

use ratatui::text::Line;
use ratatui::{Frame, layout::Rect};

use crate::ui::state::AppState;
use crate::ui::transcript::cache::TranscriptRenderCache;
use crate::ui::transcript::entry::TranscriptEntry;
use crate::ui::view::ViewState;

pub(super) mod cache;
pub mod cell;
pub mod entry;
mod viewport;
mod wrap;

/// Converts transcript and active tool calls into a full frame.
pub(super) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
) {
    view.with_transcript_render_cache(|cache| {
        render_cached_transcript(frame, area, state, view, cache);
    });
}

/// Renders transcript entries through the per-entry cache.
fn render_cached_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
    cache: &mut TranscriptRenderCache,
) {
    cache.retain_entries(state.transcript().iter().map(TranscriptEntry::id));
    let render_mode = view.transcript_render_mode();
    let total_rows = transcript_row_count(area.width, state, cache, render_mode);
    if total_rows == 0 {
        viewport::render_transcript_lines(
            frame,
            area,
            &[Line::from("Ready. Type a message and press Enter.")],
            view,
        );
        return;
    }

    let (start, end) = viewport::visible_row_range(total_rows, area, view);
    let visible = visible_transcript_lines(area.width, state, cache, render_mode, start, end);
    viewport::render_transcript_lines_at_top(frame, area, &visible);
}

/// Counts transcript rows without cloning cached row contents.
fn transcript_row_count(
    width: u16,
    state: &AppState,
    cache: &mut TranscriptRenderCache,
    render_mode: cell::TranscriptRenderMode,
) -> usize {
    state
        .transcript()
        .iter()
        .map(|entry| {
            cache
                .entry_line_count(width, state.theme(), entry, render_mode)
                .saturating_add(1)
        })
        .sum()
}

/// Clones only rows that intersect the requested visible range.
fn visible_transcript_lines(
    width: u16,
    state: &AppState,
    cache: &mut TranscriptRenderCache,
    render_mode: cell::TranscriptRenderMode,
    start: usize,
    end: usize,
) -> Vec<Line<'static>> {
    let mut visible = Vec::new();
    let mut cursor = 0usize;
    for entry in state.transcript() {
        let lines = cache.entry_lines(width, state.theme(), entry, render_mode);
        append_visible_rows(&mut visible, lines, cursor, start, end);
        cursor = cursor.saturating_add(lines.len());
        append_visible_rows(&mut visible, &[Line::from("")], cursor, start, end);
        cursor = cursor.saturating_add(1);
        if cursor >= end {
            break;
        }
    }
    visible
}

/// Appends only the intersection between one row slice and the visible range.
fn append_visible_rows(
    visible: &mut Vec<Line<'static>>,
    rows: &[Line<'static>],
    row_start: usize,
    visible_start: usize,
    visible_end: usize,
) {
    let row_end = row_start.saturating_add(rows.len());
    let start = row_start.max(visible_start);
    let end = row_end.min(visible_end);
    if start >= end {
        return;
    }
    let local_start = start.saturating_sub(row_start);
    let local_end = end.saturating_sub(row_start);
    if let Some(slice) = rows.get(local_start..local_end) {
        visible.extend(slice.iter().cloned());
    }
}

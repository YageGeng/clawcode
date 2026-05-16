//! Layout helpers for the local TUI.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use typed_builder::TypedBuilder;

/// Holds the vertical regions used by the main TUI frame.
#[derive(TypedBuilder)]
pub(super) struct FrameRows {
    /// Area used for transcript history.
    pub(super) transcript: Rect,
    /// Area used for the top status row.
    pub(super) top_status: Rect,
    /// Area used for the prompt composer.
    pub(super) composer: Rect,
    /// Area used for the bottom status row.
    pub(super) bottom_status: Rect,
}

/// Splits the terminal frame into stable vertical regions.
pub(super) fn frame_rows(area: Rect, composer_text: &str) -> Option<FrameRows> {
    let composer_height = composer_height(composer_text);
    let transcript_min = if area.height >= composer_height.saturating_add(5) {
        3
    } else {
        1
    };
    let constraints = vec![
        Constraint::Min(transcript_min),
        Constraint::Length(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ];

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    match rows.as_ref() {
        [transcript, top_status, composer, bottom_status] => Some(
            FrameRows::builder()
                .transcript(*transcript)
                .top_status(*top_status)
                .composer(*composer)
                .bottom_status(*bottom_status)
                .build(),
        ),
        _ => None,
    }
}

/// Calculates the input composer height based on multiline content and UI budget.
pub(super) fn composer_height(text: &str) -> u16 {
    let line_count = text.lines().count().max(1) as u16;
    // The composer is intentionally borderless, matching Codex-style input
    // chrome while still reserving enough rows for multiline prompts.
    line_count.clamp(3, 6)
}

/// Creates a centered area for modal rendering with fixed width and height.
pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    Rect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies composer height obeys minimum and maximum constraints.
    #[test]
    fn composer_height_with_limits() {
        assert_eq!(composer_height("one"), 3);
        assert_eq!(composer_height("one\ntwo\nthree\nfour"), 4);
        assert_eq!(composer_height("1\n2\n3\n4\n5\n6\n7"), 6);
    }

    /// Verifies centered rectangles never exceed frame bounds.
    #[test]
    fn centered_rect_stays_in_area() {
        let area = Rect::new(0, 0, 20, 10);
        let rect = centered_rect(10, 4, area);

        assert_eq!(rect, Rect::new(5, 3, 10, 4));
        assert_eq!(centered_rect(40, 20, area), area);
    }
}

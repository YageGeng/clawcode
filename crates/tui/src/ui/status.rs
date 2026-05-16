//! Status row rendering helpers for the local TUI.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::state::AppState;

const MODEL_STYLE: Style = Style::new().fg(Color::Rgb(245, 158, 11));
const CWD_STYLE: Style = Style::new().fg(Color::Green);

/// Renders the top status row.
pub(super) fn render_top_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(Paragraph::new(state.top_status_line()), area);
}

/// Renders the bottom status row constrained to the available terminal width.
pub(super) fn render_bottom_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(
        Paragraph::new(bottom_status_line(state, area.width as usize)),
        area,
    );
}

/// Builds the styled bottom status line with model, cwd, and token usage.
fn bottom_status_line(state: &AppState, width: usize) -> Line<'static> {
    let token_status = format!("tokens: {}", state.usage().total_tokens());
    let token_width = UnicodeWidthStr::width(token_status.as_str());
    if width <= token_width {
        return Line::from(truncate_to_display_width(&token_status, width));
    }

    let model = state.model_label();
    let cwd = state.cwd().display().to_string();
    let separator = " | ";
    let suffix_width = UnicodeWidthStr::width(separator) + token_width;
    if width > suffix_width {
        let prefix_width = width - suffix_width;
        let mut spans = styled_prefix_spans(model, &cwd, prefix_width);
        spans.push(Span::raw(separator));
        spans.push(Span::raw(token_status));
        return Line::from(spans);
    }

    Line::from(truncate_to_display_width(
        &format!("{separator}{token_status}"),
        width,
    ))
}

/// Builds colored model/cwd prefix spans after applying display-width truncation.
fn styled_prefix_spans(model: &str, cwd: &str, max_width: usize) -> Vec<Span<'static>> {
    let prefix = truncate_to_display_width(&format!("{model} | {cwd}"), max_width);
    if let Some((model_part, cwd_part)) = prefix.split_once(" | ") {
        vec![
            Span::styled(model_part.to_string(), MODEL_STYLE),
            Span::raw(" | "),
            Span::styled(cwd_part.to_string(), CWD_STYLE),
        ]
    } else {
        vec![Span::styled(prefix, MODEL_STYLE)]
    }
}

/// Returns text truncated to the requested terminal display width.
fn truncate_to_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut truncated = String::new();
    let mut used_width = 0;
    let content_width = max_width - 3;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        // Reserve the last three columns for the ASCII ellipsis marker.
        if used_width + char_width > content_width {
            break;
        }
        truncated.push(ch);
        used_width += char_width;
    }
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::SessionId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Verifies the bottom status row colors model and cwd independently.
    #[test]
    fn bottom_status_styles_model_and_cwd_segments() {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            SessionId::new("s1".to_string()),
            "/tmp/project".into(),
            "gpt-5.5 high".to_string(),
        );

        terminal
            .draw(|frame| render_bottom_status(frame, frame.area(), &state))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer.cell((0, 0)).expect("model cell").fg,
            Color::Rgb(245, 158, 11)
        );
        assert_eq!(buffer.cell((15, 0)).expect("cwd cell").fg, Color::Green);
    }
}

//! Ratatui rendering for the local TUI.

use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::ui::state::{AppState, ToolCallView, TranscriptCell};
use crate::ui::view::ViewState;

/// Render the complete TUI frame for the current application state.
pub fn render(frame: &mut Frame<'_>, state: &AppState, view: &ViewState, composer_text: &str) {
    let composer_height = composer_height(composer_text);
    let show_help = frame.area().height >= composer_height.saturating_add(6);
    let transcript_min = if frame.area().height >= composer_height.saturating_add(5) {
        3
    } else {
        1
    };
    let constraints = if show_help {
        vec![
            Constraint::Min(transcript_min),
            Constraint::Length(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Min(transcript_min),
            Constraint::Length(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ]
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());
    let (transcript_row, top_status_row, composer_row, bottom_status_row, help_row) =
        match rows.as_ref() {
            [
                transcript_row,
                top_status_row,
                composer_row,
                bottom_status_row,
                help_row,
            ] => (
                *transcript_row,
                *top_status_row,
                *composer_row,
                *bottom_status_row,
                Some(*help_row),
            ),
            [
                transcript_row,
                top_status_row,
                composer_row,
                bottom_status_row,
            ] => (
                *transcript_row,
                *top_status_row,
                *composer_row,
                *bottom_status_row,
                None,
            ),
            _ => return,
        };

    render_transcript(frame, transcript_row, state, view);
    frame.render_widget(Paragraph::new(state.top_status_line()), top_status_row);
    render_composer(frame, composer_row, composer_text);
    frame.render_widget(
        Paragraph::new(state.bottom_status_line(bottom_status_row.width as usize)),
        bottom_status_row,
    );
    if let Some(help_row) = help_row {
        render_help_bar(frame, help_row);
    }

    if let Some(approval) = state.pending_approval() {
        let overlay = centered_rect(72, 8, frame.area());
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Paragraph::new(approval_lines(approval.title(), approval.body()))
                .block(Block::default().borders(Borders::ALL)),
            overlay,
        );
    }
}

/// Calculates the input composer height based on multiline content and UI budget.
fn composer_height(text: &str) -> u16 {
    let line_count = text.lines().count().max(1) as u16;
    // The composer is rendered inside a bordered block, so two extra rows are
    // required for the top and bottom borders to avoid hiding the input line.
    line_count.saturating_add(2).clamp(3, 8)
}

/// Converts transcript and active tool calls into a full frame.
fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &AppState, view: &ViewState) {
    let mut lines = Vec::new();
    for cell in state.transcript() {
        append_transcript_cell_lines(&mut lines, cell);
        lines.push(Line::from(""));
    }

    let mut call_keys: Vec<_> = state.tool_calls().keys().collect();
    call_keys.sort_unstable();

    for call_id in call_keys {
        if let Some(call) = state.tool_calls().get(call_id) {
            append_tool_call_lines(
                &mut lines,
                call_id.as_str(),
                call,
                view.tool_calls_collapsed(),
            );
        }
    }

    if lines.is_empty() {
        lines.push(Line::from("Ready. Type a message and press Enter."));
    }

    let max_scroll = transcript_scroll_offset(lines.len(), area);
    let scroll_offset = effective_transcript_scroll(max_scroll, view);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .scroll((scroll_offset, 0)),
        area,
    );
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
    let visible_height = area.height.saturating_sub(2) as usize;
    if visible_height == 0 {
        return 0;
    }

    line_count.saturating_sub(visible_height) as u16
}

/// Appends transcript text as visible lines so scrolling matches rendered output.
fn append_transcript_cell_lines(lines: &mut Vec<Line<'static>>, cell: &TranscriptCell) {
    match cell {
        TranscriptCell::Assistant(text) => {
            append_styled_text_lines(lines, text, "", Style::default().fg(Color::Reset));
        }
        TranscriptCell::Reasoning(text) => {
            append_styled_text_lines(
                lines,
                text,
                "",
                Style::default().add_modifier(Modifier::DIM),
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

/// Renders the input composer row.
fn render_composer(frame: &mut Frame<'_>, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(format!("> {text}"))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Renders the fixed help text line.
fn render_help_bar(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new("Enter submit  Ctrl+J newline  Ctrl+C cancel/quit")
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// Renders one tool-call entry with status, arguments, and output.
fn append_tool_call_lines(
    lines: &mut Vec<Line<'static>>,
    call_id: &str,
    call: &ToolCallView,
    collapsed: bool,
) {
    let status_text = match call.status() {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    };

    if collapsed {
        let output_line_count = terminal_display_lines(call.output()).len();
        lines.push(Line::from(Span::styled(
            format!(
                "[{status_text}] {name} ({call_id})  output: {output_line_count} lines  args: {}",
                compact_inline(call.arguments()),
                name = call.name()
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        return;
    }

    lines.push(Line::from(Span::styled(
        format!("[{status_text}] {name} ({call_id})", name = call.name()),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    append_wrapped_lines(lines, "args", call.arguments());
    append_terminal_output_lines(lines, "out", call.output());
    lines.push(Line::from(""));
}

/// Converts multi-line values into compact single-line summaries.
fn compact_inline(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "<empty>".to_string()
    } else {
        compact
    }
}

/// Appends a labeled text body split into one line per content line.
fn append_wrapped_lines(lines: &mut Vec<Line<'static>>, label: &str, text: &str) {
    lines.push(Line::from(Span::styled(
        format!("{label}:"),
        Style::default().add_modifier(Modifier::DIM),
    )));

    let mut has_content = false;
    for line in text.lines() {
        lines.push(Line::from(line.to_string()));
        has_content = true;
    }

    if !has_content {
        lines.push(Line::from("<empty>".to_string()));
    }
}

/// Appends shell-like output after normalizing terminal control behavior.
fn append_terminal_output_lines(lines: &mut Vec<Line<'static>>, label: &str, text: &str) {
    lines.push(Line::from(Span::styled(
        format!("{label}:"),
        Style::default().add_modifier(Modifier::DIM),
    )));

    let display_lines = terminal_display_lines(text);
    if display_lines.is_empty() {
        lines.push(Line::from("<empty>".to_string()));
        return;
    }

    for line in display_lines {
        lines.push(Line::from(line));
    }
}

/// Converts captured terminal output into stable text lines for ratatui rendering.
fn terminal_display_lines(text: &str) -> Vec<String> {
    let text = strip_ansi_control_sequences(text);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    let _ = chars.next();
                    lines.push(std::mem::take(&mut current));
                } else {
                    // Carriage return rewrites the current terminal row; keep only the final state.
                    current.clear();
                }
            }
            '\n' => {
                lines.push(std::mem::take(&mut current));
            }
            '\t' => current.push('\t'),
            ch if ch.is_control() => {}
            ch => current.push(ch),
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

/// Removes common ANSI escape/control sequences from captured command output.
fn strip_ansi_control_sequences(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            stripped.push(ch);
            continue;
        }

        // Consume a basic ANSI escape sequence through its final byte.
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }

    stripped
}

/// Builds overlay content for pending user approval prompts.
fn approval_lines(title: &str, body: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(body.to_string()),
        Line::from(""),
        Line::from(Span::styled(
            "[a] allow once   [r] reject",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]
}

/// Creates a centered area for modal rendering with fixed width and height.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    Rect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate,
        TextContent, ToolCall, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Builds an ACP session id for render tests.
    fn sid(value: &str) -> SessionId {
        SessionId::new(value.to_string())
    }

    /// Builds a text content block for render fixtures.
    fn text(value: impl Into<String>) -> ContentBlock {
        ContentBlock::Text(TextContent::new(value))
    }

    /// Applies an ACP assistant chunk to render state.
    fn push_assistant(state: &mut AppState, session_id: &SessionId, text_value: impl Into<String>) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text(text_value))),
        ));
    }

    /// Applies an ACP tool call snapshot to render state.
    fn push_tool_call(state: &mut AppState, session_id: &SessionId, command: &str) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new("call-1"), "shell")
                    .status(ToolCallStatus::Completed)
                    .raw_input(serde_json::json!({"command": command})),
            ),
        ));
    }

    /// Applies ACP tool output content to render state.
    fn push_tool_output(state: &mut AppState, session_id: &SessionId, output: &str) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .content(vec![
                        agent_client_protocol::schema::ToolCallContent::Content(Content::new(
                            text(output),
                        )),
                    ])
                    .status(ToolCallStatus::Completed),
            )),
        ));
    }

    /// Verifies the renderer can produce output in a small terminal area.
    #[test]
    fn render_handles_small_terminal() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
            .expect("draw");
    }

    /// Verifies the bordered input area has enough height to render text and borders.
    #[test]
    fn render_keeps_composer_text_visible_above_status_bar() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), "hello"))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("> hello")));
    }

    /// Verifies terminal resize keeps core chrome visible instead of collapsing the input box.
    #[test]
    fn render_after_resize_keeps_input_and_status_visible() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        state.append_user_message("a long prompt that should survive terminal resizing");

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), "resize"))
            .expect("draw");
        terminal.backend_mut().resize(36, 8);
        terminal.resize(Rect::new(0, 0, 36, 8)).expect("resize");
        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), "resize"))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("> resize")));
        assert!(screen.iter().any(|line| line.contains("tokens:")));
    }

    /// Verifies the transcript follows the newest assistant output when content overflows.
    #[test]
    fn render_transcript_scrolls_to_latest_output() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        let session_id = sid("s1");
        for index in 0..12 {
            push_assistant(&mut state, &session_id, format!("old output {index}\n"));
        }
        push_assistant(&mut state, &session_id, "latest assistant output");

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(
            screen
                .iter()
                .any(|line| line.contains("latest assistant output"))
        );
    }

    /// Verifies manual transcript scrolling can reveal older output.
    #[test]
    fn render_transcript_manual_scroll_shows_older_output() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let mut view = ViewState::default();

        let session_id = sid("s1");
        for index in 0..12 {
            push_assistant(&mut state, &session_id, format!("old output {index}\n"));
        }
        push_assistant(&mut state, &session_id, "latest assistant output");
        view.scroll_page_up(16);

        terminal
            .draw(|frame| render(frame, &state, &view, ""))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("old output")));
        assert!(
            !screen
                .iter()
                .any(|line| line.contains("latest assistant output"))
        );
    }

    /// Verifies shell-style carriage-return progress output renders as the final terminal line.
    #[test]
    fn render_tool_output_handles_carriage_return_updates() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_tool_call(&mut state, &session_id, "progress");
        push_tool_output(
            &mut state,
            &session_id,
            "stdout:\nold progress\rlatest progress\n",
        );

        let mut view = ViewState::default();
        view.toggle_tool_calls();

        terminal
            .draw(|frame| render(frame, &state, &view, ""))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("latest progress")));
        assert!(!screen.iter().any(|line| line.contains("old progress")));
    }

    /// Verifies tool calls render as collapsed summaries by default.
    #[test]
    fn render_tool_call_defaults_to_collapsed_summary() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_tool_call(&mut state, &session_id, "printf hello");
        push_tool_output(&mut state, &session_id, "stdout:\nfull shell output\n");

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("output: 2 lines")));
        assert!(!screen.iter().any(|line| line.contains("full shell output")));
    }

    /// Verifies composer height obeys minimum and maximum constraints.
    #[test]
    fn composer_height_with_limits() {
        assert_eq!(composer_height(""), 3);
        assert_eq!(composer_height("a"), 3);
        assert_eq!(composer_height("a\nb\nc\nd\ne\nf\ng"), 8);
        assert_eq!(composer_height("a\nb\nc"), 5);
    }

    /// Verifies centered rectangles never exceed frame bounds.
    #[test]
    fn centered_rect_stays_in_area() {
        let area = Rect::new(0, 0, 20, 10);
        let overlay = centered_rect(72, 8, area);

        assert!(overlay.x >= area.x);
        assert!(overlay.y >= area.y);
        assert!(overlay.right() <= area.right());
        assert!(overlay.bottom() <= area.bottom());
    }

    /// Returns the test backend screen as printable rows.
    fn rendered_screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol()))
                    .collect::<String>()
            })
            .collect()
    }
}

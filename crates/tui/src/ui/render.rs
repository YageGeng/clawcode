//! Ratatui rendering for the local TUI.

use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::ui::composer::Composer;
use crate::ui::session_router::SessionRouterState;
use crate::ui::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::view::ViewState;
use crate::ui::{agent_picker, approval, layout, status, transcript};

/// Render the complete TUI frame for the current application state.
pub fn render(
    frame: &mut Frame<'_>,
    state: &AppState,
    view: &ViewState,
    composer: &Composer,
) {
    let Some(rows) = layout::frame_rows(frame.area(), composer.text()) else {
        return;
    };

    transcript::render_transcript(frame, rows.transcript, state, view);
    status::render_top_status(frame, rows.top_status, state);
    render_composer(frame, rows.composer, composer, state.theme());
    status::render_bottom_status(frame, rows.bottom_status, state);

    if let Some(approval) = state.pending_approval() {
        let overlay = layout::centered_rect(72, 8, frame.area());
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Paragraph::new(approval::approval_lines(
                approval.title(),
                approval.body(),
            ))
            .block(Block::default().borders(Borders::ALL)),
            overlay,
        );
    }
}

/// Render the complete TUI frame for the active session router state.
pub(crate) fn render_router(
    frame: &mut Frame<'_>,
    router: &SessionRouterState,
    view: &ViewState,
    composer: &Composer,
) {
    let Some(rows) = layout::frame_rows_with_agent_picker(
        frame.area(),
        composer.text(),
        router.agent_picker_height(),
    ) else {
        return;
    };
    let state = router.active_state();

    transcript::render_transcript(frame, rows.transcript, state, view);
    status::render_top_status(frame, rows.top_status, state);
    render_composer(frame, rows.composer, composer, state.theme());
    agent_picker::render_agent_picker(
        frame,
        rows.agent_picker,
        router,
        state.theme(),
    );
    status::render_bottom_status_with_agent(
        frame,
        rows.bottom_status,
        state,
        router.active_agent_label().as_str(),
    );

    if let Some(approval) = state.pending_approval() {
        let overlay = layout::centered_rect(72, 8, frame.area());
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Paragraph::new(approval::approval_lines(
                approval.title(),
                approval.body(),
            ))
            .block(Block::default().borders(Borders::ALL)),
            overlay,
        );
    }
}

/// Renders the input composer row.
fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    composer: &Composer,
    theme: &Theme,
) {
    // No explicit background; lets the terminal background show through.
    let text = composer.text();
    let composer_style = Style::default().bg(theme.composer_bg());
    // Use the full allocated area so that ratatui's Paragraph::wrap can
    // flow long single-line prompts across multiple visual rows.  Shrinking
    // the input area with vertical-padding based on newline count would
    // clip wrapped content to a single row.
    // When the text has more visual lines than the available area, scroll
    // the paragraph content so the cursor line remains visible.
    let (cursor_column, cursor_row) =
        composer.cursor_cell_offset(area.width, 2);
    let scroll_row = cursor_row.saturating_sub(area.height.saturating_sub(1));
    frame.render_widget(Paragraph::new("").style(composer_style), area);
    frame.render_widget(
        Paragraph::new(format!("> {text}"))
            .wrap(Wrap { trim: false })
            .scroll((scroll_row, 0))
            .style(composer_style),
        area,
    );
    let cursor_x = area
        .x
        .saturating_add(cursor_column.min(area.width.saturating_sub(1)));
    let cursor_y = area.y.saturating_add(
        cursor_row
            .saturating_sub(scroll_row)
            .min(area.height.saturating_sub(1)),
    );
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, ContentChunk, SessionId, SessionNotification,
        SessionUpdate, TextContent, ToolCall, ToolCallId, ToolCallStatus,
        ToolCallUpdate, ToolCallUpdateFields,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    /// Builds an ACP session id for render tests.
    fn sid(value: &str) -> SessionId {
        SessionId::new(value.to_string())
    }

    /// Builds a text content block for render fixtures.
    fn text(value: impl Into<String>) -> ContentBlock {
        ContentBlock::Text(TextContent::new(value))
    }

    /// Applies an ACP assistant chunk to render state.
    fn push_assistant(
        state: &mut AppState,
        session_id: &SessionId,
        text_value: impl Into<String>,
    ) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text(
                text_value,
            ))),
        ));
    }

    /// Applies an ACP tool call snapshot to render state.
    fn push_tool_call_with_args(
        state: &mut AppState,
        session_id: &SessionId,
        call_id: &str,
        name: &str,
        status: ToolCallStatus,
        raw_input: serde_json::Value,
    ) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new(call_id), name)
                    .status(status)
                    .raw_input(raw_input),
            ),
        ));
    }

    /// Applies a shell tool call snapshot to render state.
    fn push_tool_call(
        state: &mut AppState,
        session_id: &SessionId,
        command: &str,
    ) {
        push_tool_call_with_args(
            state,
            session_id,
            "call-1",
            "shell",
            ToolCallStatus::Completed,
            serde_json::json!({"command": command}),
        );
    }

    /// Applies ACP tool output content to render state.
    fn push_tool_output(
        state: &mut AppState,
        session_id: &SessionId,
        output: &str,
    ) {
        state.apply_session_update(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .content(vec![
                        agent_client_protocol::schema::ToolCallContent::Content(
                            Content::new(text(output)),
                        ),
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
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
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
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer("hello"))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("> hello")));
    }

    /// Verifies the borderless composer places the terminal cursor in the input row.
    #[test]
    fn render_places_cursor_at_end_of_composer_text() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer("hello"))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        let input_y = screen
            .iter()
            .position(|line| line.contains("> hello"))
            .expect("input row") as u16;
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(7, input_y));
    }

    /// Verifies the composer cursor follows the editable prompt cursor offset.
    #[test]
    fn render_places_cursor_at_composer_cursor_offset() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let mut composer = composer("hello");
        composer.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        composer.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        composer.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::CONTROL,
        ));

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer)
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        let input_y = screen
            .iter()
            .position(|line| line.contains("> hello"))
            .expect("input row") as u16;
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(4, input_y));
    }

    /// Verifies the composer follows the Codex-style borderless input treatment.
    #[test]
    fn composer_area_is_borderless() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &ViewState::default(),
                    &composer("borderless"),
                )
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        let input_line = screen
            .iter()
            .find(|line| line.contains("> borderless"))
            .expect("input line");
        let input_index = screen
            .iter()
            .position(|line| line.contains("> borderless"))
            .expect("input index");

        assert!(!input_line.contains('│'));
        assert!(!screen[input_index.saturating_sub(1)].contains('┌'));
        assert!(!screen[input_index.saturating_add(1)].contains('└'));
    }

    /// Verifies the borderless composer keeps breathing room above the bottom status line.
    #[test]
    fn composer_keeps_gap_above_status_bar() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &ViewState::default(),
                    &composer("spaced"),
                )
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        let input_index = screen
            .iter()
            .position(|line| line.contains("> spaced"))
            .expect("input index");
        let status_index = screen
            .iter()
            .position(|line| line.contains("tokens:"))
            .expect("status index");

        assert!(status_index >= input_index + 2);
    }

    /// Verifies the old keyboard shortcut help row is no longer rendered.
    #[test]
    fn render_omits_shortcut_help_row() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal).join("\n");
        assert!(!screen.contains("Enter submit"));
        assert!(!screen.contains("Ctrl+J newline"));
    }

    /// Verifies assistant output uses the same borderless treatment as Codex.
    #[test]
    fn transcript_area_is_borderless() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_assistant(&mut state, &session_id, "assistant body");

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        let assistant_index = screen
            .iter()
            .position(|line| line.contains("assistant body"))
            .expect("assistant output");

        assert!(!screen[assistant_index].contains('│'));
        assert!(!screen[assistant_index.saturating_sub(1)].contains('┌'));
        assert!(!screen[assistant_index.saturating_add(1)].contains('└'));
    }

    /// Verifies long transcript lines wrap vertically instead of being truncated.
    #[test]
    fn transcript_wraps_long_assistant_lines() {
        let backend = TestBackend::new(24, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_assistant(
            &mut state,
            &session_id,
            "alpha beta gamma delta epsilon zeta",
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("alpha beta")));
        assert!(screen.iter().any(|line| line.contains("epsilon zeta")));
    }

    /// Verifies streaming updates remain visible after the transcript has wrapped and overflowed.
    #[test]
    fn transcript_streaming_keeps_latest_wrapped_line_live() {
        let backend = TestBackend::new(26, 9);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_assistant(
            &mut state,
            &session_id,
            "alpha beta gamma delta epsilon zeta eta theta iota",
        );
        push_assistant(&mut state, &session_id, " kappa");

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("kappa")));
    }

    /// Verifies long words still stream through wrapping instead of disappearing until newline.
    #[test]
    fn transcript_wraps_long_unbroken_streaming_text() {
        let backend = TestBackend::new(18, 9);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_assistant(&mut state, &session_id, "abcdefghijklmnop");
        push_assistant(&mut state, &session_id, "qrstuvwxyz");

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("abcdefgh")));
        assert!(screen.iter().any(|line| line.contains("stuvwxyz")));
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
        state.append_user_message(
            "a long prompt that should survive terminal resizing",
        );

        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &ViewState::default(),
                    &composer("resize"),
                )
            })
            .expect("draw");
        terminal.backend_mut().resize(36, 8);
        terminal.resize(Rect::new(0, 0, 36, 8)).expect("resize");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &ViewState::default(),
                    &composer("resize"),
                )
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("> resize")));
        assert!(screen.iter().any(|line| line.contains("tokens:")));
    }

    /// Verifies the transcript follows the newest assistant output when content overflows.
    #[test]
    fn transcript_scrolls_to_latest_output() {
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        let session_id = sid("s1");
        for index in 0..12 {
            push_assistant(
                &mut state,
                &session_id,
                format!("old output {index}\n"),
            );
        }
        push_assistant(&mut state, &session_id, "latest assistant output");

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
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
    fn transcript_manual_scroll_shows_older_output() {
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
            push_assistant(
                &mut state,
                &session_id,
                format!("old output {index}\n"),
            );
        }
        push_assistant(&mut state, &session_id, "latest assistant output");
        view.scroll_page_up(16);

        terminal
            .draw(|frame| render(frame, &state, &view, &composer("")))
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

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("latest progress")));
        assert!(!screen.iter().any(|line| line.contains("old progress")));
    }

    /// Verifies tool calls render the first preview lines by default.
    #[test]
    fn render_tool_call_defaults_to_preview() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_tool_call(&mut state, &session_id, "printf hello");
        push_tool_output(
            &mut state,
            &session_id,
            "stdout:\nfull shell output\n",
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(
            screen
                .iter()
                .any(|line| line.contains("• Ran printf hello"))
        );
        assert!(screen.iter().any(|line| line.contains("stdout:")));
        assert!(screen.iter().any(|line| line.contains("full shell output")));
    }

    /// Verifies the composer input area allows multi-line visual wrapping
    /// when a single logical line exceeds the available width.
    #[test]
    fn composer_wraps_long_single_line_content() {
        // Narrow terminal so a long prompt must wrap.
        let backend = TestBackend::new(24, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        terminal
            .draw(|frame| {
                render(
                    frame,
                    &state,
                    &ViewState::default(),
                    &composer("abcdefghijklmnopqrstuvwxyz"),
                )
            })
            .expect("draw");
        // The long prompt should wrap across multiple visual rows.
        let screen = rendered_screen(&terminal);
        // With "> " prefix + 26-char text in a 24-column terminal, the text
        // wraps: the prefix occupies the first line, then content flows.
        assert!(screen.iter().any(|line| line.contains("> ")));
        assert!(
            screen
                .iter()
                .any(|line| line.contains("abcdefghijklmnopqrstuvwx"))
        );
        assert!(screen.iter().any(|line| line.contains("yz")));
    }

    /// Verifies multiline composer input keeps the cursor line visible
    /// when explicit newlines push content beyond the composer area.
    #[test]
    fn composer_scrolls_to_keep_cursor_visible_in_multiline_input() {
        // Small terminal so the composer area is limited.
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );

        // Simulate typing several lines; cursor is at the end of the last line.
        let text = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8";
        let composer = composer(text);

        // The composer area is at most 6 rows (clamped by layout), but text
        // has 5 logical lines.  If the cursor is on line 5 and only 3-6 rows
        // are visible, the last line should still appear on screen.
        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer)
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        // The composer area height maxes at 6 rows (layout clamp).
        // With 8 logical lines, scrolling must reveal the last line where
        // the cursor sits.
        assert!(
            screen.iter().any(|line| line.contains("line8")),
            "last line should be visible in composer area"
        );
    }

    /// Verifies shell tool calls use Codex-style headers and five-line output previews.
    #[test]
    fn render_tool_call_shell_preview_uses_codex_style() {
        let backend = TestBackend::new(90, 18);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_tool_call_with_args(
            &mut state,
            &session_id,
            "shell-1",
            "shell",
            ToolCallStatus::Completed,
            serde_json::json!({"command": "cargo test -p tui"}),
        );
        state.apply_session_update(SessionNotification::new(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("shell-1"),
                ToolCallUpdateFields::new().content(vec![
                    agent_client_protocol::schema::ToolCallContent::Content(
                        Content::new(text(
                            "line1\nline2\nline3\nline4\nline5\nline6",
                        )),
                    ),
                ]),
            )),
        ));

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(
            screen
                .iter()
                .any(|line| line.contains("• Ran cargo test -p tui"))
        );
        assert!(screen.iter().any(|line| line.contains("  └ line1")));
        assert!(screen.iter().any(|line| line.contains("    line5")));
        assert!(screen.iter().any(|line| line.contains("... +1 lines")));
        assert!(!screen.iter().any(|line| line.contains("line6")));
        assert!(!screen.iter().any(|line| line.contains("[done]")));
    }

    /// Verifies running tool calls without output do not claim there is no output yet.
    #[test]
    fn render_running_tool_call_without_output_shows_header_only() {
        let backend = TestBackend::new(90, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        push_tool_call_with_args(
            &mut state,
            &session_id,
            "shell-1",
            "shell",
            ToolCallStatus::InProgress,
            serde_json::json!({"command": "cargo test -p tui"}),
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(
            screen
                .iter()
                .any(|line| line.contains("• Running cargo test -p tui"))
        );
        assert!(!screen.iter().any(|line| line.contains("(no output)")));
    }

    /// Verifies supported tool categories render concise titles from arguments.
    #[test]
    fn render_tool_call_titles_for_supported_categories() {
        let backend = TestBackend::new(120, 42);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        let cases = [
            (
                "read-1",
                "read_file",
                serde_json::json!({"path": "src/lib.rs", "offset": 2, "limit": 3}),
                "• Ran Read src/lib.rs · lines 2..5",
            ),
            (
                "write-1",
                "write_file",
                serde_json::json!({"path": "docs/spec.md", "content": "very secret content"}),
                "• Ran Write docs/spec.md",
            ),
            (
                "edit-1",
                "edit",
                serde_json::json!({"filePath": "src/main.rs", "oldString": "old secret", "newString": "new secret", "replaceAll": true}),
                "• Ran Edit src/main.rs · replace all",
            ),
            (
                "patch-1",
                "apply_patch",
                serde_json::json!({"patchText": "*** Begin Patch\n*** End Patch"}),
                "• Ran Apply patch",
            ),
            (
                "skill-1",
                "skill",
                serde_json::json!({"name": "rust-best-practices"}),
                "• Ran Load skill rust-best-practices",
            ),
            (
                "agent-1",
                "spawn_agent",
                serde_json::json!({"task_name": "inspect-tui", "role": "explorer", "prompt": "inspect"}),
                "• Ran Spawn agent explorer: inspect-tui",
            ),
            (
                "mcp-1",
                "mcp__filesystem__read_file",
                serde_json::json!({"path": "README.md"}),
                "• Ran MCP filesystem/read_file · README.md",
            ),
            (
                "unknown-1",
                "custom_tool",
                serde_json::json!({"id": "123"}),
                "• Ran custom_tool {\"id\":\"123\"}",
            ),
        ];

        for (call_id, name, raw_input, _) in &cases {
            push_tool_call_with_args(
                &mut state,
                &session_id,
                call_id,
                name,
                ToolCallStatus::Completed,
                raw_input.clone(),
            );
        }

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal).join("\n");
        for (_, _, _, expected) in &cases {
            assert!(screen.contains(expected), "missing {expected}\n{screen}");
        }
        assert!(!screen.contains("very secret content"));
        assert!(!screen.contains("old secret"));
        assert!(!screen.contains("new secret"));
    }

    /// Verifies subagent message tools include a bounded content preview.
    #[test]
    fn render_subagent_message_tool_titles_include_bounded_content_preview() {
        let backend = TestBackend::new(140, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        let session_id = sid("s1");
        let long_content =
            "please inspect this very long task description ".repeat(5);
        push_tool_call_with_args(
            &mut state,
            &session_id,
            "send-1",
            "send_message",
            ToolCallStatus::Completed,
            serde_json::json!({"to": "Tesla", "content": long_content}),
        );
        push_tool_call_with_args(
            &mut state,
            &session_id,
            "follow-1",
            "followup_task",
            ToolCallStatus::Completed,
            serde_json::json!({"to": "Galileo", "content": "run focused tests"}),
        );

        terminal
            .draw(|frame| {
                render(frame, &state, &ViewState::default(), &composer(""))
            })
            .expect("draw");

        let screen = rendered_screen(&terminal).join("\n");
        assert!(screen.contains(
            "• Ran Send message to Tesla · please inspect this very long"
        ));
        assert!(screen.contains("..."));
        assert!(screen.contains("• Ran Follow up Galileo · run focused tests"));
        assert!(!screen.contains(
            "description please inspect this very long task description"
        ));
    }

    /// Verifies `/agent` picker renders Main [default] under the composer.
    #[test]
    fn render_agent_picker_shows_main_default_under_composer() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut router = SessionRouterState::new(
            sid("root-session"),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        router.open_agent_picker();

        terminal
            .draw(|frame| {
                render_router(
                    frame,
                    &router,
                    &ViewState::default(),
                    &composer(""),
                )
            })
            .expect("draw");

        let screen = rendered_screen(&terminal);
        assert!(screen.iter().any(|line| line.contains("Main [default]")));
    }

    /// Verifies the bottom status keeps the active agent visible when cwd is long.
    #[test]
    fn render_bottom_status_shows_active_agent_after_switch() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let root = sid("root-session");
        let child = sid("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/very/long/project/path/that/would/otherwise/hide/the/agent/label"
                .into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let metadata = protocol::AgentUiMetadata::builder()
            .session_id(protocol::SessionId::from("child-session"))
            .parent_session_id(protocol::SessionId::from("root-session"))
            .agent_path(protocol::AgentPath::root().join("inspect"))
            .nickname("finder".to_string())
            .role("worker".to_string())
            .status(protocol::AgentStatus::Running)
            .is_root(false)
            .build();
        let patch = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(protocol::AgentUiEventKind::Upsert)
            .agents(vec![metadata])
            .build();
        let meta = serde_json::json!({
            "clawcode": {
                "subagents": patch,
            }
        })
        .as_object()
        .cloned()
        .expect("metadata root should be an object");
        router.apply_session_notification(SessionNotification::new(
            root,
            SessionUpdate::ToolCallUpdate(
                ToolCallUpdate::new(
                    ToolCallId::new("clawcode-subagents"),
                    ToolCallUpdateFields::default(),
                )
                .meta(meta),
            ),
        ));
        router
            .select_agent_session(
                child,
                &mut ViewState::default(),
                &mut Composer::default(),
            )
            .expect("select child");

        terminal
            .draw(|frame| {
                render_router(
                    frame,
                    &router,
                    &ViewState::default(),
                    &composer(""),
                )
            })
            .expect("draw");

        let screen = rendered_screen(&terminal).join("\n");
        assert!(screen.contains("agent: finder [worker]"));
    }

    /// Builds a composer fixture with the cursor at the end of the provided text.
    fn composer(text: &str) -> Composer {
        let mut composer = Composer::default();
        composer.insert_str(text);
        composer
    }

    /// Returns the test backend screen as printable rows.
    fn rendered_screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| {
                        buffer.cell((x, y)).map(|cell| cell.symbol())
                    })
                    .collect::<String>()
            })
            .collect()
    }
}

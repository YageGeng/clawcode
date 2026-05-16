//! Ratatui rendering for the local TUI.

use ratatui::style::{Color, Style};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::ui::state::AppState;
use crate::ui::view::ViewState;
use crate::ui::{approval, layout, status, transcript};

/// Render the complete TUI frame for the current application state.
pub fn render(frame: &mut Frame<'_>, state: &AppState, view: &ViewState, composer_text: &str) {
    let Some(rows) = layout::frame_rows(frame.area(), composer_text) else {
        return;
    };

    transcript::render_transcript(frame, rows.transcript, state, view);
    status::render_top_status(frame, rows.top_status, state);
    render_composer(frame, rows.composer, composer_text);
    status::render_bottom_status(frame, rows.bottom_status, state);

    if let Some(approval) = state.pending_approval() {
        let overlay = layout::centered_rect(72, 8, frame.area());
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Paragraph::new(approval::approval_lines(approval.title(), approval.body()))
                .block(Block::default().borders(Borders::ALL)),
            overlay,
        );
    }
}

/// Renders the input composer row.
fn render_composer(frame: &mut Frame<'_>, area: Rect, text: &str) {
    let composer_style = Style::default().bg(Color::Rgb(235, 238, 244));
    let vertical_padding = area
        .height
        .saturating_sub(text.lines().count().max(1) as u16)
        / 2;
    let input_area = Rect {
        y: area.y.saturating_add(vertical_padding),
        height: area.height.saturating_sub(vertical_padding * 2),
        ..area
    };

    frame.render_widget(Paragraph::new("").style(composer_style), area);
    frame.render_widget(
        Paragraph::new(format!("> {text}"))
            .wrap(Wrap { trim: false })
            .style(composer_style),
        input_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate,
        TextContent, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
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
    fn push_tool_call(state: &mut AppState, session_id: &SessionId, command: &str) {
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
            .draw(|frame| render(frame, &state, &ViewState::default(), "borderless"))
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
            .draw(|frame| render(frame, &state, &ViewState::default(), "spaced"))
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
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
        push_tool_output(&mut state, &session_id, "stdout:\nfull shell output\n");

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
                    agent_client_protocol::schema::ToolCallContent::Content(Content::new(text(
                        "line1\nline2\nline3\nline4\nline5\nline6",
                    ))),
                ]),
            )),
        ));

        terminal
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
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
        let long_content = "please inspect this very long task description ".repeat(5);
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
            .draw(|frame| render(frame, &state, &ViewState::default(), ""))
            .expect("draw");

        let screen = rendered_screen(&terminal).join("\n");
        assert!(screen.contains("• Ran Send message to Tesla · please inspect this very long"));
        assert!(screen.contains("..."));
        assert!(screen.contains("• Ran Follow up Galileo · run focused tests"));
        assert!(!screen.contains("description please inspect this very long task description"));
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

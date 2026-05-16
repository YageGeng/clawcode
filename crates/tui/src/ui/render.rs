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
    // The composer is intentionally borderless, matching Codex-style input
    // chrome while still reserving enough rows for multiline prompts.
    line_count.clamp(3, 6)
}

/// Converts transcript and active tool calls into a full frame.
fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &AppState, view: &ViewState) {
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

/// Renders the fixed help text line.
fn render_help_bar(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new("Enter submit  Ctrl+J newline  Ctrl+C cancel/quit")
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;

/// Renders one tool-call transcript cell with a Codex-style compact preview.
fn append_tool_call_lines(lines: &mut Vec<Line<'static>>, call: &ToolCallView) {
    lines.push(Line::from(vec![
        status_bullet(call.status()),
        " ".into(),
        Span::styled(
            format!("{} {}", status_verb(call.status()), tool_summary(call)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    append_tool_output_preview_lines(lines, call.status(), call.output());
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

/// Appends the first five display lines from normalized tool output.
fn append_tool_output_preview_lines(
    lines: &mut Vec<Line<'static>>,
    status: ToolCallStatus,
    text: &str,
) {
    let display_lines = terminal_display_lines(text);
    if display_lines.is_empty() {
        if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
            lines.push(dim_line("  └ (no output)"));
        }
        return;
    }

    for (index, line) in display_lines
        .iter()
        .take(TOOL_OUTPUT_PREVIEW_LINES)
        .enumerate()
    {
        let prefix = if index == 0 { "  └ " } else { "    " };
        lines.push(dim_line(format!("{prefix}{line}")));
    }

    if display_lines.len() > TOOL_OUTPUT_PREVIEW_LINES {
        let omitted = display_lines.len() - TOOL_OUTPUT_PREVIEW_LINES;
        lines.push(dim_line(format!("    ... +{omitted} lines")));
    }
}

/// Builds a dimmed display line for secondary tool output.
fn dim_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

/// Returns the status verb shown in the tool-call header.
fn status_verb(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "Queued",
        ToolCallStatus::InProgress => "Running",
        ToolCallStatus::Completed => "Ran",
        ToolCallStatus::Failed => "Failed",
        _ => "Tool",
    }
}

/// Returns the bullet style shown in the tool-call header.
fn status_bullet(status: ToolCallStatus) -> Span<'static> {
    let style = match status {
        ToolCallStatus::Completed => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        ToolCallStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::DIM),
    };
    Span::styled("•", style)
}

/// Builds a concise category-specific title for a tool call.
fn tool_summary(call: &ToolCallView) -> String {
    let args = tool_arguments(call.arguments());
    match call.name() {
        "shell" => shell_summary(&args),
        "read_file" => read_file_summary(&args),
        "write_file" => path_summary("Write", &args, "path"),
        "edit" => edit_summary(&args),
        "apply_patch" => "Apply patch".to_string(),
        "skill" => path_summary("Load skill", &args, "name"),
        "spawn_agent" => spawn_agent_summary(&args),
        "send_message" => message_tool_summary("Send message to", &args),
        "followup_task" => message_tool_summary("Follow up", &args),
        "wait_agent" => path_summary("Wait agent", &args, "agent_path"),
        "list_agents" => "List agents".to_string(),
        "close_agent" => path_summary("Close agent", &args, "agent_path"),
        name if name.starts_with("mcp__") => mcp_summary(name, &args),
        name => unknown_tool_summary(name, call.arguments()),
    }
}

/// Parses stored JSON arguments for summary rendering.
fn tool_arguments(arguments: &str) -> serde_json::Value {
    serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null)
}

/// Builds the shell command summary.
fn shell_summary(args: &serde_json::Value) -> String {
    let command = string_field(args, "command").unwrap_or("shell");
    match string_field(args, "cwd") {
        Some(cwd) => format!("{command} · cwd: {cwd}"),
        None => command.to_string(),
    }
}

/// Builds the read_file summary with optional line range.
fn read_file_summary(args: &serde_json::Value) -> String {
    let path = string_field(args, "path").unwrap_or("<unknown>");
    let Some(offset) = args.get("offset").and_then(serde_json::Value::as_u64) else {
        return format!("Read {path}");
    };
    let Some(limit) = args.get("limit").and_then(serde_json::Value::as_u64) else {
        return format!("Read {path} · lines {offset}..");
    };
    format!("Read {path} · lines {offset}..{}", offset + limit)
}

/// Builds a path-like summary for tools whose primary argument is one string field.
fn path_summary(prefix: &str, args: &serde_json::Value, field: &str) -> String {
    match string_field(args, field) {
        Some(value) if !value.is_empty() => format!("{prefix} {value}"),
        _ => prefix.to_string(),
    }
}

/// Builds the edit summary without leaking oldString/newString bodies.
fn edit_summary(args: &serde_json::Value) -> String {
    let mut summary = path_summary("Edit", args, "filePath");
    if args
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        summary.push_str(" · replace all");
    }
    summary
}

/// Builds the spawn_agent summary from role and task name.
fn spawn_agent_summary(args: &serde_json::Value) -> String {
    let role = string_field(args, "role").unwrap_or("default");
    let task = string_field(args, "task_name").unwrap_or("task");
    format!("Spawn agent {role}: {task}")
}

/// Builds subagent message summaries with a bounded content preview.
fn message_tool_summary(prefix: &str, args: &serde_json::Value) -> String {
    let mut summary = path_summary(prefix, args, "to");
    if let Some(content) = string_field(args, "content") {
        let preview = truncate_chars(&compact_inline(content), 80);
        if !preview.is_empty() && preview != "<empty>" {
            summary.push_str(" · ");
            summary.push_str(&preview);
        }
    }
    summary
}

/// Builds the MCP summary from its namespaced tool name and common target fields.
fn mcp_summary(name: &str, args: &serde_json::Value) -> String {
    let rest = name.strip_prefix("mcp__").unwrap_or(name);
    let mut parts = rest.splitn(2, "__");
    let server = parts.next().unwrap_or("unknown");
    let tool = parts.next().unwrap_or("tool");
    let mut summary = format!("MCP {server}/{tool}");
    if let Some(target) = ["path", "file", "query", "url", "name"]
        .iter()
        .find_map(|field| string_field(args, field))
    {
        summary.push_str(" · ");
        summary.push_str(target);
    }
    summary
}

/// Builds a fallback summary for tools without category-specific rendering.
fn unknown_tool_summary(name: &str, arguments: &str) -> String {
    let args = truncate_chars(&compact_inline(arguments), 120);
    if args == "<empty>" {
        name.to_string()
    } else {
        format!("{name} {args}")
    }
}

/// Extracts one string field from JSON arguments.
fn string_field<'a>(args: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    args.get(field).and_then(serde_json::Value::as_str)
}

/// Truncates a string by character count and appends an ASCII ellipsis marker.
fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
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
    fn render_composer_is_borderless() {
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
    fn render_composer_keeps_gap_above_status_bar() {
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

    /// Verifies assistant output uses the same borderless treatment as Codex.
    #[test]
    fn render_transcript_is_borderless() {
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

    /// Verifies reasoning output has a distinct low-emphasis style.
    #[test]
    fn render_reasoning_uses_distinct_style() {
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

    /// Verifies composer height obeys minimum and maximum constraints.
    #[test]
    fn composer_height_with_limits() {
        assert_eq!(composer_height(""), 3);
        assert_eq!(composer_height("a"), 3);
        assert_eq!(composer_height("a\nb\nc\nd\ne\nf\ng"), 6);
        assert_eq!(composer_height("a\nb\nc"), 3);
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

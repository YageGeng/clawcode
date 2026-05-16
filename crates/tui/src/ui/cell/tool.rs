//! Tool-call transcript cells for the local TUI.

use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::terminal_output::terminal_display_lines;

const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;

/// Renderable view of an ACP tool call.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ToolCallCell {
    /// Unique ACP call id for the tool invocation.
    call_id: String,
    /// Tool title shown to the user.
    name: String,
    /// JSON argument text accumulated from ACP raw input.
    arguments: String,
    /// Tool output text accumulated from ACP update content.
    output: String,
    /// Latest ACP execution status for the tool.
    status: ToolCallStatus,
}

impl ToolCallCell {
    /// Creates a pending placeholder used when updates arrive before snapshots.
    pub fn pending(call_id: String) -> Self {
        Self::builder()
            .call_id(call_id)
            .name(String::new())
            .arguments(String::new())
            .output(String::new())
            .status(ToolCallStatus::Pending)
            .build()
    }

    /// Returns the ACP call id.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the display tool name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the accumulated argument text.
    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    /// Returns the accumulated output text.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Returns the latest tool execution status.
    pub fn status(&self) -> ToolCallStatus {
        self.status
    }

    /// Replaces the display tool name.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Replaces the accumulated argument text.
    pub fn set_arguments(&mut self, arguments: impl Into<String>) {
        self.arguments = arguments.into();
    }

    /// Appends tool output received from ACP updates.
    pub fn push_output(&mut self, output: &str) {
        self.output.push_str(output);
    }

    /// Replaces the latest ACP execution status.
    pub fn set_status(&mut self, status: ToolCallStatus) {
        self.status = status;
    }

    /// Returns styled logical lines for this tool-call cell.
    pub fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            status_bullet(self.status),
            " ".into(),
            Span::styled(
                format!("{} {}", status_verb(self.status), self.summary()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        append_tool_output_preview_lines(&mut lines, self.status, &self.output);
        lines
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    pub fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(format!(
            "{} {}",
            status_verb(self.status),
            self.summary()
        ))];
        lines.extend(
            terminal_display_lines(&self.output)
                .into_iter()
                .map(Line::from),
        );
        lines
    }

    /// Builds a concise category-specific title for this tool call.
    fn summary(&self) -> String {
        let args = tool_arguments(&self.arguments);
        match self.name() {
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
            name => unknown_tool_summary(name, self.arguments()),
        }
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

/// Converts multi-line values into compact single-line summaries.
fn compact_inline(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "<empty>".to_string()
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a completed shell tool call fixture.
    fn shell_call(output: impl Into<String>) -> ToolCallCell {
        ToolCallCell::builder()
            .call_id("call-1".to_string())
            .name("shell".to_string())
            .arguments(r#"{"command":"pwd","cwd":"/tmp"}"#.to_string())
            .output(output.into())
            .status(ToolCallStatus::Completed)
            .build()
    }

    /// Verifies tool output previews are capped at five normalized lines.
    #[test]
    fn tool_call_cell_display_lines_preview_first_five_output_lines() {
        let cell = shell_call("one\ntwo\nthree\nfour\nfive\nsix");

        let lines = cell.display_lines(80);

        assert_eq!(lines.len(), 7);
        assert!(lines[1].to_string().contains("one"));
        assert!(lines[5].to_string().contains("five"));
        assert!(lines[6].to_string().contains("+1 lines"));
    }

    /// Verifies completed empty tool calls show the no-output placeholder.
    #[test]
    fn tool_call_cell_display_lines_shows_completed_empty_output() {
        let cell = shell_call("");

        let lines = cell.display_lines(80);

        assert!(lines[1].to_string().contains("(no output)"));
    }
}

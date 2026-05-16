//! Tool-call transcript cells for the local TUI.

use std::path::PathBuf;

use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::terminal_output::terminal_display_lines;

const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;
const TOOL_DIFF_PREVIEW_LINES: usize = 24;
/// Background used for added file lines in structured diff output.
const DIFF_ADDED_BG: Color = Color::Rgb(18, 66, 42);
/// Background used for removed file lines in structured diff output.
const DIFF_REMOVED_BG: Color = Color::Rgb(76, 34, 38);

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
    /// Structured file diffs accumulated from ACP diff content.
    #[builder(default)]
    diffs: Vec<ToolCallDiff>,
    /// Latest ACP execution status for the tool.
    status: ToolCallStatus,
}

/// Final file state rendered as unified diff-style output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDiff {
    /// Path displayed in the diff header.
    path: PathBuf,
    /// File content before the tool ran; `None` means the file was created.
    old_text: Option<String>,
    /// File content after the tool ran.
    new_text: String,
}

impl ToolCallCell {
    /// Creates a pending placeholder used when updates arrive before snapshots.
    pub fn pending(call_id: String) -> Self {
        Self::builder()
            .call_id(call_id)
            .name(String::new())
            .arguments(String::new())
            .output(String::new())
            .diffs(Vec::new())
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

    /// Appends a structured file diff received from ACP updates.
    pub fn push_diff(&mut self, path: PathBuf, old_text: Option<String>, new_text: String) {
        self.diffs.push(ToolCallDiff {
            path,
            old_text,
            new_text,
        });
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
        append_tool_output_preview_lines(
            &mut lines,
            self.status,
            &self.output,
            self.diffs.is_empty(),
        );
        append_tool_diff_preview_lines(&mut lines, &self.diffs);
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
        for diff in &self.diffs {
            lines.extend(diff.raw_lines().into_iter().map(Line::from));
        }
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

impl ToolCallDiff {
    /// Builds plain unified diff-style lines for this file change.
    fn raw_lines(&self) -> Vec<String> {
        let path = self.path.display().to_string();
        let old_line_count = self
            .old_text
            .as_deref()
            .map(|text| split_diff_lines(text).len())
            .unwrap_or(0);
        let new_line_count = split_diff_lines(&self.new_text).len();
        let mut lines = vec![
            format!("--- {path}"),
            format!("+++ {path}"),
            hunk_header(old_line_count, new_line_count),
        ];
        lines.extend(unified_diff_body(
            self.old_text.as_deref().unwrap_or(""),
            &self.new_text,
            self.old_text.is_none(),
        ));
        lines
    }
}

/// Appends the first five display lines from normalized tool output.
fn append_tool_output_preview_lines(
    lines: &mut Vec<Line<'static>>,
    status: ToolCallStatus,
    text: &str,
    show_empty_placeholder: bool,
) {
    let display_lines = terminal_display_lines(text);
    if display_lines.is_empty() {
        if show_empty_placeholder
            && matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
        {
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

/// Appends unified diff-style display lines for structured file diffs.
fn append_tool_diff_preview_lines(lines: &mut Vec<Line<'static>>, diffs: &[ToolCallDiff]) {
    let all_lines = diffs
        .iter()
        .flat_map(ToolCallDiff::raw_lines)
        .collect::<Vec<_>>();
    for (index, line) in all_lines.iter().take(TOOL_DIFF_PREVIEW_LINES).enumerate() {
        let prefix = if index == 0 { "  └ " } else { "    " };
        lines.push(diff_line(prefix, line.to_string(), line));
    }
    if all_lines.len() > TOOL_DIFF_PREVIEW_LINES {
        lines.push(diff_line(
            "    ",
            format!(
                "... +{} diff lines",
                all_lines.len() - TOOL_DIFF_PREVIEW_LINES
            ),
            "",
        ));
    }
}

/// Builds a styled line for one unified diff row.
fn diff_line(prefix: &'static str, display: String, raw: &str) -> Line<'static> {
    let style = if raw.starts_with('+') && !raw.starts_with("+++") {
        Style::default().fg(Color::Green).bg(DIFF_ADDED_BG)
    } else if raw.starts_with('-') && !raw.starts_with("---") {
        Style::default().fg(Color::Red).bg(DIFF_REMOVED_BG)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    // Keep the tree prefix unstyled so diff backgrounds never bleed into normal transcript chrome.
    Line::from(vec![Span::raw(prefix), Span::styled(display, style)])
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

/// Builds a whole-file unified diff hunk header from old/new line counts.
fn hunk_header(old_line_count: usize, new_line_count: usize) -> String {
    let old_start = usize::from(old_line_count > 0);
    let new_start = usize::from(new_line_count > 0);
    format!("@@ -{old_start},{old_line_count} +{new_start},{new_line_count} @@")
}

/// Builds a simple line-based unified diff body from final old/new file states.
fn unified_diff_body(old_text: &str, new_text: &str, is_new_file: bool) -> Vec<String> {
    if is_new_file {
        return split_diff_lines(new_text)
            .into_iter()
            .map(|line| format!("+{line}"))
            .collect();
    }
    let old_lines = split_diff_lines(old_text);
    let new_lines = split_diff_lines(new_text);
    let table = lcs_table(&old_lines, &new_lines);
    collect_diff_lines(&old_lines, &new_lines, &table)
}

/// Splits text into display lines while preserving a visible empty file state.
fn split_diff_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n')
        .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
        .collect()
}

/// Computes the longest-common-subsequence table used by the line diff renderer.
/// SAFETY: loop bounds are derived from the same slices and table dimensions.
#[allow(clippy::indexing_slicing)]
fn lcs_table(old_lines: &[String], new_lines: &[String]) -> Vec<Vec<usize>> {
    let mut table = vec![vec![0; new_lines.len() + 1]; old_lines.len() + 1];
    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            table[i][j] = if old_lines[i] == new_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    table
}

/// Collects context, removed, and added lines from an LCS table.
/// SAFETY: every indexed access is guarded by the current old/new cursor bounds.
#[allow(clippy::indexing_slicing)]
fn collect_diff_lines(
    old_lines: &[String],
    new_lines: &[String],
    table: &[Vec<usize>],
) -> Vec<String> {
    let mut output = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < old_lines.len() || j < new_lines.len() {
        if i < old_lines.len() && j < new_lines.len() && old_lines[i] == new_lines[j] {
            output.push(format!(" {}", old_lines[i]));
            i += 1;
            j += 1;
        } else if i < old_lines.len()
            && (j == new_lines.len() || table[i + 1][j] >= table[i][j + 1])
        {
            output.push(format!("-{}", old_lines[i]));
            i += 1;
        } else if j < new_lines.len() {
            output.push(format!("+{}", new_lines[j]));
            j += 1;
        }
    }
    output
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

    /// Verifies ACP diffs render as unified diff-style lines.
    #[test]
    fn tool_call_cell_display_lines_renders_unified_diff() {
        let mut cell = ToolCallCell::builder()
            .call_id("call-1".to_string())
            .name("apply_patch".to_string())
            .arguments(String::new())
            .output(String::new())
            .status(ToolCallStatus::Completed)
            .build();
        cell.push_diff(
            "src/main.rs".into(),
            Some("fn old() {}\n".to_string()),
            "fn new() {}\n".to_string(),
        );

        let lines = cell.display_lines(80);
        let rendered = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("--- src/main.rs")));
        assert!(rendered.iter().any(|line| line.contains("+++ src/main.rs")));
        assert!(rendered.iter().any(|line| line.contains("@@ -1,1 +1,1 @@")));
        assert!(rendered.iter().any(|line| line.contains("-fn old() {}")));
        assert!(rendered.iter().any(|line| line.contains("+fn new() {}")));
        let removed = rendered
            .iter()
            .position(|line| line.contains("-fn old() {}"))
            .expect("removed line");
        let added = rendered
            .iter()
            .position(|line| line.contains("+fn new() {}"))
            .expect("added line");
        assert!(removed < added);

        let removed_spans = &lines[removed].spans;
        let added_spans = &lines[added].spans;
        let removed_prefix_style = removed_spans.first().expect("removed prefix").style;
        let removed_diff_style = removed_spans.get(1).expect("removed diff").style;
        let added_prefix_style = added_spans.first().expect("added prefix").style;
        let added_diff_style = added_spans.get(1).expect("added diff").style;
        let header_line = lines
            .iter()
            .find(|line| line.to_string().contains("--- src/main.rs"))
            .expect("header line");
        let header_prefix_style = header_line.spans.first().expect("header prefix").style;
        let header_diff_style = header_line.spans.get(1).expect("header diff").style;
        let header_text = lines
            .iter()
            .find(|line| line.to_string().contains("--- src/main.rs"))
            .expect("header line")
            .to_string();
        assert!(removed_prefix_style.bg.is_none());
        assert!(added_prefix_style.bg.is_none());
        assert!(header_prefix_style.bg.is_none());
        assert!(removed_diff_style.bg.is_some());
        assert!(added_diff_style.bg.is_some());
        assert!(header_diff_style.bg.is_none());
        assert_ne!(removed_diff_style.bg, added_diff_style.bg);
        assert_eq!(header_text, "  └ --- src/main.rs");
    }
}

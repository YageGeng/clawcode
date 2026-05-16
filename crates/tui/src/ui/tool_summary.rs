//! Tool-call summary helpers for the local TUI.

use crate::ui::state::ToolCallView;

/// Builds a concise category-specific title for a tool call.
pub(super) fn tool_summary(call: &ToolCallView) -> String {
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

/// Converts multi-line values into compact single-line summaries.
fn compact_inline(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "<empty>".to_string()
    } else {
        compact
    }
}

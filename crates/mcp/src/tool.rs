//! Tool metadata.

/// Tool metadata from `tools/list`.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct McpToolInfo {
    /// The MCP server this tool belongs to.
    pub server_name: String,
    /// Raw tool name as reported by the server.
    pub raw_name: String,
    /// Model-visible callable name (e.g. "mcp__filesystem__read_file").
    pub callable_name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: serde_json::Value,
}

/// Normalize a tool name for model visibility.
///
/// Replaces characters outside `[a-zA-Z0-9_-]` with `_`,
/// then truncates to at most 64 characters.
pub fn normalize_tool_name(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    }
}

/// Model-facing MCP tool metadata returned by remote discovery.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDescriptor {
    /// Remote tool name before session namespacing.
    pub name: String,
    /// Human-readable remote description.
    pub description: String,
    /// Remote JSON input schema.
    pub input_schema: serde_json::Value,
}

/// Normalized result returned by an MCP connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCallResult {
    /// Model-visible output fragments.
    pub content: Vec<String>,
    /// Whether the remote server marked the call as failed.
    pub is_error: bool,
}

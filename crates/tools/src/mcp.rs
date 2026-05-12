//! Reserved trait for future MCP server integration.

use async_trait::async_trait;

/// Reserved trait for future MCP server integration.
#[async_trait]
pub trait McpTool: Send + Sync {
    /// MCP tool name.
    fn name(&self) -> &str;
    /// Execute the MCP tool.
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, String>;
}

/// Placeholder -- does nothing, returns an error indicating MCP is not yet implemented.
pub struct NoopMcp;

#[async_trait]
impl McpTool for NoopMcp {
    fn name(&self) -> &str {
        "noop_mcp"
    }
    async fn execute(&self, _: serde_json::Value) -> Result<String, String> {
        Err("MCP not yet implemented".into())
    }
}

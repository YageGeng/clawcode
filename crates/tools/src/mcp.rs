//! MCP tool handler — adapts MCP tool calls to the [`Tool`] trait.

use std::sync::Arc;

use async_trait::async_trait;
use mcp::{McpConnectionManager, McpToolInfo};

use crate::{Tool, ToolContext};

/// Wraps an MCP tool so it implements the [`Tool`] trait.
///
/// Tool names follow the `mcp__<server>__<tool>` convention.
pub struct McpToolHandler {
    tool_info: McpToolInfo,
    manager: Arc<McpConnectionManager>,
}

impl McpToolHandler {
    pub fn new(tool_info: McpToolInfo, manager: Arc<McpConnectionManager>) -> Self {
        Self { tool_info, manager }
    }
}

#[async_trait]
impl Tool for McpToolHandler {
    fn name(&self) -> &str {
        &self.tool_info.callable_name
    }

    fn description(&self) -> &str {
        &self.tool_info.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.tool_info.input_schema.clone()
    }

    fn needs_approval(&self, _arguments: &serde_json::Value, _ctx: &ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<String, String> {
        self.manager
            .call_tool(
                &self.tool_info.server_name,
                &self.tool_info.raw_name,
                arguments,
            )
            .await
    }
}

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register all tools from an [`McpConnectionManager`] into this registry.
    pub fn register_mcp_tools(&self, manager: Arc<McpConnectionManager>) {
        for tool_info in manager.list_all_tools() {
            let handler = McpToolHandler::new(tool_info, Arc::clone(&manager));
            self.register(Arc::new(handler));
        }
    }
}

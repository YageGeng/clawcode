//! MCP tool handler — adapts MCP tool calls to the [`Tool`] trait.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use mcp::{McpConnectionManager, McpToolInfo};
use serde::Serialize;

use crate::{Tool, ToolContext};

/// MCP approval metadata.
#[derive(Debug, Clone, Serialize)]
struct McpApprovalInvocation {
    /// MCP server name.
    server: String,
    /// MCP tool name.
    tool: String,
    /// Stable hash of arguments.
    arguments_hash: String,
}

impl crate::ToolApprovalInvocation for McpApprovalInvocation {
    /// Return server, tool, and argument hash as the session approval key.
    fn cache_keys(&self, cwd: &Path) -> Vec<crate::ApprovalCacheKey> {
        vec![crate::ApprovalCacheKey::new(
            "mcp",
            cwd.to_path_buf(),
            serde_json::json!({
                "server": self.server.clone(),
                "tool": self.tool.clone(),
                "arguments_hash": self.arguments_hash.clone(),
            }),
        )]
    }
}

/// Wraps an MCP tool so it implements the [`Tool`] trait.
///
/// Tool names follow the `mcp__<server>__<tool>` convention.
pub struct McpToolHandler {
    tool_info: McpToolInfo,
    manager: Arc<McpConnectionManager>,
}

impl McpToolHandler {
    pub fn new(
        tool_info: McpToolInfo,
        manager: Arc<McpConnectionManager>,
    ) -> Self {
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

    fn invocation(
        &self,
        call_id: &str,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<crate::ToolInvocation, crate::ToolInvocationError> {
        let arguments_hash = serde_json::to_string(&arguments)
            .map(|serialized| {
                xxhash_rust::xxh32::xxh32(serialized.as_bytes(), 0).to_string()
            })
            .map_err(|error| {
                crate::ToolInvocationError::InvalidArguments(error.to_string())
            })?;

        Ok(crate::ToolInvocation::builder()
            .call_id(call_id.to_string())
            .tool_name(self.name().to_string())
            .raw_arguments(arguments)
            .cwd(ctx.cwd.clone())
            .approval(Arc::new(McpApprovalInvocation {
                server: self.tool_info.server_name.clone(),
                tool: self.tool_info.raw_name.clone(),
                arguments_hash,
            }))
            .build())
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

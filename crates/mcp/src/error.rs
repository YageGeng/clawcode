//! MCP operation errors.

/// Errors that can occur during MCP operations.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server '{server}' startup failed: {reason}")]
    Startup { server: String, reason: String },

    #[error("server '{server}' tool call '{tool}' timed out ({timeout_secs}s)")]
    ToolTimeout {
        server: String,
        tool: String,
        timeout_secs: u64,
    },

    #[error("server '{server}' not found or not connected")]
    ServerNotFound { server: String },

    #[error("MCP protocol error on '{server}': {msg}")]
    Protocol { server: String, msg: String },

    #[error("transport error: {0}")]
    Transport(String),
}

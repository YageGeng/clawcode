//! MCP client — manages server connections, tool discovery, and tool calls.

pub mod auth;
pub mod error;
pub mod tool;

mod client;
mod manager;
mod transport;

pub use auth::default_auth_dir;
pub use client::Handler;
pub use error::McpError;
pub use manager::McpConnectionManager;
pub use protocol::mcp::{McpOAuthParams, McpServerConfig, McpStartupStatus, McpTransportConfig};
pub use tool::{McpToolInfo, normalize_tool_name};

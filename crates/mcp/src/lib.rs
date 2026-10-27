//! Session-scoped MCP client and tool factory.

mod auth;
mod client;
mod config;
mod error;
mod event;
mod factory;
mod host;
mod resource;
mod server;
mod session;
mod tool;
mod transport;

pub use client::{
    InitializeClient, McpClient, McpProgress, McpProgressSink,
    McpRequestControl,
};
pub use config::{
    McpAuthentication, McpHttpHeaders, McpHttpUrl, McpOAuthSettings,
    McpStdioTransport, McpStreamableHttpTransport, McpTransport,
    RuntimeMcpServer,
};
pub use error::McpError;
pub(crate) use event::McpServerChange;
pub use event::{McpClientEvent, McpSessionEvent};
pub use factory::{McpFactory, SessionMcpFactory};
pub use host::McpHost;
pub use protocol::McpSessionChange;
pub use server::{McpServerRuntime, McpServerStateMachine};
pub use session::{McpSession, McpSessionRequest};
pub use tool::McpAgentTool;
pub use transport::{McpConnector, RmcpConnector};

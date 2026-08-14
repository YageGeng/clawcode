use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpConnection, McpConnector, McpError, McpFactory, RuntimeMcpServer,
    SessionMcpFactory,
};
use protocol::{
    McpCallResult, McpConnectionState, McpToolDescriptor, SessionId, ToolCall,
    ToolCallId, TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{DiscardToolUpdates, ToolExecutionContext};

/// In-memory connector used to test MCP tool registration without a network server.
struct FakeConnector;

/// In-memory MCP connection exposing one echo-like remote tool.
struct FakeConnection;

/// MCP connection whose tool call never settles without a factory timeout.
struct PendingConnection;

#[async_trait]
impl McpConnector for FakeConnector {
    /// Returns one deterministic connection or a server-specific failure.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
    ) -> Result<Arc<dyn McpConnection>, McpError> {
        if server.name == "broken" {
            return Err(McpError::Service("broken server".to_string()));
        }
        if server.name == "pending" {
            return Ok(Arc::new(PendingConnection));
        }
        Ok(Arc::new(FakeConnection))
    }
}

/// Creates one stdio server definition whose transport is unused by the fake connector.
fn runtime_server(name: &str, enabled: bool) -> RuntimeMcpServer {
    RuntimeMcpServer::builder()
        .name(name.to_string())
        .enabled(enabled)
        .external(false)
        .startup_timeout_sec(30)
        .tool_timeout_sec(120)
        .transport(mcp::McpTransport::Stdio {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })
        .oauth(None)
        .build()
}

#[async_trait]
impl McpConnection for FakeConnection {
    /// Lists one remote tool definition.
    async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>, McpError> {
        Ok(vec![McpToolDescriptor {
            name: "remote_echo".to_string(),
            description: "Remote echo".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }])
    }

    /// Returns the supplied arguments as text.
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpCallResult, McpError> {
        Ok(McpCallResult {
            content: vec![format!(
                "{name}:{}",
                arguments.get("text").expect("text argument")
            )],
            is_error: false,
        })
    }
}

#[async_trait]
impl McpConnection for PendingConnection {
    /// Lists one remote tool before its invocation becomes permanently pending.
    async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>, McpError> {
        Ok(vec![McpToolDescriptor {
            name: "pending".to_string(),
            description: "Never settles".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }])
    }

    /// Remains pending to prove configured call timeout enforcement.
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpCallResult, McpError> {
        std::future::pending().await
    }
}

/// Session MCP factory namespaces discovered tools and preserves call correlation.
#[tokio::test]
async fn factory_registers_and_executes_namespaced_remote_tools() {
    let session = SessionMcpFactory::new(
        vec![runtime_server("demo", true)],
        Arc::new(FakeConnector),
    )
    .create()
    .await
    .expect("MCP registry should build");

    assert_eq!(session.tools.names(), vec!["mcp__demo__remote_echo"]);
    let result = session
        .tools
        .execute(
            ToolCall {
                tool_call_id: ToolCallId::try_from("call-1")
                    .expect("valid call id"),
                name: "mcp__demo__remote_echo".to_string(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
            &ToolExecutionContext::builder()
                .session_id(
                    SessionId::try_from("session-1").expect("valid session id"),
                )
                .turn_id(TurnId::try_from("turn-1").expect("valid turn id"))
                .cwd("/workspace".into())
                .cancellation(CancellationToken::new())
                .updates(Arc::new(DiscardToolUpdates))
                .build(),
        )
        .await
        .expect("remote tool should execute");

    assert_eq!(result.tool_call_id.as_str(), "call-1");
    assert_eq!(result.blocks[0].text(), Some("remote_echo:\"hello\""));
}

/// Factory retains connected, failed, and disabled status without losing healthy tools.
#[tokio::test]
async fn factory_keeps_connected_failed_and_disabled_server_statuses() {
    let session = SessionMcpFactory::new(
        vec![
            runtime_server("alpha", true),
            runtime_server("broken", true),
            runtime_server("disabled", false),
        ],
        Arc::new(FakeConnector),
    )
    .create()
    .await
    .expect("session capabilities");

    assert_eq!(session.servers[0].state, McpConnectionState::Connected);
    assert_eq!(session.servers[0].tools[0].name, "mcp__alpha__remote_echo");
    assert_eq!(session.servers[1].state, McpConnectionState::Failed);
    assert!(session.servers[1].error.is_some());
    assert_eq!(session.servers[2].state, McpConnectionState::Disabled);
    assert_eq!(session.tools.names(), vec!["mcp__alpha__remote_echo"]);
}

/// A remote MCP call cannot outlive the server's configured tool timeout.
#[tokio::test]
async fn remote_tool_call_honors_configured_timeout() {
    let mut server = runtime_server("pending", true);
    server.tool_timeout_sec = 1;
    let session = SessionMcpFactory::new(vec![server], Arc::new(FakeConnector))
        .create()
        .await
        .expect("MCP registry should build");
    let context = ToolExecutionContext::builder()
        .session_id(
            SessionId::try_from("session-timeout").expect("valid session id"),
        )
        .turn_id(TurnId::try_from("turn-timeout").expect("valid turn id"))
        .cwd("/workspace".into())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build();
    let execution = session.tools.execute(
        ToolCall {
            tool_call_id: ToolCallId::try_from("call-timeout")
                .expect("valid call id"),
            name: "mcp__pending__pending".to_string(),
            arguments: serde_json::json!({}),
        },
        &context,
    );

    let error = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .expect("configured timeout should settle before guard")
        .expect_err("pending call should time out");
    assert!(error.to_string().contains("timed out after 1 seconds"));
}

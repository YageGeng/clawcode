use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use protocol::{
    ContentBlock, McpConnectionState, McpServerInfo, McpToolDescriptor,
    McpToolInfo, ToolCall, ToolDefinition, ToolResult,
};
use tools::{AgentTool, ToolError, ToolExecutionContext, ToolRegistry};

use crate::{McpConnection, McpConnector, McpError, RuntimeMcpServer};

/// Factory interface used by kernel construction for session-scoped MCP tools.
#[async_trait]
pub trait McpFactory: Send + Sync {
    /// Connects configured servers and returns tools plus immutable status.
    async fn create(&self) -> Result<McpSession, McpError>;
}

/// Session-scoped MCP tool registry and ordered server status snapshot.
pub struct McpSession {
    /// Namespaced tools available to the model for this session.
    pub tools: ToolRegistry,
    /// Connected, failed, and disabled servers in configuration order.
    pub servers: Vec<McpServerInfo>,
}

/// Session-scoped factory backed by one connector implementation.
pub struct SessionMcpFactory {
    servers: Vec<RuntimeMcpServer>,
    connector: Arc<dyn McpConnector>,
}

impl SessionMcpFactory {
    /// Creates an MCP factory from validated servers and a transport connector.
    #[must_use]
    pub fn new(
        servers: Vec<RuntimeMcpServer>,
        connector: Arc<dyn McpConnector>,
    ) -> Self {
        Self { servers, connector }
    }
}

#[async_trait]
impl McpFactory for SessionMcpFactory {
    /// Isolates server failures while preserving registration conflicts as fatal.
    async fn create(&self) -> Result<McpSession, McpError> {
        let mut registry = ToolRegistry::default();
        let mut servers = Vec::with_capacity(self.servers.len());
        for server in &self.servers {
            if !server.enabled {
                servers.push(
                    McpServerInfo::builder()
                        .name(server.name.clone())
                        .state(McpConnectionState::Disabled)
                        .build(),
                );
                continue;
            }
            let connection = match self.connector.connect(server).await {
                Ok(connection) => connection,
                Err(error) => {
                    servers.push(
                        McpServerInfo::builder()
                            .name(server.name.clone())
                            .state(McpConnectionState::Failed)
                            .error(Some(error.to_string()))
                            .build(),
                    );
                    continue;
                }
            };
            let descriptors = match connection.list_tools().await {
                Ok(descriptors) => descriptors,
                Err(error) => {
                    servers.push(
                        McpServerInfo::builder()
                            .name(server.name.clone())
                            .state(McpConnectionState::Failed)
                            .error(Some(error.to_string()))
                            .build(),
                    );
                    continue;
                }
            };
            let mut tools = Vec::with_capacity(descriptors.len());
            for descriptor in descriptors {
                let public_name =
                    format!("mcp__{}__{}", server.name, descriptor.name);
                tools.push(McpToolInfo {
                    name: public_name.clone(),
                    description: (!descriptor.description.is_empty())
                        .then(|| descriptor.description.clone()),
                    input_schema: descriptor.input_schema.clone(),
                });
                registry
                    .register(Arc::new(
                        McpAgentTool::builder()
                            .public_name(public_name)
                            .descriptor(descriptor)
                            .connection(Arc::clone(&connection))
                            .tool_timeout(Duration::from_secs(
                                server.tool_timeout_sec,
                            ))
                            .build(),
                    ))
                    .map_err(|error| {
                        McpError::ToolRegistration(error.to_string())
                    })?;
            }
            servers.push(
                McpServerInfo::builder()
                    .name(server.name.clone())
                    .state(McpConnectionState::Connected)
                    .tools(tools)
                    .build(),
            );
        }
        Ok(McpSession {
            tools: registry,
            servers,
        })
    }
}

#[derive(typed_builder::TypedBuilder)]
struct McpAgentTool {
    public_name: String,
    descriptor: McpToolDescriptor,
    connection: Arc<dyn McpConnection>,
    tool_timeout: Duration,
}

#[async_trait]
impl AgentTool for McpAgentTool {
    /// Returns the namespaced remote MCP tool definition.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.public_name.clone(),
            description: self.descriptor.description.clone(),
            parameters: self.descriptor.input_schema.clone(),
        }
    }

    /// Calls the remote MCP tool and converts its content to model-visible text blocks.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments =
            call.arguments.as_object().cloned().ok_or_else(|| {
                ToolError::InvalidArguments {
                    tool: call.name.clone(),
                    message: "expected a JSON object".to_string(),
                }
            })?;
        let tool_name = call.name.clone();
        let result = tokio::select! {
            () = _context.cancellation.cancelled() => {
                return Err(ToolError::Cancelled);
            }
            result = tokio::time::timeout(
                self.tool_timeout,
                self.connection.call_tool(&self.descriptor.name, arguments),
            ) => {
                match result {
                    Ok(result) => result.map_err(|error| ToolError::Execution {
                        tool: tool_name.clone(),
                        message: error.to_string(),
                    })?,
                    Err(_elapsed) => {
                        return Err(ToolError::Execution {
                            tool: tool_name,
                            message: format!(
                                "MCP tool call timed out after {} seconds",
                                self.tool_timeout.as_secs(),
                            ),
                        });
                    }
                }
            }
        };
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(
                result
                    .content
                    .into_iter()
                    .map(|text| ContentBlock::Text { text })
                    .collect(),
            )
            .is_error(result.is_error)
            .build())
    }
}

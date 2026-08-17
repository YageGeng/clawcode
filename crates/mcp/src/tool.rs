//! Adapter from one discovered MCP Tool to the shared agent Tool contract.

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    ContentBlock, McpRequestContext, McpTaskStatus, McpToolInfo,
    McpToolRequest, TimestampMs, ToolCall, ToolCallId, ToolDefinition,
    ToolResult,
};
use tools::{AgentTool, ToolError, ToolExecutionContext, ToolUpdateSink};

use crate::{McpError, McpProgress, McpProgressSink, McpServerRuntime};

/// Converts normalized MCP progress into replaceable Tool result snapshots.
struct AgentToolProgress {
    tool_call_id: ToolCallId,
    updates: Arc<dyn ToolUpdateSink>,
}

impl McpProgressSink for AgentToolProgress {
    /// Publishes one model-visible latest progress snapshot without losing numeric data.
    fn publish(&self, progress: McpProgress) {
        let summary = progress.message.clone().unwrap_or_else(|| {
            progress.total.map_or_else(
                || format!("MCP progress: {}", progress.progress),
                |total| {
                    format!("MCP progress: {} of {total}", progress.progress)
                },
            )
        });
        self.updates.publish(
            ToolResult::builder()
                .tool_call_id(self.tool_call_id.clone())
                .blocks(vec![
                    ContentBlock::Text { text: summary },
                    ContentBlock::Structured {
                        value: serde_json::json!({
                            "progress": progress.progress,
                            "total": progress.total,
                            "message": progress.message,
                        }),
                    },
                ])
                .is_error(false)
                .build(),
        );
    }

    /// Publishes one replaceable Task lifecycle snapshot with structured state.
    fn publish_task(&self, status: McpTaskStatus) {
        let summary = status.message.clone().unwrap_or_else(|| {
            format!("MCP Task {}: {:?}", status.task_id, status.state)
        });
        self.updates.publish(
            ToolResult::builder()
                .tool_call_id(self.tool_call_id.clone())
                .blocks(vec![
                    ContentBlock::Text { text: summary },
                    ContentBlock::Structured {
                        value: serde_json::to_value(status).unwrap_or_else(
                            |error| {
                                serde_json::json!({
                                    "error": format!(
                                        "failed to serialize MCP Task status: {error}"
                                    )
                                })
                            },
                        ),
                    },
                ])
                .is_error(false)
                .build(),
        );
    }
}

/// Session-scoped adapter retaining structured routing independently from its public name.
pub struct McpAgentTool {
    info: McpToolInfo,
    server: Arc<McpServerRuntime>,
    definition_revision: u64,
}

impl McpAgentTool {
    /// Creates an adapter from one exact immutable catalog revision.
    #[must_use]
    pub fn new(
        info: McpToolInfo,
        server: Arc<McpServerRuntime>,
        definition_revision: u64,
    ) -> Self {
        Self {
            info,
            server,
            definition_revision,
        }
    }

    /// Returns the catalog revision that supplied this immutable definition.
    #[must_use]
    pub fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    /// Validates model arguments as one object against the Server-provided JSON Schema.
    fn validated_arguments(
        &self,
        call: &ToolCall,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ToolError> {
        let arguments =
            call.arguments.as_object().cloned().ok_or_else(|| {
                ToolError::InvalidArguments {
                    tool: self.info.public_name.clone(),
                    message: "MCP Tool arguments must be a JSON object"
                        .to_string(),
                }
            })?;
        let validator = jsonschema::validator_for(&self.info.input_schema)
            .map_err(|error| ToolError::InvalidArguments {
                tool: self.info.public_name.clone(),
                message: format!(
                    "Server supplied an invalid JSON Schema: {error}"
                ),
            })?;
        if let Some(error) = validator
            .iter_errors(&serde_json::Value::Object(arguments.clone()))
            .next()
        {
            return Err(ToolError::InvalidArguments {
                tool: self.info.public_name.clone(),
                message: error.to_string(),
            });
        }
        Ok(arguments)
    }
}

#[async_trait]
impl AgentTool for McpAgentTool {
    /// Projects the exact remote definition under its stable namespaced public name.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.info.public_name.clone(),
            description: self.info.description.clone().unwrap_or_else(|| {
                format!(
                    "MCP Tool '{}' provided by server '{}'",
                    self.info.reference.remote_name,
                    self.info.reference.server_id
                )
            }),
            parameters: self.info.input_schema.clone(),
        }
    }

    /// Validates the stable public name and the complete Server-provided argument schema.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        if call.name != self.info.public_name {
            return Err(ToolError::NotFound(call.name.clone()));
        }
        self.validated_arguments(call).map(|_arguments| ())
    }

    /// Routes one call through its structured reference and preserves rich MCP content.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        if call.name != self.info.public_name {
            return Err(ToolError::NotFound(call.name));
        }
        let arguments = self.validated_arguments(&call)?;
        let result = self
            .server
            .call_tool(
                McpToolRequest {
                    reference: self.info.reference.clone(),
                    arguments,
                },
                McpRequestContext::builder()
                    .server_id(self.info.reference.server_id.clone())
                    .session_id(context.session_id.clone())
                    .turn_id(context.turn_id.clone())
                    .trace_id(context.trace_id.clone())
                    .requested_at_ms(TimestampMs::now())
                    .build(),
                &context.cancellation,
                Arc::new(AgentToolProgress {
                    tool_call_id: call.tool_call_id.clone(),
                    updates: Arc::clone(&context.updates),
                }),
            )
            .await
            .map_err(|error| match error {
                McpError::RequestCancelled(_) => ToolError::Cancelled,
                other => ToolError::Execution {
                    tool: self.info.public_name.clone(),
                    message: other.to_string(),
                },
            })?;
        let mut blocks = result.content;
        if let Some(value) = result.structured_content {
            blocks.push(ContentBlock::Structured { value });
        }
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(blocks)
            .is_error(result.is_error)
            .build())
    }
}

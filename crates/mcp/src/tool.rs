//! Adapter from one discovered MCP Tool to the shared agent Tool contract.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use protocol::{
    ContentBlock, McpRequestContext, McpToolInfo, McpToolRequest, TimestampMs,
    ToolCall, ToolCallId, ToolDefinition, ToolResult,
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
    fn publish(&self, mut progress: McpProgress) {
        if let Some(message) = &mut progress.message {
            // Count both display bytes and JSON escapes; neither view may retain the original tail.
            let mut cost = 0;
            let mut end = 0;
            for character in message.chars() {
                let encoded = if character.is_control() {
                    6
                } else if matches!(character, '"' | '\\') {
                    2
                } else {
                    character.len_utf8()
                };
                cost += character.len_utf8() + encoded;
                if cost > 800 {
                    break;
                }
                end += character.len_utf8();
            }
            if end < message.len() {
                message.truncate(end);
                message.push_str("...");
            }
        }
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
}

/// Session-scoped adapter retaining structured routing independently from its public name.
#[derive(typed_builder::TypedBuilder)]
#[builder(builder_method(vis = ""), builder_type(vis = ""))]
pub struct McpAgentTool {
    info: McpToolInfo,
    server: Arc<McpServerRuntime>,
    definition_revision: u64,
    #[builder(default)]
    validator: OnceLock<Result<jsonschema::Validator, String>>,
}

impl McpAgentTool {
    /// Creates an adapter from one exact immutable catalog revision.
    #[must_use]
    pub fn new(
        info: McpToolInfo,
        server: Arc<McpServerRuntime>,
        definition_revision: u64,
    ) -> Self {
        Self::builder()
            .info(info)
            .server(server)
            .definition_revision(definition_revision)
            .build()
    }

    /// Returns the catalog revision that supplied this immutable definition.
    #[must_use]
    pub fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    /// Borrows validated arguments and lazily compiles this immutable definition once.
    fn validated_arguments<'call>(
        &self,
        call: &'call ToolCall,
    ) -> Result<&'call serde_json::Map<String, serde_json::Value>, ToolError>
    {
        let arguments = call.arguments.as_object().ok_or_else(|| {
            ToolError::InvalidArguments {
                tool: self.info.public_name.clone(),
                message: "MCP Tool arguments must be a JSON object".to_string(),
            }
        })?;
        // Preflight and execution share the compiled schema, including a cached definition error.
        let validator = self
            .validator
            .get_or_init(|| {
                jsonschema::validator_for(&self.info.input_schema).map_err(
                    |error| {
                        format!(
                            "Server supplied an invalid JSON Schema: {error}"
                        )
                    },
                )
            })
            .as_ref()
            .map_err(|message| ToolError::InvalidArguments {
                tool: self.info.public_name.clone(),
                message: message.clone(),
            })?;
        if let Some(error) = validator.iter_errors(&call.arguments).next() {
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

    /// Borrows immutable catalog metadata without copying output schemas.
    fn metadata(&self) -> &protocol::ToolMetadata {
        &self.info.metadata
    }

    /// Validates the stable public name and the complete Server-provided argument schema.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        // Direct adapter callers must observe the same Hidden policy as Kernel declarations.
        if call.name != self.info.public_name
            || self.info.metadata.exposure == protocol::ToolExposure::Hidden
        {
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
        self.validate(&call)?;
        // Validation proved the variant; transfer the owned map instead of cloning it again.
        let serde_json::Value::Object(arguments) = call.arguments else {
            unreachable!("validated MCP arguments must be an object");
        };
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
        // A visible tool on a Hidden server does not implicitly open its resources.
        for block in &mut blocks {
            if let ContentBlock::ResourceLink { server_id, .. } = block {
                *server_id = self
                    .server
                    .allows_resources()
                    .then(|| self.info.reference.server_id.clone());
            }
        }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use protocol::{
        McpServerId, McpToolInfo, McpToolRef, SessionId, ToolCall, ToolCallId,
    };
    use tokio_util::sync::CancellationToken;
    use tools::AgentTool;

    use super::McpAgentTool;
    use crate::{McpServerRuntime, RmcpConnector, RuntimeMcpServer};

    /// Captures the actual text and structured progress snapshots.
    #[derive(Default)]
    struct ProgressCapture(std::sync::Mutex<Vec<protocol::ToolResult>>);

    impl tools::ToolUpdateSink for ProgressCapture {
        /// Retains one published result for projection assertions.
        fn publish(&self, result: protocol::ToolResult) {
            self.0.lock().expect("capture").push(result);
        }
    }

    /// Both progress representations share the one-KiB message budget, including JSON escapes.
    #[test]
    fn progress_text_and_structured_views_are_bounded() {
        use crate::McpProgressSink;
        let capture = Arc::new(ProgressCapture::default());
        let sink = super::AgentToolProgress {
            tool_call_id: ToolCallId::try_from("progress").expect("id"),
            updates: Arc::clone(&capture) as Arc<dyn tools::ToolUpdateSink>,
        };
        for text in ["正文".repeat(1000), "\u{0001}".repeat(2000)] {
            sink.publish(crate::McpProgress {
                progress: 1.0,
                total: Some(2.0),
                message: Some(text),
            });
        }
        for result in capture.0.lock().expect("capture").iter() {
            let mut bytes = Vec::new();
            for block in &result.blocks {
                tools::ToolContentText::try_from(block)
                    .expect("view")
                    .write_to(&mut bytes)
                    .expect("write");
            }
            assert!(bytes.len() <= 1024);
            assert!(result.output.is_none());
        }
    }

    /// Builds the real adapter over a disabled transport so validation needs no external service.
    async fn validation_tool(schema: serde_json::Value) -> McpAgentTool {
        let config = config::McpServerConfig::builder()
            .name("validation".to_string())
            .protocol("2025-11-25".to_string())
            .enabled(false)
            .command("unused".to_string())
            .build();
        let server = McpServerRuntime::bootstrap(
            RuntimeMcpServer::try_from(config).expect("config"),
            Arc::new(RmcpConnector::default()),
            SessionId::try_from("validation").expect("Session id"),
            crate::host::unavailable_host(),
            CancellationToken::new(),
        )
        .await;
        let reference = McpToolRef {
            server_id: McpServerId::try_from("validation").expect("Server id"),
            remote_name: "check".to_string(),
        };
        let info = McpToolInfo::builder()
            .public_name(reference.public_name())
            .reference(reference)
            .input_schema(schema)
            .build();
        McpAgentTool::new(info, server, 1)
    }

    /// Validation borrows its input while still checking every later argument value.
    #[tokio::test]
    async fn validation_borrows_arguments_and_checks_each_call() {
        let tool = validation_tool(serde_json::json!({
            "type": "object", "properties": { "value": { "type": "string" } },
            "required": ["value"]
        }))
        .await;
        let mut call = ToolCall {
            tool_call_id: ToolCallId::try_from("validation-call")
                .expect("Tool call id"),
            name: tool.info.public_name.clone(),
            arguments: serde_json::json!({ "value": "first" }),
        };
        let arguments =
            tool.validated_arguments(&call).expect("valid arguments");
        assert!(
            std::ptr::eq(
                arguments,
                call.arguments.as_object().expect("object")
            ),
            "preflight must not copy the arguments"
        );
        call.arguments = serde_json::json!({ "value": 42 });
        assert!(tool.validate(&call).is_err());
        call.arguments = serde_json::json!({ "value": "next" });
        tool.validate(&call).expect("later valid arguments");
    }

    /// Hidden adapters reject direct callers before compiling a schema or sending a request.
    #[tokio::test]
    async fn hidden_adapter_cannot_be_validated_for_execution() {
        let mut tool =
            validation_tool(serde_json::json!({"type":"object"})).await;
        tool.info.metadata.exposure = protocol::ToolExposure::Hidden;
        let call = ToolCall {
            tool_call_id: ToolCallId::try_from("hidden").expect("id"),
            name: tool.info.public_name.clone(),
            arguments: serde_json::json!({}),
        };
        assert!(matches!(
            tool.validate(&call),
            Err(tools::ToolError::NotFound(_))
        ));
    }

    /// A malformed definition consistently fails without treating a previous error as success.
    #[tokio::test]
    async fn invalid_schema_remains_an_error_on_repeated_calls() {
        let tool =
            validation_tool(serde_json::json!({ "type": "invalid-type" }))
                .await;
        let call = ToolCall {
            tool_call_id: ToolCallId::try_from("invalid-schema-call")
                .expect("Tool call id"),
            name: tool.info.public_name.clone(),
            arguments: serde_json::json!({}),
        };
        for _ in 0..2 {
            assert!(
                matches!(tool.validate(&call), Err(tools::ToolError::InvalidArguments { message, .. }) if message.contains("Server supplied an invalid JSON Schema"))
            );
        }
    }
}

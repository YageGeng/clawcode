use std::sync::Arc;

use async_trait::async_trait;
use tools::{
    Result, ToolCallRequest, ToolContext, ToolInvocation, ToolOutput, handler::ToolHandler,
    registry::ToolRegistryBuilder,
};

/// Verifies router-visible specs are owned by the builder output rather than inferred from handlers.
#[tokio::test]
async fn builder_controls_model_visible_definitions() {
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(VisibleTool));
    builder.register_handler("hidden_alias", Arc::new(HiddenAliasTool));

    let router = builder.build_router();
    let names = router
        .definitions()
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"visible_tool".to_string()));
    assert!(!names.contains(&"hidden_alias".to_string()));
}

/// Verifies handlers registered without a visible spec remain dispatchable by name.
#[tokio::test]
async fn builder_can_register_hidden_dispatch_only_handlers() {
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(VisibleTool));
    builder.register_handler("hidden_alias", Arc::new(HiddenAliasTool));
    let router = builder.build_router();

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-hidden",
                "hidden_alias",
                serde_json::json!({"text": "hidden"}),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "hidden");
}

/// Verifies router dispatch normalizes tool calls into invocation data for handlers.
#[tokio::test]
async fn router_dispatch_passes_invocation_metadata_to_handlers() {
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(InspectingTool));
    let router = builder.build_router();

    let output = router
        .dispatch(
            ToolCallRequest {
                id: "call-visible".to_string(),
                call_id: Some("provider-call-7".to_string()),
                name: "inspecting_tool".to_string(),
                arguments: serde_json::json!({"text": "hello"}),
            },
            ToolContext::new("session-7", "thread-9"),
        )
        .await
        .unwrap();

    assert_eq!(output.structured["call_id"], "provider-call-7");
    assert_eq!(output.structured["tool_name"], "inspecting_tool");
    assert_eq!(output.structured["session_id"], "session-7");
    assert_eq!(output.structured["text"], "hello");
}

/// Simple visible tool used to prove the builder surfaces explicit specs.
struct VisibleTool;

#[async_trait]
impl ToolHandler for VisibleTool {
    fn name(&self) -> &'static str {
        "visible_tool"
    }

    fn description(&self) -> &'static str {
        "Visible builder-managed tool."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string"
                }
            },
            "required": ["text"]
        })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args = invocation
            .function_arguments()
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Ok(ToolOutput::text(
            args["text"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

/// Hidden handler used to prove dispatch can exist without a visible spec.
struct HiddenAliasTool;

#[async_trait]
impl ToolHandler for HiddenAliasTool {
    fn name(&self) -> &'static str {
        "visible_tool"
    }

    fn description(&self) -> &'static str {
        "Alias-only hidden tool."
    }

    fn parameters(&self) -> serde_json::Value {
        VisibleTool.parameters()
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args = invocation
            .function_arguments()
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Ok(ToolOutput::text(
            args["text"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

/// Tool that echoes invocation metadata to prove the router normalized the tool call.
struct InspectingTool;

#[async_trait]
impl ToolHandler for InspectingTool {
    fn name(&self) -> &'static str {
        "inspecting_tool"
    }

    fn description(&self) -> &'static str {
        "Reports invocation metadata for router dispatch tests."
    }

    fn parameters(&self) -> serde_json::Value {
        VisibleTool.parameters()
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(ToolOutput {
            text: text.clone(),
            structured: serde_json::json!({
                "call_id": invocation.effective_call_id(),
                "tool_name": invocation.tool_name,
                "session_id": invocation.context.session_id,
                "text": text,
            }),
        })
    }
}

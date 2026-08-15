use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    ContentBlock, SessionId, ToolCall, ToolCallId, ToolDefinition, ToolResult,
    TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{
    AgentTool, BuiltinToolFactory, DiscardToolUpdates, ToolError,
    ToolExecutionContext, ToolFactory, ToolRegistry,
};

/// Creates one correlated tool call with explicit JSON arguments.
fn tool_call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        tool_call_id: ToolCallId::try_from("tool-call-1")
            .expect("tool call id"),
        name: name.to_string(),
        arguments,
    }
}

/// Returns a direct execution context rooted at the process directory.
fn execution_context() -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-1").expect("session id"))
        .turn_id(TurnId::try_from("turn-1").expect("turn id"))
        .cwd(std::env::current_dir().expect("current directory"))
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build()
}

/// Tool that exposes which immutable registry snapshot executed it.
struct TaggedTool(&'static str);

#[async_trait]
impl AgentTool for TaggedTool {
    /// Returns one stable test-tool definition.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "tagged".to_string(),
            description: "Return the registry snapshot tag".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Returns this tool instance's immutable tag.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: self.0.to_string(),
            }])
            .is_error(false)
            .build())
    }
}

/// Built-in tools reject malformed arguments before side-effectful execution.
#[test]
fn builtins_validate_arguments_during_preflight() {
    let registry = BuiltinToolFactory::new().create().expect("built-in tools");

    assert!(
        registry
            .validate(&tool_call("bash", serde_json::json!({})))
            .is_err()
    );
    assert!(
        registry
            .validate(&tool_call(
                "read",
                serde_json::json!({ "path": "README.md", "offset": "bad" }),
            ))
            .is_err()
    );
}

/// Cloned registries preserve old tool implementations after later upserts.
#[tokio::test]
async fn registry_clones_preserve_immutable_turn_snapshots() {
    let mut initial = ToolRegistry::default();
    initial
        .register(Arc::new(TaggedTool("old")))
        .expect("register old tool");
    let old_snapshot = initial.clone();

    initial.upsert(Arc::new(TaggedTool("new")));
    let new_snapshot = initial;
    let call = tool_call("tagged", serde_json::json!({}));
    let context = execution_context();

    let old = old_snapshot
        .execute(call.clone(), &context)
        .await
        .expect("old execution");
    let new = new_snapshot
        .execute(call.clone(), &context)
        .await
        .expect("new execution");
    assert_eq!(old.blocks[0].text(), Some("old"));
    assert_eq!(new.blocks[0].text(), Some("new"));
}

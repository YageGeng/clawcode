use std::path::PathBuf;
use std::sync::Arc;

use protocol::{SessionId, ToolCall, ToolCallId, TurnId};
use tokio_util::sync::CancellationToken;
use tools::{
    BuiltinToolFactory, DiscardToolUpdates, ToolError, ToolExecutionContext,
    ToolFactory,
};

/// Builds a typed execution context shared by built-in tool tests.
fn execution_context() -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-1").expect("valid session id"))
        .turn_id(TurnId::try_from("turn-1").expect("valid turn id"))
        .cwd(PathBuf::from("/workspace"))
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build()
}

/// Default built-ins expose only pi's coding tools.
#[test]
fn builtin_factory_registers_only_pi_coding_tools() {
    let registry = BuiltinToolFactory::new()
        .create()
        .expect("built-ins should register");

    assert_eq!(registry.names(), vec!["bash", "edit", "read", "write"]);
}

/// Factory switches can disable filesystem and shell groups independently.
#[test]
fn builtin_factory_respects_group_switches() {
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .shell_enabled(false)
        .create()
        .expect("built-ins should register");

    assert!(registry.names().is_empty());
}

/// Registry returns a typed error for unknown tool names.
#[tokio::test]
async fn registry_rejects_unknown_tools() {
    let registry = BuiltinToolFactory::new()
        .create()
        .expect("built-ins should register");
    let error = registry
        .execute(
            ToolCall {
                tool_call_id: ToolCallId::try_from("call-1")
                    .expect("valid tool call id"),
                name: "missing".to_string(),
                arguments: serde_json::json!({}),
            },
            &execution_context(),
        )
        .await
        .expect_err("unknown tool should fail");

    assert_eq!(error, ToolError::NotFound("missing".to_string()));
}

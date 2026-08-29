use std::path::PathBuf;
use std::sync::Arc;

use protocol::{
    SessionId, ToolCall, ToolCallId, ToolPromptContribution, TurnId,
};
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
        .trace_id(
            protocol::TraceId::try_from("trace-1").expect("valid trace id"),
        )
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

    assert_eq!(
        registry.names(),
        vec!["edit", "exec_command", "read", "write", "write_stdin"]
    );
}

/// Built-in tools expose pi's exact System Prompt contributions.
#[test]
fn builtin_tools_expose_pi_prompt_contributions() {
    let registry = BuiltinToolFactory::new()
        .create()
        .expect("built-ins should register");
    let contributions: std::collections::BTreeMap<_, _> = registry
        .prompt_tools()
        .into_iter()
        .map(|tool| (tool.name, tool.contribution))
        .collect();

    assert_eq!(
        contributions.get("read"),
        Some(&ToolPromptContribution {
            snippet: Some("Read file contents".to_string()),
            guidelines: vec![
                "Use read to examine files instead of cat or sed.".to_string()
            ],
        })
    );
    assert_eq!(
        contributions.get("write"),
        Some(&ToolPromptContribution {
            snippet: Some("Create or overwrite files".to_string()),
            guidelines: vec![
                "Use write only for new files or complete rewrites."
                    .to_string()
            ],
        })
    );
    assert_eq!(
        contributions.get("edit"),
        Some(&ToolPromptContribution {
            snippet: Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".to_string()),
            guidelines: vec![
                "Use edit for precise changes (edits[].oldText must match exactly)".to_string(),
                "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".to_string(),
                "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".to_string(),
                "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".to_string(),
            ],
        })
    );
    assert_eq!(
        contributions.get("exec_command"),
        Some(&ToolPromptContribution {
            snippet: Some(
                "Run shell commands with optional PTY support".to_string()
            ),
            guidelines: vec![
                "Use write_stdin with the returned session_id to poll or interact with background commands.".to_string()
            ],
        })
    );
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

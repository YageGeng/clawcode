use std::path::PathBuf;
use std::sync::Arc;

use extension::{
    DiscardExtensionDiagnostics, ExtensionContext, ExtensionFactory,
    ExtensionRegistrar, ExtensionRuntime,
};
use protocol::{
    ExtensionId, ExtensionInvocation, ExtensionSnapshot, LaneId, SessionId,
    SessionTreeSnapshot, ThinkingLevel, TimestampMs, ToolCall, ToolCallEvent,
    ToolCallId, ToolCallResult, UserBashDisposition, UserBashEvent,
};

/// Builds the real compiled module registry used by production sessions.
fn runtime() -> ExtensionRuntime {
    let enabled =
        [ExtensionId::try_from("command-guard").expect("command guard id")];
    let factory = extensions::compiled_extensions()
        .expect("compiled catalogue")
        .select(&enabled)
        .expect("select command guard");
    let mut registrar = ExtensionRegistrar::new();
    for module in factory.create_modules().expect("create modules") {
        registrar
            .register_module(module.as_ref())
            .expect("register module");
    }
    ExtensionRuntime::new(
        Arc::new(registrar.freeze()),
        Arc::new(DiscardExtensionDiagnostics),
    )
}

/// Creates a complete immutable context for tool-policy dispatch.
fn context() -> ExtensionContext {
    let session_id =
        SessionId::try_from("session-guard").expect("session identifier");
    ExtensionContext::builder()
        .invocation(
            ExtensionInvocation::builder()
                .extension_id(ExtensionId::try_from("host").expect("host id"))
                .session_id(session_id.clone())
                .timestamp_ms(TimestampMs::from(1))
                .cwd(PathBuf::from("/tmp"))
                .build(),
        )
        .snapshot(
            ExtensionSnapshot::builder()
                .thinking_level(ThinkingLevel::Off)
                .models(Vec::new())
                .messages(Vec::new())
                .tree(
                    SessionTreeSnapshot::builder()
                        .session_id(session_id)
                        .lane(LaneId::try_from("main").expect("lane id"))
                        .entries(Vec::new())
                        .build(),
                )
                .pending_steer(0)
                .pending_follow_up(0)
                .active_tools(Vec::new())
                .all_tools(vec!["bash".to_string()])
                .cancelled(false)
                .build(),
        )
        .build()
}

/// Creates a bash call with the command under test.
fn bash_call(command: &str) -> ToolCallEvent {
    ToolCallEvent {
        call: ToolCall {
            tool_call_id: ToolCallId::try_from("call-guard")
                .expect("tool call id"),
            name: "bash".to_string(),
            arguments: serde_json::json!({ "command": command }),
        },
    }
}

/// Destructive model-authored bash commands never reach the built-in tool.
#[tokio::test]
async fn command_guard_blocks_destructive_tool_calls() {
    let result = runtime()
        .emit_tool_call(bash_call("rm -rf /tmp/guard-target"), &context())
        .await;

    assert!(matches!(result, ToolCallResult::Block(_)));
}

/// Safe model-authored bash commands continue through normal validation.
#[tokio::test]
async fn command_guard_allows_safe_tool_calls() {
    let result = runtime()
        .emit_tool_call(bash_call("printf safe"), &context())
        .await;

    assert_eq!(result, ToolCallResult::Continue);
}

/// Destructive user-authored bash commands return a policy result without execution.
#[tokio::test]
async fn command_guard_intercepts_destructive_user_bash() {
    let result = runtime()
        .emit_user_bash(
            &UserBashEvent {
                command: "rm -rf /tmp/guard-target".to_string(),
                exclude_from_context: false,
                cwd: PathBuf::from("/tmp"),
            },
            &context(),
        )
        .await
        .expect("guard result");

    assert_eq!(result.exit_code, None);
    assert!(matches!(
        result.disposition,
        UserBashDisposition::Blocked { ref reason }
            if reason.contains("rejected")
    ));
    assert!(result.output.contains("rejected"));
}

/// Guard parsing recognizes direct rm flag variants without scanning harmless arguments.
#[tokio::test]
async fn command_guard_parses_direct_remove_invocations() {
    for command in [
        "rm -rf /tmp/guard-target",
        "rm -fr /tmp/guard-target",
        "rm -r -f /tmp/guard-target",
        "/bin/rm -rf /tmp/guard-target",
    ] {
        assert!(matches!(
            runtime()
                .emit_tool_call(bash_call(command), &context())
                .await,
            ToolCallResult::Block(_)
        ));
    }
    for command in ["echo 'rm -rf /tmp/value'", "rm /tmp/file", "printf safe"] {
        assert_eq!(
            runtime()
                .emit_tool_call(bash_call(command), &context())
                .await,
            ToolCallResult::Continue
        );
    }
}

/// Safe user-authored commands remain owned by the kernel bash executor.
#[tokio::test]
async fn command_guard_allows_safe_user_bash() {
    let result = runtime()
        .emit_user_bash(
            &UserBashEvent {
                command: "printf safe".to_string(),
                exclude_from_context: false,
                cwd: PathBuf::from("/tmp"),
            },
            &context(),
        )
        .await;

    assert_eq!(result, None);
}

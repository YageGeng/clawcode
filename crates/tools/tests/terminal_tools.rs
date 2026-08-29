use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use protocol::{SessionId, ToolCall, ToolCallId, ToolResult, TraceId, TurnId};
use tokio_util::sync::CancellationToken;
use tools::{
    BuiltinToolFactory, DiscardToolUpdates, TerminalError,
    TerminalInteractionOutput, TerminalInteractionRequest,
    TerminalInteractionState, TerminalManager, TerminalService,
    TerminalSpawnRequest, ToolExecutionContext, ToolFactory, ToolUpdateSink,
};

/// Builds one execution context backed by the supplied native terminal manager.
fn execution_context(
    cwd: &Path,
    manager: Arc<TerminalManager>,
) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("tool-terminal").expect("session id"))
        .turn_id(TurnId::try_from("tool-turn").expect("turn id"))
        .trace_id(TraceId::try_from("tool-trace").expect("trace id"))
        .cwd(cwd.to_path_buf())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .terminals(manager as Arc<dyn TerminalService>)
        .build()
}

/// Builds one execution context around an arbitrary terminal service.
fn execution_context_with_service(
    cwd: &Path,
    terminals: Arc<dyn TerminalService>,
) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("tool-terminal").expect("session id"))
        .turn_id(TurnId::try_from("tool-turn").expect("turn id"))
        .trace_id(TraceId::try_from("tool-trace").expect("trace id"))
        .cwd(cwd.to_path_buf())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .terminals(terminals)
        .build()
}

/// Builds one execution context that records partial tool results.
fn execution_context_with_updates(
    cwd: &Path,
    manager: Arc<TerminalManager>,
    updates: Arc<CapturingToolUpdates>,
) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("tool-terminal").expect("session id"))
        .turn_id(TurnId::try_from("tool-turn").expect("turn id"))
        .trace_id(TraceId::try_from("tool-trace").expect("trace id"))
        .cwd(cwd.to_path_buf())
        .cancellation(CancellationToken::new())
        .updates(updates)
        .terminals(manager as Arc<dyn TerminalService>)
        .build()
}

/// Retains partial tool results so integration tests can assert streaming behavior.
#[derive(Default)]
struct CapturingToolUpdates {
    results: Mutex<Vec<ToolResult>>,
}

impl ToolUpdateSink for CapturingToolUpdates {
    /// Records each published replacement snapshot in arrival order.
    fn publish(&self, update: ToolResult) {
        self.results.lock().expect("tool update lock").push(update);
    }
}

/// Captures interaction waits without starting an operating-system process.
#[derive(Default)]
struct RecordingTerminalService {
    yield_durations: Mutex<Vec<Duration>>,
}

/// Returns one deterministic backend failure through the model tool boundary.
struct FailedTerminalService;

#[async_trait]
impl TerminalService for RecordingTerminalService {
    /// Rejects spawn because this fixture only observes write_stdin requests.
    async fn spawn(
        &self,
        _request: TerminalSpawnRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        Err(TerminalError::Unavailable)
    }

    /// Records the effective wait and returns a retained terminal response.
    async fn interact(
        &self,
        request: TerminalInteractionRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        self.yield_durations
            .lock()
            .expect("recording terminal lock")
            .push(request.yield_duration);
        Ok(TerminalInteractionOutput::builder()
            .output(String::new())
            .state(TerminalInteractionState::Running {
                session_id: request.terminal_id,
            })
            .wall_time(Duration::ZERO)
            .build())
    }
}

#[async_trait]
impl TerminalService for FailedTerminalService {
    /// Returns retained output and a backend failure from process creation.
    async fn spawn(
        &self,
        _request: TerminalSpawnRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        Ok(TerminalInteractionOutput::builder()
            .output("partial output".to_string())
            .state(TerminalInteractionState::Failed {
                message: "backend watcher disconnected".to_string(),
            })
            .wall_time(Duration::ZERO)
            .build())
    }

    /// Rejects interaction because this fixture covers process creation only.
    async fn interact(
        &self,
        _request: TerminalInteractionRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        Err(TerminalError::Unavailable)
    }
}

/// Builds one correlated model tool call from JSON arguments.
fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        tool_call_id: ToolCallId::try_from(format!("call-{name}"))
            .expect("tool call id"),
        name: name.to_string(),
        arguments,
    }
}

/// Decodes the single JSON text block returned by terminal tools.
fn response_json(result: &protocol::ToolResult) -> serde_json::Value {
    assert_eq!(result.blocks.len(), 1, "one terminal result block");
    let text = result
        .blocks
        .first()
        .and_then(protocol::ContentBlock::text)
        .expect("terminal text block");
    serde_json::from_str(text).expect("terminal response JSON")
}

/// The built-in shell group exposes only the two Codex-style tools.
#[test]
fn shell_registry_exposes_only_codex_style_tools() {
    let names = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry")
        .names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["exec_command", "write_stdin"]);
}

/// The exec_command schema exposes Codex's workdir name and omits the old alias.
#[test]
fn exec_command_schema_uses_workdir() {
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");
    let definition = registry
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "exec_command")
        .expect("exec_command definition");
    assert!(definition.parameters["properties"].get("workdir").is_some());
    assert!(definition.parameters["properties"].get("cwd").is_none());
}

/// The tool adapters carry one real PTY from creation through interactive exit.
#[tokio::test]
async fn exec_then_write_stdin_completes_interactive_pty() {
    let root = tempfile::tempdir().expect("temporary directory");
    let manager = Arc::new(TerminalManager::new(
        SessionId::try_from("tool-terminal").expect("session id"),
    ));
    let context = execution_context(root.path(), Arc::clone(&manager));
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");
    let started = registry
        .execute(
            call(
                "exec_command",
                serde_json::json!({
                    "cmd": "python3 -u -c 'print(input(\"prompt: \"))'",
                    "tty": true,
                    "yield\x2dtime_ms": 250
                }),
            ),
            &context,
        )
        .await
        .expect("execute PTY command");
    let started = response_json(&started);
    assert!(
        started["output"]
            .as_str()
            .is_some_and(|text| text.contains("prompt: "))
    );
    let terminal_id = started["session_id"].as_u64().expect("session id");
    let completed = registry
        .execute(
            call(
                "write_stdin",
                serde_json::json!({
                    "session_id": terminal_id,
                    "chars": "answer\n",
                    "yield\x2dtime_ms": 1000
                }),
            ),
            &context,
        )
        .await
        .expect("write PTY input");
    let completed = response_json(&completed);
    assert!(
        completed["output"]
            .as_str()
            .is_some_and(|text| text.contains("answer"))
    );
    assert_eq!(completed["exit_code"], 0);
}

/// Shell output is published before completion without changing the final result.
#[tokio::test]
async fn exec_command_streams_output_before_returning_the_final_result() {
    let root = tempfile::tempdir().expect("temporary directory");
    let manager = Arc::new(TerminalManager::new(
        SessionId::try_from("tool-terminal").expect("session id"),
    ));
    let updates = Arc::new(CapturingToolUpdates::default());
    let context = execution_context_with_updates(
        root.path(),
        manager,
        Arc::clone(&updates),
    );
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");

    let result = registry
        .execute(
            call(
                "exec_command",
                serde_json::json!({
                    "cmd": "printf first; sleep 0.2; printf second",
                    "yield\x2dtime_ms": 1000
                }),
            ),
            &context,
        )
        .await
        .expect("execute streamed command");
    let snapshots = updates
        .results
        .lock()
        .expect("tool update lock")
        .iter()
        .map(response_json)
        .collect::<Vec<_>>();

    assert!(snapshots.iter().any(|snapshot| {
        snapshot["output"] == "first" && snapshot.get("exit_code").is_none()
    }));
    assert_eq!(response_json(&result)["output"], "firstsecond");
}

/// Interactive writes publish new output before the retained process exits.
#[tokio::test]
async fn write_stdin_streams_output_before_returning_the_final_result() {
    let root = tempfile::tempdir().expect("temporary directory");
    let manager = Arc::new(TerminalManager::new(
        SessionId::try_from("tool-terminal").expect("session id"),
    ));
    let updates = Arc::new(CapturingToolUpdates::default());
    let context = execution_context_with_updates(
        root.path(),
        manager,
        Arc::clone(&updates),
    );
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");
    let started = registry
        .execute(
            call(
                "exec_command",
                serde_json::json!({
                    "cmd": "python3 -u -c 'import time; print(input(\"prompt: \")); time.sleep(0.2); print(\"done\")'",
                    "tty": true,
                    "yield\x2dtime_ms": 250
                }),
            ),
            &context,
        )
        .await
        .expect("start interactive command");
    let terminal_id = response_json(&started)["session_id"]
        .as_u64()
        .expect("session id");
    updates.results.lock().expect("tool update lock").clear();

    let result = registry
        .execute(
            call(
                "write_stdin",
                serde_json::json!({
                    "session_id": terminal_id,
                    "chars": "answer\n",
                    "yield\x2dtime_ms": 1000
                }),
            ),
            &context,
        )
        .await
        .expect("complete interactive command");
    let snapshots = updates
        .results
        .lock()
        .expect("tool update lock")
        .iter()
        .map(response_json)
        .collect::<Vec<_>>();

    assert!(snapshots.iter().any(|snapshot| {
        snapshot["output"].as_str().is_some_and(|output| {
            output.contains("answer") && !output.contains("done")
        })
    }));
    assert!(
        response_json(&result)["output"]
            .as_str()
            .is_some_and(|output| output.contains("done"))
    );
}

/// Invalid wait and output bounds are rejected before a process is spawned.
#[tokio::test]
async fn exec_command_validates_bounded_arguments() {
    let root = tempfile::tempdir().expect("temporary directory");
    let manager = Arc::new(TerminalManager::new(
        SessionId::try_from("tool-terminal").expect("session id"),
    ));
    let context = execution_context(root.path(), manager);
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");
    let error = registry
        .execute(
            call(
                "exec_command",
                serde_json::json!({
                    "cmd": ":",
                    "yield\x2dtime_ms": 31_000,
                    "max_output_tokens": 0
                }),
            ),
            &context,
        )
        .await
        .expect_err("invalid bounds");
    assert!(
        error.to_string().contains("yield-time_ms"),
        "unexpected validation error: {error}"
    );
}

/// Backend failures remain visible as failed tool results with partial output.
#[tokio::test]
async fn exec_command_marks_backend_failure_as_tool_error() {
    let root = tempfile::tempdir().expect("temporary directory");
    let context = execution_context_with_service(
        root.path(),
        Arc::new(FailedTerminalService),
    );
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");

    let result = registry
        .execute(
            call("exec_command", serde_json::json!({ "cmd": "ignored" })),
            &context,
        )
        .await
        .expect("structured failed tool result");
    let response = response_json(&result);

    assert!(result.is_error);
    assert_eq!(response["output"], "partial output");
    assert_eq!(response["error"], "backend watcher disconnected");
    assert!(response.get("session_id").is_none());
}

/// Empty polling accepts 300 seconds while writes retain the 30 second ceiling.
#[tokio::test]
async fn write_stdin_uses_contextual_wait_limits() {
    let root = tempfile::tempdir().expect("temporary directory");
    let service = Arc::new(RecordingTerminalService::default());
    let context = execution_context_with_service(
        root.path(),
        Arc::clone(&service) as Arc<dyn TerminalService>,
    );
    let registry = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry");
    let definition = registry
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "write_stdin")
        .expect("write_stdin definition");
    assert_eq!(
        definition.parameters["properties"]["yield-time_ms"]["maximum"],
        300_000
    );

    registry
        .execute(
            call(
                "write_stdin",
                serde_json::json!({
                    "session_id": 7319,
                    "yield-time_ms": 300_000
                }),
            ),
            &context,
        )
        .await
        .expect("long poll");
    assert_eq!(
        service
            .yield_durations
            .lock()
            .expect("recorded waits")
            .as_slice(),
        &[Duration::from_millis(300_000)]
    );

    let error = registry
        .execute(
            call(
                "write_stdin",
                serde_json::json!({
                    "session_id": 7319,
                    "chars": "x",
                    "yield\x2dtime_ms": 300_000
                }),
            ),
            &context,
        )
        .await
        .expect_err("write wait above 30 seconds");
    assert!(error.to_string().contains("30000"));
}

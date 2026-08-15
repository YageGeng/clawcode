use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    ContextPoint, ExtensionContext, ExtensionDiagnosticSink, ExtensionError,
    ExtensionHandler, ExtensionModule, ExtensionRegistrar, ExtensionRuntime,
    InputPoint, NON_UI_EXTENSION_POINT_NAMES, SessionBeforeSwitchPoint,
    ToolCallPoint, ToolExecutionUpdatePoint,
};
use protocol::{
    AgentMessage, ContentBlock, ContextEvent, ContextResult,
    ExtensionDescriptor, ExtensionId, ExtensionInvocation, ExtensionSnapshot,
    InputEvent, InputResult, InputSource, LaneId, MessageContent, MessageId,
    MessageIdentity, MessageTiming, ModelRequest, SessionBeforeSwitchEvent,
    SessionCancelResult, SessionId, SessionSwitchReason, SessionTreeSnapshot,
    ThinkingLevel, TimestampMs, ToolBlock, ToolCall, ToolCallEvent, ToolCallId,
    ToolCallResult, ToolExecutionUpdateEvent, ToolResult, TurnId,
};

/// Creates one complete user message for context-chain assertions.
fn user_message(id: &str, text: &str) -> AgentMessage {
    let timestamp = TimestampMs::from(1);
    AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from(id).expect("message id"),
            turn_id: TurnId::try_from("turn-1").expect("turn id"),
        },
        timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
            .expect("message timing"),
        content: MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        },
    }
}

/// Creates an immutable handler context without requiring a kernel host.
fn extension_context() -> ExtensionContext {
    let session_id = SessionId::try_from("session-1").expect("session id");
    ExtensionContext::builder()
        .invocation(
            ExtensionInvocation::builder()
                .extension_id(ExtensionId::try_from("host").expect("host id"))
                .session_id(session_id.clone())
                .turn_id(Some(TurnId::try_from("turn-1").expect("turn id")))
                .timestamp_ms(TimestampMs::from(1))
                .cwd(PathBuf::from("/workspace"))
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
                .all_tools(Vec::new())
                .cancelled(false)
                .build(),
        )
        .build()
}

/// Appends one user message to the current model-context candidate.
struct AppendContext {
    id: &'static str,
    text: &'static str,
}

#[async_trait]
impl ExtensionHandler<ContextPoint> for AppendContext {
    /// Returns a replacement containing the prior chain value and one new message.
    async fn handle(
        &self,
        event: &ContextEvent,
        _context: &ExtensionContext,
    ) -> Result<ContextResult, ExtensionError> {
        let mut messages = event.request.messages.clone();
        messages.push(user_message(self.id, self.text));
        Ok(ContextResult {
            messages: Some(messages),
        })
    }
}

/// Input handler with a configured result and observable call count.
struct InputHandler {
    result: InputResult,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ExtensionHandler<InputPoint> for InputHandler {
    /// Records the invocation and returns the configured input decision.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

/// Cancels a pending session switch.
struct CancelSwitch;

#[async_trait]
impl ExtensionHandler<SessionBeforeSwitchPoint> for CancelSwitch {
    /// Returns the first cancellation decision.
    async fn handle(
        &self,
        _event: &SessionBeforeSwitchEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionCancelResult, ExtensionError> {
        Ok(SessionCancelResult { cancel: true })
    }
}

/// Tool preflight handler that fails instead of returning a block explicitly.
struct FailingToolHandler;

#[async_trait]
impl ExtensionHandler<ToolCallPoint> for FailingToolHandler {
    /// Simulates an extension bug at the fail-safe tool boundary.
    async fn handle(
        &self,
        _event: &ToolCallEvent,
        _context: &ExtensionContext,
    ) -> Result<ToolCallResult, ExtensionError> {
        Err(ExtensionError::Handler("broken policy".to_string()))
    }
}

/// Module registering all handlers needed by the composition tests.
struct RuntimeModule {
    first_input_calls: Arc<AtomicUsize>,
    skipped_input_calls: Arc<AtomicUsize>,
}

impl ExtensionModule for RuntimeModule {
    /// Returns stable metadata for diagnostic source attribution.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("runtime").expect("extension id"),
            name: "Runtime".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers chain, short-circuit, cancellation, and fail-safe handlers.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ContextPoint, _>(AppendContext {
            id: "message-a",
            text: "a",
        })?;
        registrar.on::<ContextPoint, _>(AppendContext {
            id: "message-b",
            text: "b",
        })?;
        registrar.on::<InputPoint, _>(InputHandler {
            result: InputResult::Transform {
                text: "transformed".to_string(),
            },
            calls: Arc::clone(&self.first_input_calls),
        })?;
        registrar.on::<InputPoint, _>(InputHandler {
            result: InputResult::Handled,
            calls: Arc::clone(&self.first_input_calls),
        })?;
        registrar.on::<InputPoint, _>(InputHandler {
            result: InputResult::Continue,
            calls: Arc::clone(&self.skipped_input_calls),
        })?;
        registrar.on::<SessionBeforeSwitchPoint, _>(CancelSwitch)?;
        registrar.on::<ToolCallPoint, _>(FailingToolHandler)
    }
}

/// Diagnostic sink that records sanitized handler failures.
#[derive(Default)]
struct RecordingDiagnostics {
    failures: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl ExtensionDiagnosticSink for RecordingDiagnostics {
    /// Records one failure for deterministic assertions.
    async fn report(
        &self,
        extension_id: &ExtensionId,
        point: &'static str,
        message: &str,
    ) {
        self.failures.lock().expect("diagnostic mutex").push((
            extension_id.to_string(),
            point.to_string(),
            message.to_string(),
        ));
    }
}

/// Runtime composition chains values and stops only on domain terminal decisions.
#[tokio::test]
async fn runtime_applies_typed_composition_rules() {
    let first_input_calls = Arc::new(AtomicUsize::new(0));
    let skipped_input_calls = Arc::new(AtomicUsize::new(0));
    let mut registrar = ExtensionRegistrar::new();
    registrar
        .register_module(&RuntimeModule {
            first_input_calls: Arc::clone(&first_input_calls),
            skipped_input_calls: Arc::clone(&skipped_input_calls),
        })
        .expect("register runtime module");
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    let runtime = ExtensionRuntime::new(
        Arc::new(registrar.freeze()),
        diagnostics.clone(),
    );
    let context = extension_context();

    let context_result = runtime
        .emit_context(
            ContextEvent {
                request: ModelRequest {
                    messages: vec![user_message("message-initial", "initial")],
                    tools: Vec::new(),
                },
            },
            &context,
        )
        .await;
    assert_eq!(
        context_result.messages.expect("replacement context").len(),
        3
    );

    let input_result = runtime
        .emit_input(
            InputEvent {
                text: "original".to_string(),
                source: InputSource::Interactive,
                streaming_behavior: None,
            },
            &context,
        )
        .await;
    assert_eq!(input_result, InputResult::Handled);
    assert_eq!(first_input_calls.load(Ordering::SeqCst), 2);
    assert_eq!(skipped_input_calls.load(Ordering::SeqCst), 0);

    let cancellation = runtime
        .emit_session_before_switch(
            &SessionBeforeSwitchEvent {
                reason: SessionSwitchReason::New,
                target_session_id: None,
            },
            &context,
        )
        .await;
    assert!(cancellation.cancel);

    let tool_result = runtime
        .emit_tool_call(
            ToolCallEvent {
                call: ToolCall {
                    tool_call_id: ToolCallId::try_from("tool-call-1")
                        .expect("tool call id"),
                    name: "read".to_string(),
                    arguments: serde_json::json!({ "path": "README.md" }),
                },
            },
            &context,
        )
        .await;
    assert!(matches!(
        tool_result,
        ToolCallResult::Block(ToolBlock { .. })
    ));
    assert_eq!(
        diagnostics.failures.lock().expect("diagnostic mutex").len(),
        1
    );
}

/// Update handler that synchronizes concurrent tool lifecycles at one barrier.
struct ConcurrentUpdate {
    barrier: Arc<tokio::sync::Barrier>,
    entered: Arc<AtomicUsize>,
}

#[async_trait]
impl ExtensionHandler<ToolExecutionUpdatePoint> for ConcurrentUpdate {
    /// Waits until two independent tool updates enter the same shared handler.
    async fn handle(
        &self,
        _event: &ToolExecutionUpdateEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.barrier.wait().await;
        Ok(())
    }
}

/// Module that registers one concurrency-observable tool update handler.
struct ConcurrentModule {
    barrier: Arc<tokio::sync::Barrier>,
    entered: Arc<AtomicUsize>,
}

impl ExtensionModule for ConcurrentModule {
    /// Returns stable metadata for the concurrent handler.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("concurrent").expect("extension id"),
            name: "Concurrent".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the shared handler without adding a runtime-wide mutex.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ToolExecutionUpdatePoint, _>(ConcurrentUpdate {
            barrier: Arc::clone(&self.barrier),
            entered: Arc::clone(&self.entered),
        })
    }
}

/// Independent tool update lifecycles can enter handlers concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_does_not_serialize_parallel_tool_updates_globally() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let entered = Arc::new(AtomicUsize::new(0));
    let mut registrar = ExtensionRegistrar::new();
    registrar
        .register_module(&ConcurrentModule {
            barrier,
            entered: Arc::clone(&entered),
        })
        .expect("register concurrent module");
    let runtime = ExtensionRuntime::new(
        Arc::new(registrar.freeze()),
        Arc::new(RecordingDiagnostics::default()),
    );
    let context = extension_context();
    let call = ToolCall {
        tool_call_id: ToolCallId::try_from("tool-call-concurrent")
            .expect("tool call id"),
        name: "read".to_string(),
        arguments: serde_json::json!({ "path": "README.md" }),
    };
    let event = ToolExecutionUpdateEvent {
        call,
        partial_result: ToolResult::builder()
            .tool_call_id(
                ToolCallId::try_from("tool-call-concurrent")
                    .expect("tool call id"),
            )
            .blocks(Vec::new())
            .is_error(false)
            .build(),
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(
            runtime.emit_tool_execution_update(&event, &context),
            runtime.emit_tool_execution_update(&event, &context),
        );
    })
    .await
    .expect("updates should enter concurrently");
    assert_eq!(entered.load(Ordering::SeqCst), 2);
}

/// The public point catalog exactly matches Pi's current non-UI extension surface.
#[test]
fn public_point_catalog_contains_only_pi_non_ui_hooks() {
    assert_eq!(
        NON_UI_EXTENSION_POINT_NAMES,
        [
            "project_trust",
            "resources_discover",
            "session_start",
            "session_info_changed",
            "session_before_switch",
            "session_before_fork",
            "session_before_compact",
            "session_compact",
            "session_before_tree",
            "session_tree",
            "session_shutdown",
            "input",
            "before_agent_start",
            "agent_start",
            "agent_end",
            "agent_settled",
            "turn_start",
            "turn_end",
            "message_start",
            "message_update",
            "message_end",
            "context",
            "before_provider_request",
            "before_provider_headers",
            "after_provider_response",
            "model_select",
            "thinking_level_select",
            "tool_execution_start",
            "tool_execution_update",
            "tool_execution_end",
            "tool_call",
            "tool_result",
            "user_bash",
        ]
    );
}

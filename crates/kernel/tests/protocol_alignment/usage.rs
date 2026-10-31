use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, MessageEndPoint, StaticExtensionFactory,
};
use kernel::{ModelError, SessionEventSinks};
use protocol::{
    AgentEventPayload, AgentMessage, AgentOutcome, ExtensionDescriptor,
    ExtensionId, MessageContent, MessageEndEvent, MessageId, ModelFinal,
    ModelRequest, ModelStreamEvent, ModelUsage, RunRequest, SessionId,
    StaticExtensionRegistration, StopReason, ToolCall, ToolCallId,
};

use super::fixture::{Route, TestKernel, text};

/// Builds a terminal event that reports `total` tokens.
fn finished_with(stop_reason: StopReason, total: u64) -> ModelStreamEvent {
    ModelStreamEvent::Finished(
        ModelFinal::builder()
            .stop_reason(stop_reason)
            .raw_stop_reason(None)
            .usage(protocol::ModelUsage::from(
                protocol::Usage::new()
                    .input_tokens(total)
                    .output_tokens(0)
                    .cached_input_tokens(0)
                    .cache_creation_input_tokens(0)
                    .total_tokens(total),
            ))
            .build(),
    )
}

/// First request calls bash with `command`; every later request answers. Each attempt costs 15.
fn tool_then_answer(command: &'static str) -> impl Fn(&ModelRequest) -> Route {
    move |request| {
        if request
            .messages
            .last()
            .is_some_and(|message| text(&message.content) == "use tool")
        {
            Route::Events(vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("bash-call").expect("id"),
                        )
                        .name("bash".to_string())
                        .arguments(serde_json::json!({ "command": command }))
                        .build(),
                ),
                finished_with(StopReason::ToolUse, 15),
            ])
        } else {
            Route::Events(vec![
                ModelStreamEvent::TextDelta("done".into()),
                finished_with(StopReason::EndTurn, 15),
            ])
        }
    }
}

/// Runs one prompt to settlement and returns the settled usage.
async fn settled_usage(test: &TestKernel, session: &SessionId) -> ModelUsage {
    test.sinks
        .payloads(session)
        .into_iter()
        .find_map(|payload| match payload {
            AgentEventPayload::AgentSettled { usage, .. } => Some(usage),
            _ => None,
        })
        .expect("settled usage")
}

/// Starts "use tool" without waiting for the Run.
fn start_tool_run(
    test: &TestKernel,
    session: &SessionId,
) -> tokio::task::JoinHandle<Result<protocol::RunResult, kernel::KernelError>> {
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(session);
    let run_session = session.clone();
    tokio::spawn(async move {
        kernel
            .run(
                RunRequest {
                    session_id: run_session,
                    input: "use tool".into(),
                },
                sink,
            )
            .await
    })
}

/// Idle usage sums every settled assistant attempt of the Run.
#[tokio::test]
async fn agent_settled_usage_sums_run_assistants() {
    let test = TestKernel::new(tool_then_answer("true"));
    let session = test.create("owner").await;
    start_tool_run(&test, &session)
        .await
        .expect("join")
        .expect("run");
    assert_eq!(
        settled_usage(&test, &session).await.as_ref().total_tokens,
        Some(30)
    );
}

/// Known usage before cancellation is retained without another accumulation pass.
#[tokio::test]
async fn cancelled_run_preserves_known_usage() {
    let test = TestKernel::new(tool_then_answer("sleep 5"));
    let session = test.create("owner").await;
    let run = start_tool_run(&test, &session);
    test.wait_until(|| {
        test.sinks.saw(&session, |event| {
            matches!(event, AgentEventPayload::ToolExecutionStart { .. })
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    test.kernel.cancel_session(&session).expect("cancel");
    run.await.expect("join").expect("cancelled run settles");
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Cancelled,
            ..
        }
    )));
    assert_eq!(
        settled_usage(&test, &session).await.as_ref().total_tokens,
        Some(15)
    );
}

/// Replaces an assistant message's identity, which the Kernel rejects as a protocol error.
#[derive(Clone)]
struct BreakAssistantIdentity;

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for BreakAssistantIdentity {
    /// Returns the assistant under a new id so validation fails after the attempt settled.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        if !matches!(
            event.message.content,
            MessageContent::Assistant {
                model_message: None,
                ..
            }
        ) {
            return Ok(None);
        }
        let mut message = event.message.clone();
        message.identity.message_id =
            MessageId::try_from("message-replaced").expect("id");
        Ok(Some(message))
    }
}

/// Registers the identity-breaking handler.
struct BreakModule;

impl ExtensionModule for BreakModule {
    /// Identifies the identity-breaking test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("identity-breaker").expect("id"),
            name: "Identity breaker".into(),
            version: "1".into(),
        }
    }

    /// Registers the message_end replacement.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<MessageEndPoint, _>(BreakAssistantIdentity)
    }
}

/// Settled usage survives an internal error after a model has already consumed tokens.
#[tokio::test]
async fn failed_run_preserves_usage_from_settled_assistant() {
    let test = TestKernel::with_extensions(
        |_| {
            Route::Events(vec![
                ModelStreamEvent::TextDelta("answer".into()),
                finished_with(StopReason::EndTurn, 15),
            ])
        },
        Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(BreakModule) as Arc<dyn ExtensionModule>)
            })],
        )),
    );
    let session = test.create("owner").await;
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect_err("identity check fails the Run");
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Failed { .. },
            ..
        }
    )));
    assert_eq!(
        settled_usage(&test, &session).await.as_ref().total_tokens,
        Some(15)
    );
}

/// No model usage yields zero tokens and no synthesized reasoning count.
#[tokio::test]
async fn preflight_failure_has_zero_usage_without_reasoning_tokens() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    *test.model.preflight_failure.lock().expect("preflight") =
        Some(|| ModelError::Unavailable("offline".into()));
    let session = test.create("owner").await;
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect_err("preflight failure");
    let usage = settled_usage(&test, &session).await;
    assert_eq!(usage.as_ref().total_tokens, None);
    assert_eq!(usage.as_ref().reasoning_tokens, None);
}

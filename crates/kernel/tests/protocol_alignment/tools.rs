use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, MessageEndPoint, StaticExtensionFactory,
};
use kernel::SessionEventSinks;
use protocol::{
    AgentEventPayload, AgentMessage, ContentBlock, ExtensionDescriptor,
    ExtensionId, MessageContent, MessageEndEvent, ModelRequest,
    ModelStreamEvent, RunRequest, SessionId, StaticExtensionRegistration,
    StopReason, ToolCall, ToolCallId,
};

use super::fixture::{Route, TestKernel, finished, text};

/// Routes the first prompt to one long bash call and later requests to an answer.
fn long_tool(request: &ModelRequest) -> Route {
    if request
        .messages
        .last()
        .is_some_and(|message| text(&message.content) == "run tool")
    {
        Route::Events(vec![
            ModelStreamEvent::ToolCall(
                ToolCall::builder()
                    .tool_call_id(
                        ToolCallId::try_from("bash-call").expect("id"),
                    )
                    .name("bash".to_string())
                    .arguments(serde_json::json!({ "command": "sleep 5" }))
                    .build(),
            ),
            finished(StopReason::ToolUse),
        ])
    } else {
        Route::answer("done")
    }
}

/// Starts the tool Run, cancels it mid-call, and returns its Session.
async fn cancel_running_tool(test: &TestKernel) -> SessionId {
    let session = test.create("owner").await;
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(&session);
    let run_session = session.clone();
    let run = tokio::spawn(async move {
        kernel
            .run(
                RunRequest {
                    session_id: run_session,
                    input: "run tool".into(),
                },
                sink,
            )
            .await
    });
    test.wait_until(|| {
        test.sinks.saw(&session, |event| {
            matches!(event, AgentEventPayload::ToolExecutionStart { .. })
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    test.kernel.cancel_session(&session).expect("cancel");
    run.await.expect("join").expect("cancelled run settles");
    session
}

/// Returns whether the persisted transcript holds a cancelled tool result.
fn persisted_cancelled(test: &TestKernel, session: &SessionId) -> bool {
    test.kernel
        .session_transcript(session)
        .expect("transcript")
        .iter()
        .any(|message| {
            matches!(
                message.content,
                MessageContent::ToolResult {
                    cancelled: true,
                    ..
                }
            )
        })
}

/// A tool interrupted by cancel is marked cancelled from its end event to storage.
#[tokio::test]
async fn cancelled_turn_marks_tool_results_cancelled() {
    let test = TestKernel::new(long_tool);
    let session = cancel_running_tool(&test).await;
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::ToolExecutionEnd { result, .. } if result.cancelled
    )));
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::MessageEnd { message }
            if matches!(message.content, MessageContent::ToolResult { cancelled: true, .. })
    )));
    assert!(persisted_cancelled(&test, &session));
}

/// Replaces every tool result with fresh content that omits the cancellation flag.
#[derive(Clone)]
struct ReplaceToolResult;

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for ReplaceToolResult {
    /// Rebuilds the tool result content the way an extension rewriting output would.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        let MessageContent::ToolResult {
            tool_call_id,
            is_error,
            details,
            output,
            ..
        } = &event.message.content
        else {
            return Ok(None);
        };
        Ok(Some(AgentMessage {
            content: MessageContent::ToolResult {
                tool_call_id: tool_call_id.clone(),
                blocks: vec![ContentBlock::Text {
                    text: "rewritten".to_string(),
                }],
                is_error: *is_error,
                details: details.clone(),
                output: output.clone(),
                cancelled: false,
            },
            ..event.message.clone()
        }))
    }
}

/// Registers the rewriting message_end handler.
struct ReplaceModule;

impl ExtensionModule for ReplaceModule {
    /// Identifies the rewriting test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("tool-rewriter").expect("id"),
            name: "Tool rewriter".into(),
            version: "1".into(),
        }
    }

    /// Registers the message_end replacement.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<MessageEndPoint, _>(ReplaceToolResult)
    }
}

/// An extension replacing visible tool content cannot erase cancellation.
#[tokio::test]
async fn tool_message_end_extension_preserves_cancelled_flag() {
    let test = TestKernel::with_extensions(
        long_tool,
        Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(ReplaceModule) as Arc<dyn ExtensionModule>)
            })],
        )),
    );
    let session = cancel_running_tool(&test).await;
    assert!(persisted_cancelled(&test, &session));
}

/// A tool start carries the file location resolved exactly as the tool resolves it.
#[tokio::test]
async fn tool_start_reports_resolved_location() {
    let test = TestKernel::new(|request| {
        if request
            .messages
            .last()
            .is_some_and(|message| text(&message.content) == "read page")
        {
            Route::Events(vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("read-call").expect("id"),
                        )
                        .name("read".to_string())
                        .arguments(serde_json::json!({ "path": "@page.txt" }))
                        .build(),
                ),
                finished(StopReason::ToolUse),
            ])
        } else {
            Route::answer("done")
        }
    });
    let session = test.create("owner").await;
    let cwd = test.root.path().to_path_buf();
    std::fs::write(cwd.join("page.txt"), "page").expect("page");
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "read page".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");
    let locations = test
        .sinks
        .payloads(&session)
        .into_iter()
        .find_map(|payload| match payload {
            AgentEventPayload::ToolExecutionStart { locations, .. } => {
                Some(locations)
            }
            _ => None,
        })
        .expect("tool start");
    assert_eq!(
        locations,
        vec![protocol::ToolLocation {
            path: cwd.join("page.txt"),
            line: None,
        }]
    );
}

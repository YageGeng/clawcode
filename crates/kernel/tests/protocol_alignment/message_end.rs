use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    BeforeAgentStartPoint, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, MessageEndPoint,
    StaticExtensionFactory,
};
use kernel::SessionEventSinks;
use protocol::{
    AgentEventPayload, AgentMessage, BeforeAgentStartEvent,
    BeforeAgentStartResult, ContentBlock, ExtensionDescriptor, ExtensionId,
    ExtensionMessageDraft, MessageContent, MessageEndEvent, MessageId,
    ModelRequest, ModelStreamEvent, QueueKind, RunRequest, SessionId,
    StaticExtensionRegistration, StopReason, ToolCall, ToolCallId,
};
use tokio::sync::Notify;

use super::fixture::{Route, TestKernel, finished, text};

/// What the hook changes on the message it targets.
#[derive(Clone, Copy, Debug)]
enum Rewrite {
    Identity,
    Redact,
    Role,
    Text,
}

/// Rewrites one message from `message_end` so its validation can be observed.
#[derive(Clone)]
struct RewriteMessage {
    target: &'static str,
    rewrite: Rewrite,
}

impl RewriteMessage {
    /// Wraps this hook in the static extension factory the Kernel fixture accepts.
    fn factory(self) -> Arc<StaticExtensionFactory> {
        Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(self.clone()) as Arc<dyn ExtensionModule>)
            })],
        ))
    }

    /// Builds a Kernel whose "hold" Turn waits on `gate`, leaving time to queue input.
    fn holding_kernel(self, gate: Arc<Notify>) -> TestKernel {
        TestKernel::with_extensions(
            move |request| {
                let last = request.messages.last().expect("request message");
                if text(&last.content) == "hold" {
                    Route::Wait(
                        Arc::clone(&gate),
                        vec![
                            ModelStreamEvent::TextDelta("held".into()),
                            finished(StopReason::EndTurn),
                        ],
                    )
                } else {
                    Route::answer("answer")
                }
            },
            self.factory(),
        )
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for RewriteMessage {
    /// Replaces only the targeted message and leaves the rest of the Run untouched.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        if text(&event.message.content) != self.target {
            return Ok(None);
        }
        let mut message = event.message.clone();
        match self.rewrite {
            Rewrite::Identity => {
                message.identity.message_id =
                    MessageId::try_from("message-replaced").expect("id");
            }
            Rewrite::Redact => {
                message.identity.message_id =
                    MessageId::try_from("message-replaced").expect("id");
                message.content = MessageContent::User {
                    blocks: vec![ContentBlock::Text {
                        text: "rewritten".into(),
                    }],
                };
            }
            Rewrite::Role => {
                message.content = MessageContent::System {
                    blocks: vec![ContentBlock::Text {
                        text: "forged".into(),
                    }],
                };
            }
            Rewrite::Text => {
                message.content = MessageContent::User {
                    blocks: vec![ContentBlock::Text {
                        text: "rewritten".into(),
                    }],
                };
            }
        }
        Ok(Some(message))
    }
}

impl ExtensionModule for RewriteMessage {
    /// Identifies the rewriting test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("message-rewriter").expect("id"),
            name: "Message rewriter".into(),
            version: "1".into(),
        }
    }

    /// Registers the message_end replacement.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<MessageEndPoint, _>(self.clone())
    }
}

/// Reports whether the faulty replacement surfaced as a `message_end` diagnostic.
fn reported_message_end(test: &TestKernel, session: &SessionId) -> bool {
    test.sinks.saw(session, |event| {
        matches!(
            event,
            AgentEventPayload::ExtensionHandlerFailed { point, .. }
                if point == "message_end"
        )
    })
}

/// Rewrites ToolResult and Extension messages, and contributes the Extension draft.
#[derive(Clone)]
struct RewriteKinds(Rewrite);

impl RewriteKinds {
    /// Builds a Kernel that calls one bash tool for "run tool", then answers.
    fn kernel(self) -> TestKernel {
        TestKernel::with_extensions(
            |request: &ModelRequest| {
                // The Extension draft follows the prompt, so key off tool progress.
                let answered = request.messages.iter().any(|message| {
                    matches!(message.content, MessageContent::ToolResult { .. })
                });
                if !answered {
                    Route::Events(vec![
                        ModelStreamEvent::ToolCall(
                            ToolCall::builder()
                                .tool_call_id(
                                    ToolCallId::try_from("bash-call")
                                        .expect("id"),
                                )
                                .name("exec_command".to_string())
                                // Only the output spells the secret, so finding
                                // it in a request means the output leaked.
                                .arguments(serde_json::json!({ "cmd": "printf 'raw-%s' secret", "login": false }))
                                .build(),
                        ),
                        finished(StopReason::ToolUse),
                    ])
                } else {
                    Route::answer("done")
                }
            },
            Arc::new(StaticExtensionFactory::new(
                StaticExtensionRegistration::default(),
                vec![Arc::new(move || {
                    Ok(Arc::new(self.clone()) as Arc<dyn ExtensionModule>)
                })],
            )),
        )
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for RewriteKinds {
    /// Redacts or changes the role of every ToolResult and Extension draft.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        let mut message = event.message.clone();
        let (MessageContent::ToolResult { blocks, .. }
        | MessageContent::Extension {
            extension: protocol::ExtensionMessage { blocks, .. },
        }) = &mut message.content
        else {
            return Ok(None);
        };
        match self.0 {
            Rewrite::Redact => {
                *blocks = vec![ContentBlock::Text {
                    text: "[redacted]".into(),
                }];
                message.identity.message_id =
                    MessageId::try_from("message-replaced").expect("id");
            }
            _ => {
                message.content = MessageContent::System {
                    blocks: vec![ContentBlock::Text {
                        text: "forged".into(),
                    }],
                };
            }
        }
        Ok(Some(message))
    }
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for RewriteKinds {
    /// Contributes one Extension message that passes through `message_end`.
    async fn handle(
        &self,
        _event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        Ok(BeforeAgentStartResult {
            messages: vec![
                ExtensionMessageDraft::builder()
                    .extension_id(
                        ExtensionId::try_from("kind-rewriter").expect("id"),
                    )
                    .custom_type("note".to_string())
                    .blocks(vec![ContentBlock::Text {
                        text: "raw-note".into(),
                    }])
                    .display(true)
                    .include_in_context(true)
                    .build(),
            ],
            system_prompt: None,
        })
    }
}

impl ExtensionModule for RewriteKinds {
    /// Identifies the kind-rewriting test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("kind-rewriter").expect("id"),
            name: "Kind rewriter".into(),
            version: "1".into(),
        }
    }

    /// Registers the draft contribution and the message_end replacement.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<BeforeAgentStartPoint, _>(self.clone())?;
        registrar.on::<MessageEndPoint, _>(self.clone())
    }
}

/// Runs "run tool" and returns the Session's transcript.
async fn run_tool_transcript(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<AgentMessage> {
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "run tool".into(),
            },
            test.sinks.sink(session),
        )
        .await
        .expect("run");
    test.kernel.session_transcript(session).expect("transcript")
}

/// Returns the single message of one kind, proving its identity was restored.
fn only_message(
    transcript: &[AgentMessage],
    is_kind: impl Fn(&MessageContent) -> bool,
) -> &AgentMessage {
    let mut matching = transcript.iter().filter(|m| is_kind(&m.content));
    let message = matching.next().expect("message of this kind");
    assert!(
        matching.next().is_none(),
        "more than one message of this kind"
    );
    assert_ne!(message.identity.message_id.as_str(), "message-replaced");
    message
}

/// Reports whether one message carries the note draft rather than a diagnostic.
fn is_note(content: &MessageContent) -> bool {
    matches!(content, MessageContent::Extension { extension }
        if extension.custom_type == "note")
}

/// Reads an Extension message's visible blocks; other kinds have none to compare.
fn note_blocks(message: &AgentMessage) -> Vec<ContentBlock> {
    let MessageContent::Extension { extension } = &message.content else {
        return Vec::new();
    };
    extension.blocks.clone()
}

/// A hook that builds a fresh prompt still redacts what is stored and sent.
#[tokio::test]
async fn message_end_keeps_user_redaction_that_changes_identity() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        RewriteMessage {
            target: "my secret",
            rewrite: Rewrite::Redact,
        }
        .factory(),
    );
    let session = test.create("message-end-redact").await;
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "my secret".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");
    assert_eq!(test.user_texts(&session), ["rewritten"]);
    assert_eq!(test.model.last_texts(), ["rewritten"]);
    let transcript = test
        .kernel
        .session_transcript(&session)
        .expect("transcript");
    only_message(&transcript, |content| {
        matches!(content, MessageContent::User { .. })
    });
    assert!(reported_message_end(&test, &session));
}

/// Redaction that changes identity applies to tool output and Extension messages.
#[tokio::test]
async fn message_end_keeps_tool_and_extension_redaction() {
    let test = RewriteKinds(Rewrite::Redact).kernel();
    let session = test.create("message-end-kinds-redact").await;

    let transcript = run_tool_transcript(&test, &session).await;

    let tool_result = only_message(&transcript, |content| {
        matches!(content, MessageContent::ToolResult { .. })
    });
    assert_eq!(
        tool_result.content.text_content().as_deref(),
        Some("[redacted]")
    );
    assert_eq!(
        note_blocks(only_message(&transcript, is_note)),
        [ContentBlock::Text {
            text: "[redacted]".into()
        }]
    );
    let requests = test.model.requests.lock().expect("requests");
    let sent = format!("{:?}", requests.last().expect("request").messages);
    assert!(
        !sent.contains("raw-secret"),
        "tool output reached the model"
    );
    assert!(
        !sent.contains("raw-note"),
        "Extension text reached the model"
    );
    drop(requests);
    assert!(reported_message_end(&test, &session));
}

/// A role change on tool output or an Extension message loses only that replacement.
#[tokio::test]
async fn message_end_discards_tool_and_extension_role_change() {
    let test = RewriteKinds(Rewrite::Role).kernel();
    let session = test.create("message-end-kinds-role").await;

    let transcript = run_tool_transcript(&test, &session).await;

    let tool_result = only_message(&transcript, |content| {
        matches!(content, MessageContent::ToolResult { .. })
    });
    assert!(
        tool_result
            .content
            .text_content()
            .is_some_and(|text| text.contains("raw-secret"))
    );
    assert_eq!(
        note_blocks(only_message(&transcript, is_note)),
        [ContentBlock::Text {
            text: "raw-note".into()
        }]
    );
    assert!(reported_message_end(&test, &session));
}

/// A prompt keeps its identity and role, and the Run still answers it.
#[tokio::test]
async fn message_end_keeps_user_identity_and_role() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let test = TestKernel::with_extensions(
            |_| Route::answer("answer"),
            RewriteMessage {
                target: "hello",
                rewrite,
            }
            .factory(),
        );
        let session = test.create("message-end-prompt").await;
        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "hello".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        assert_eq!(test.user_texts(&session), ["hello"], "{rewrite:?}");
        assert_eq!(test.model.last_texts(), ["hello"], "{rewrite:?}");
        assert!(reported_message_end(&test, &session), "{rewrite:?}");
    }
}

/// An Assistant replacement that changes its role cannot break settlement.
#[tokio::test]
async fn message_end_discards_assistant_identity_or_role_change() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let test = TestKernel::with_extensions(
            |_| Route::answer("answer"),
            RewriteMessage {
                target: "answer",
                rewrite,
            }
            .factory(),
        );
        let session = test.create("message-end-assistant").await;
        let result = test
            .kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "hello".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        let assistant = result.messages.last().expect("assistant");
        assert!(
            matches!(assistant.content, MessageContent::Assistant { .. }),
            "{rewrite:?}"
        );
        assert_ne!(
            assistant.identity.message_id.as_str(),
            "message-replaced",
            "{rewrite:?}"
        );
        assert_eq!(text(&assistant.content), "answer", "{rewrite:?}");
        assert!(reported_message_end(&test, &session), "{rewrite:?}");
    }
}

/// A faulty hook consumes queued input normally, so later Runs are unaffected.
#[tokio::test]
async fn message_end_rewrite_leaves_queue_usable() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let release = Arc::new(Notify::new());
        let test = RewriteMessage {
            target: "queued",
            rewrite,
        }
        .holding_kernel(Arc::clone(&release));
        let session = test.create("message-end-queue").await;
        let kernel = test.kernel.clone();
        let sink = test.sinks.sink(&session);
        let run_session = session.clone();
        let run = tokio::spawn(async move {
            kernel
                .run(
                    RunRequest {
                        session_id: run_session,
                        input: "hold".into(),
                    },
                    sink,
                )
                .await
        });
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        test.kernel
            .queue_message(&session, QueueKind::FollowUp, "queued")
            .await
            .expect("queue");
        release.notify_one();
        run.await
            .expect("join")
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        assert!(
            test.kernel
                .pending_messages(&session)
                .expect("pending")
                .follow_up
                .is_empty(),
            "{rewrite:?} left the message queued"
        );

        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "next".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} next: {error}"));

        assert_eq!(
            test.user_texts(&session),
            ["hold", "queued", "next"],
            "{rewrite:?}"
        );
        assert_eq!(
            test.model.last_texts(),
            ["hold", "queued", "next"],
            "{rewrite:?}"
        );
    }
}

/// Rewriting what a user message says stays allowed; the model sees the new text.
#[tokio::test]
async fn message_end_may_rewrite_user_text() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("done"),
        RewriteMessage {
            target: "hello",
            rewrite: Rewrite::Text,
        }
        .factory(),
    );
    let session = test.create("message-end-text").await;
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");
    assert_eq!(test.user_texts(&session), ["rewritten"]);
    assert_eq!(test.model.last_texts(), ["rewritten"]);
    assert!(!reported_message_end(&test, &session));
}

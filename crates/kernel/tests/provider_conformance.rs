use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, MessageEndPoint, StaticExtensionFactory,
};
use protocol::{
    AgentMessage, ContentBlock, ExtensionDescriptor, ExtensionId,
    MessageContent, MessageEndEvent, RunRequest, SessionId, ToolCall,
    ToolDefinition, ToolResult,
};
use rig_core::{
    http_client::{
        self,
        middleware::{Exchange, HttpMiddleware},
    },
    wasm_compat::WasmBoxedFuture,
};
use serde_json::{Value, json};
use tools::{AgentTool, ToolError, ToolExecutionContext};

#[path = "support/provider_server.rs"]
mod provider_server;
use provider_server::Server;

#[derive(Clone, Copy, Default)]
enum Edit {
    #[default]
    None,
    Content,
    Arguments,
    RemoveCalls,
    Stop(protocol::StopReason),
    Revive,
}

#[derive(Clone, Default)]
struct Audit {
    counts: Arc<[AtomicUsize; 2]>,
    messages: Arc<Mutex<Vec<AgentMessage>>>,
    edit: Edit,
}

impl Audit {
    /// Installs one request probe, one message editor, and one observable no-argument tool.
    fn extensions(&self) -> StaticExtensionFactory {
        let audit = self.clone();
        StaticExtensionFactory::new(
            Default::default(),
            vec![Arc::new(move || Ok(Arc::new(audit.clone())))],
        )
    }

    /// Runs the actual Kernel with local HTTP and a disposable production store.
    async fn run(
        &self,
        server: &Server,
        retry: config::RetryConfig,
    ) -> protocol::RunResult {
        let directory = tempfile::tempdir().expect("isolated store");
        let kernel = server.kernel(directory.path(), retry, self.extensions());
        let session = SessionId::try_from("audit").expect("session ID");
        kernel
            .create_session(store::SessionCreateOptions {
                session_id: session.clone(),
                cwd: directory.path().into(),
                parent_session_id: None,
            })
            .await
            .expect("create session");
        let (sender, _receiver) = tokio::sync::mpsc::channel(1024);
        kernel
            .run(
                RunRequest {
                    session_id: session,
                    input: "test".into(),
                },
                Arc::new(kernel::ChannelEventSink::new(sender)),
            )
            .await
            .expect("settled run")
    }
}

impl ExtensionModule for Audit {
    /// Provides a stable synthetic module identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("audit").expect("extension ID"),
            name: "audit".into(),
            version: "1".into(),
        }
    }

    /// Registers observers through production extension APIs.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<MessageEndPoint, _>(self.clone())?;
        registrar.register_tool(Arc::new(self.clone()))?;
        let counts = Arc::clone(&self.counts);
        registrar.register_http_middleware(move |context| RequestAudit {
            context,
            counts: Arc::clone(&counts),
        })
    }
}

struct RequestAudit {
    context: ExtensionContext,
    counts: Arc<[AtomicUsize; 2]>,
}

impl HttpMiddleware for RequestAudit {
    /// Counts real attempts and verifies the per-request session context.
    fn before_request_headers<'a>(
        &'a self,
        _: Exchange<'a>,
        _: &'a mut http::HeaderMap,
    ) -> WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            assert_eq!(self.context.invocation.session_id.as_str(), "audit");
            self.counts
                .first()
                .expect("request counter")
                .fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for Audit {
    /// Observes corrected final content before optionally removing reasoning and changing text.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        self.messages
            .lock()
            .expect("messages")
            .push(event.message.clone());
        if matches!(self.edit, Edit::None) {
            return Ok(None);
        }
        let mut message = event.message.clone();
        if let MessageContent::Assistant {
            blocks, metadata, ..
        } = &mut message.content
        {
            match self.edit {
                Edit::Content => {
                    blocks.retain(|block| {
                        !matches!(block, ContentBlock::Reasoning { .. })
                    });
                    for block in blocks {
                        if let ContentBlock::Text { text } = block {
                            *text = "Hi".into();
                        }
                    }
                }
                Edit::Arguments => {
                    for block in blocks {
                        if let ContentBlock::ToolCall { arguments, .. } = block
                        {
                            *arguments = json!({"value":"edited"});
                        }
                    }
                }
                Edit::RemoveCalls => {
                    blocks.retain(|block| {
                        !matches!(block, ContentBlock::ToolCall { .. })
                    });
                    metadata.stop_reason = protocol::StopReason::EndTurn;
                }
                Edit::Stop(reason) => metadata.stop_reason = reason,
                Edit::Revive => {
                    metadata.stop_reason = protocol::StopReason::ToolUse;
                    metadata.error = None;
                    blocks.push(ContentBlock::ToolCall {
                        tool_call_id: protocol::ToolCallId::try_from("revived")
                            .expect("call ID"),
                        name: "counted".into(),
                        arguments: json!({}),
                        invalid_arguments: None,
                    });
                }
                Edit::None => {}
            }
        }
        Ok(Some(message))
    }
}

#[async_trait]
impl AgentTool for Audit {
    /// Deliberately accepts empty arguments so invalid JSON cannot hide behind schema rejection.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "counted".into(),
            description: "count execution".into(),
            parameters: json!({"type":"object"}),
        }
    }

    /// Records every execution before returning a harmless result.
    async fn execute(
        &self,
        call: ToolCall,
        _: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        self.counts
            .get(1)
            .expect("tool counter")
            .fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: call.arguments.to_string(),
            }])
            .is_error(false)
            .build())
    }
}

/// Encodes a terminal Responses frame without replacing the production model implementation.
fn response(output: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{"id":"r","object":"response","status":"completed","model":"audit","output":output}})
    )
}

/// Builds the ordinary message item shared by scripted terminal Responses frames.
fn text_item(text: &str) -> Value {
    json!({"type":"message","id":"msg","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]})
}

/// One Kernel budget governs actual HTTP attempts and reruns native middleware each time.
#[tokio::test]
async fn retries_and_middleware_have_one_matching_attempt_count() {
    for (enabled, override_, expected) in [
        (true, Some(2), 3),
        (true, None, 6),
        (true, Some(0), 1),
        (false, Some(2), 1),
    ] {
        let server=Server::start(vec![(429,json!({"error":{"type":"rate_limit_error","message":"synthetic"}}).to_string());6]).await;
        let mut retry = config::RetryConfig::default();
        retry.agent.enabled = enabled;
        retry.agent.max_retries = 5;
        retry.agent.base_delay_ms = 0;
        retry.provider.max_retries = override_;
        let audit = Audit::default();
        audit.run(&server, retry).await;
        assert_eq!(server.requests().len(), expected);
        assert_eq!(audit.counts[0].load(Ordering::SeqCst), expected);
    }
}

/// Malformed arguments and refused turns cannot execute even a tool that accepts empty JSON.
#[tokio::test]
async fn malformed_and_refused_calls_do_not_execute() {
    for refusal in [false, true] {
        let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"counted","arguments":if refusal {"{}"} else {"{"},"status":"completed"});
        let mut output = vec![call];
        if refusal {
            output.push(json!({"type":"message","id":"refusal","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"}]}));
        }
        // A structured provider refusal/error terminates the turn even after a complete call.
        let body = if refusal {
            format!(
                "data: {}\n\n",
                json!({"type":"response.failed","response":{"id":"r","status":"failed","model":"audit","output":output,"error":{"code":"refusal","message":"declined"}}})
            )
        } else {
            response(json!(output))
        };
        let server = Server::start(vec![
            (200, body),
            (200, response(json!([text_item("done")]))),
        ])
        .await;
        let audit = Audit::default();
        let messages = audit.run(&server, Default::default()).await.messages;
        assert_eq!(
            audit.counts[1].load(Ordering::SeqCst),
            0,
            "refusal={refusal}; messages={messages:?}"
        );
        if !refusal {
            assert!(messages.iter().any(|message| matches!(
                &message.content,
                MessageContent::ToolResult { is_error: true, .. }
            )));
            assert_eq!(server.requests().len(), 2);
        } else {
            assert_eq!(server.requests().len(), 1);
        }
    }
}

/// A final correction precedes hooks; edited text and removed reasoning invalidate native replay.
#[tokio::test]
async fn corrected_message_reaches_hook_and_edits_are_authoritative() {
    let mut body = format!(
        "data: {}\n\n",
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"Bye"})
    );
    body.push_str(&response(json!([
        {"type":"reasoning","id":"reason","summary":[{"type":"summary_text","text":"Plan B"}],"encrypted_content":"cipher"},
        text_item("Hello"),
        {"type":"function_call","id":"fc","call_id":"call","name":"counted","arguments":"{}","status":"completed"}
    ])));
    let server = Server::start(vec![
        (200, body),
        (200, response(json!([text_item("done")]))),
    ])
    .await;
    let audit = Audit {
        edit: Edit::Content,
        ..Default::default()
    };
    let messages = audit.run(&server, Default::default()).await.messages;
    let observed = audit.messages.lock().expect("messages");
    let first = observed
        .iter()
        .find(|message| {
            matches!(message.content, MessageContent::Assistant { .. })
        })
        .expect("assistant hook message");
    assert!(
        matches!(&first.content,MessageContent::Assistant {blocks,model_message:Some(_),..} if blocks.iter().any(|block|matches!(block,ContentBlock::Text {text} if text=="Hello"))),
        "{first:?}"
    );
    assert!(messages.iter().filter(|message|matches!(message.content,MessageContent::Assistant {..})).all(|message|matches!(&message.content,MessageContent::Assistant {blocks,model_message:None,..} if !blocks.iter().any(|block|matches!(block,ContentBlock::Reasoning {..})) && blocks.iter().any(|block|matches!(block,ContentBlock::Text {text} if text=="Hi")))));
    let next = server.requests()[1]["input"].to_string();
    assert!(next.contains("Hi"));
    assert!(
        !next.contains("cipher")
            && !next.contains("Plan B")
            && !next.contains("Hello")
    );
}

/// Execution and replay must use the tool arguments that survived MessageEnd.
#[tokio::test]
async fn message_end_tool_arguments_control_execution_and_history() {
    let server=Server::start(vec![(200,response(json!([{"type":"function_call","id":"fc","call_id":"call","name":"counted","arguments":"{\"value\":\"original\"}","status":"completed"}]))),(200,response(json!([text_item("done")]))) ]).await;
    let audit = Audit {
        edit: Edit::Arguments,
        ..Default::default()
    };
    let result = audit.run(&server, Default::default()).await;
    let executed = result
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::ToolResult { blocks, .. } => {
                blocks.first().and_then(ContentBlock::text)
            }
            _ => None,
        })
        .expect("tool result");
    assert_eq!(
        serde_json::from_str::<Value>(executed).expect("executed arguments"),
        json!({"value":"edited"})
    );
    assert_eq!(audit.counts[1].load(Ordering::SeqCst), 1);
    let next = server.requests()[1]["input"].to_string();
    assert!(next.contains("edited") && !next.contains("original"));
}

/// Removing a call and finishing the message cannot leave a stale executable call behind.
#[tokio::test]
async fn message_end_removal_prevents_tool_execution() {
    let server=Server::start(vec![(200,response(json!([text_item("answer"),{"type":"function_call","id":"fc","call_id":"call","name":"counted","arguments":"{}","status":"completed"}]))),(200,response(json!([text_item("done")]))) ]).await;
    let audit = Audit {
        edit: Edit::RemoveCalls,
        ..Default::default()
    };
    let result = audit.run(&server, Default::default()).await;
    assert_eq!(audit.counts[1].load(Ordering::SeqCst), 0);
    assert_eq!(server.requests().len(), 1);
    assert!(matches!(
        result.turns.last().expect("turn").outcome,
        protocol::TurnOutcome::Completed {
            stop_reason: protocol::StopReason::EndTurn
        }
    ));
}

/// Hook terminal outcomes suppress tools, while genuine provider failures cannot be revived.
#[tokio::test]
async fn message_end_terminal_outcomes_cannot_execute_calls() {
    for reason in [
        protocol::StopReason::Error,
        protocol::StopReason::Refusal,
        protocol::StopReason::Cancelled,
    ] {
        let server=Server::start(vec![(200,response(json!([{"type":"function_call","id":"fc","call_id":"call","name":"counted","arguments":"{}","status":"completed"}]))),(200,response(json!([text_item("done")]))) ]).await;
        let audit = Audit {
            edit: Edit::Stop(reason),
            ..Default::default()
        };
        let result = audit.run(&server, Default::default()).await;
        assert_eq!(audit.counts[1].load(Ordering::SeqCst), 0, "{reason:?}");
        assert_eq!(server.requests().len(), 1);
        let outcome = &result.turns.last().expect("turn").outcome;
        if reason == protocol::StopReason::Cancelled {
            assert!(matches!(outcome, protocol::TurnOutcome::Cancelled));
        } else {
            assert!(matches!(outcome, protocol::TurnOutcome::Failed { .. }));
        }
    }
    let body = format!(
        "data: {}\n\n",
        json!({"type":"response.failed","response":{"id":"r","status":"failed","model":"audit","output":[],"error":{"code":"refusal","message":"declined"}}})
    );
    let server = Server::start(vec![(200, body)]).await;
    let audit = Audit {
        edit: Edit::Revive,
        ..Default::default()
    };
    let result = audit.run(&server, Default::default()).await;
    assert_eq!(audit.counts[1].load(Ordering::SeqCst), 0);
    assert!(matches!(
        result.turns.last().expect("turn").outcome,
        protocol::TurnOutcome::Failed { .. }
    ));
    assert!(result.messages.iter().any(|message| matches!(&message.content,MessageContent::Assistant{metadata,..} if metadata.stop_reason==protocol::StopReason::Error)));
}

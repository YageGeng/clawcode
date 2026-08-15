use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionContext,
    ExtensionError, ExtensionHost, ExtensionHostError, RegisteredCommand,
    RuntimeGeneration,
};
use protocol::{
    AgentMessage, ExtensionCommandDefinition, ExtensionId, ExtensionInvocation,
    ExtensionMessageDraft, ExtensionSnapshot, LaneId, MessageContent,
    MessageId, MessageIdentity, MessageTiming, SessionId, SessionTreeSnapshot,
    ThinkingLevel, TimestampMs, TurnId,
};

/// Host that records extension-message actions without kernel state.
#[derive(Default)]
struct RecordingHost {
    calls: AtomicUsize,
    command_owners: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl ExtensionHost for RecordingHost {
    /// Returns one complete extension message and records the action.
    async fn send_extension_message(
        &self,
        invocation: &ExtensionInvocation,
        draft: ExtensionMessageDraft,
    ) -> Result<AgentMessage, ExtensionHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let timestamp = invocation.timestamp_ms;
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("message-extension")
                    .expect("message id"),
                turn_id: invocation
                    .turn_id
                    .clone()
                    .expect("turn-bound invocation"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::Extension {
                extension: protocol::ExtensionMessage::builder()
                    .extension_id(draft.extension_id)
                    .custom_type(draft.custom_type)
                    .blocks(draft.blocks)
                    .display(draft.display)
                    .include_in_context(draft.include_in_context)
                    .build(),
            },
        })
    }

    /// Records the invocation owner and the normalized command owner.
    async fn register_command(
        &self,
        invocation: &ExtensionInvocation,
        command: RegisteredCommand,
    ) -> Result<(), ExtensionHostError> {
        self.command_owners
            .lock()
            .expect("command owner lock")
            .push((
                invocation.extension_id.to_string(),
                command.extension_id.to_string(),
            ));
        Ok(())
    }
}

/// Command implementation used to exercise context ownership normalization.
struct CommandHandler;

#[async_trait]
impl ExtensionCommandHandler for CommandHandler {
    /// Completes without invoking additional host capabilities.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Creates an actionable context bound to one shared runtime generation.
fn context(
    host: Arc<dyn ExtensionHost>,
    generation: RuntimeGeneration,
) -> ExtensionContext {
    let session_id = SessionId::try_from("session-1").expect("session id");
    ExtensionContext::builder()
        .invocation(
            ExtensionInvocation::builder()
                .extension_id(
                    ExtensionId::try_from("audit").expect("extension id"),
                )
                .session_id(session_id.clone())
                .turn_id(Some(TurnId::try_from("turn-1").expect("turn id")))
                .timestamp_ms(TimestampMs::from(1))
                .cwd("/workspace".into())
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
        .host(host)
        .generation(generation)
        .build()
}

/// Context actions reject stale session runtimes before reaching the host.
#[tokio::test]
async fn context_generation_guards_every_host_action() {
    let host = Arc::new(RecordingHost::default());
    let generation = RuntimeGeneration::new();
    let context = context(
        Arc::clone(&host) as Arc<dyn ExtensionHost>,
        generation.clone(),
    );
    let draft = ExtensionMessageDraft::builder()
        .extension_id(ExtensionId::try_from("audit").expect("extension id"))
        .custom_type("notice".to_string())
        .blocks(Vec::new())
        .display(true)
        .include_in_context(false)
        .build();

    context
        .send_extension_message(draft.clone())
        .await
        .expect("active context action");
    generation.invalidate();
    assert!(matches!(
        context.send_extension_message(draft).await,
        Err(ExtensionHostError::StaleRuntime)
    ));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
}

/// Dynamic command actions always use the calling extension as their owner.
#[tokio::test]
async fn context_prevents_cross_extension_command_mutation() {
    let host = Arc::new(RecordingHost::default());
    let context = context(
        Arc::clone(&host) as Arc<dyn ExtensionHost>,
        RuntimeGeneration::new(),
    );
    context
        .register_command(RegisteredCommand {
            extension_id: ExtensionId::try_from("spoofed")
                .expect("extension id"),
            definition: ExtensionCommandDefinition {
                name: "inspect".to_string(),
                description: None,
            },
            handler: Arc::new(CommandHandler),
        })
        .await
        .expect("register owned command");
    assert_eq!(
        *host.command_owners.lock().expect("command owner lock"),
        vec![("audit".to_string(), "audit".to_string())]
    );
}

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    Extension, ExtensionFactory, ExtensionPipeline, StaticExtensionFactory,
};
use protocol::{
    AgentMessage, ContentBlock, ExtensionContext, ExtensionDirective,
    ExtensionEvent, MessageContent, MessageId, MessageIdentity, MessageTiming,
    SessionId, TimestampMs, TurnId,
};

/// Recording extension that exposes pipeline dispatch order.
struct Recorder {
    name: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
}

struct DirectiveExtension(ExtensionDirective);

#[async_trait]
impl Extension for DirectiveExtension {
    /// Returns one configured directive to exercise cross-extension composition.
    async fn handle(
        &self,
        _event: &ExtensionEvent,
        _context: &ExtensionContext,
    ) -> Result<ExtensionDirective, extension::ExtensionError> {
        Ok(self.0.clone())
    }
}

#[async_trait]
impl Extension for Recorder {
    /// Records the extension name and event discriminant.
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _context: &ExtensionContext,
    ) -> Result<ExtensionDirective, extension::ExtensionError> {
        self.calls
            .lock()
            .expect("recording mutex")
            .push(format!("{}:{event}", self.name));
        Ok(ExtensionDirective::Continue)
    }
}

/// Extension factories preserve registration order across lifecycle dispatch.
#[tokio::test]
async fn pipeline_dispatches_extensions_in_registration_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let factory = StaticExtensionFactory::new(vec![
        Arc::new(Recorder {
            name: "first",
            calls: Arc::clone(&calls),
        }),
        Arc::new(Recorder {
            name: "second",
            calls: Arc::clone(&calls),
        }),
    ]);
    let pipeline = factory.create().expect("pipeline should build");
    pipeline
        .dispatch(
            &ExtensionEvent::TurnStart,
            &ExtensionContext {
                session_id: SessionId::try_from("session-1")
                    .expect("valid session id"),
                turn_id: Some(
                    TurnId::try_from("turn-1").expect("valid turn id"),
                ),
            },
        )
        .await
        .expect("event should dispatch");

    assert_eq!(
        *calls.lock().expect("recording mutex"),
        vec!["first:turn_start", "second:turn_start"]
    );
}

/// A blocking directive stops later extensions and returns the blocker.
#[tokio::test]
async fn pipeline_stops_after_block_directive() {
    struct Blocker;

    #[async_trait]
    impl Extension for Blocker {
        /// Blocks the lifecycle operation with a stable reason.
        async fn handle(
            &self,
            _event: &ExtensionEvent,
            _context: &ExtensionContext,
        ) -> Result<ExtensionDirective, extension::ExtensionError> {
            Ok(ExtensionDirective::Block {
                reason: "policy".to_string(),
            })
        }
    }

    let pipeline = ExtensionPipeline::new(vec![Arc::new(Blocker)]);
    let directive = pipeline
        .dispatch(
            &ExtensionEvent::BeforeAgentStart,
            &ExtensionContext {
                session_id: SessionId::try_from("session-1")
                    .expect("valid session id"),
                turn_id: None,
            },
        )
        .await
        .expect("event should dispatch");

    assert_eq!(directive.block_reason.as_deref(), Some("policy"));
}

/// Later metadata must not discard messages injected by an earlier extension.
#[tokio::test]
async fn pipeline_preserves_injection_when_later_extension_adds_metadata() {
    let timestamp = TimestampMs::from(10);
    let injected = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-injected")
                .expect("message id"),
            turn_id: TurnId::try_from("turn-1").expect("turn id"),
        },
        timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
            .expect("message timing"),
        content: MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: "injected".to_string(),
            }],
        },
    };
    let pipeline = ExtensionPipeline::new(vec![
        Arc::new(DirectiveExtension(ExtensionDirective::Inject {
            messages: vec![injected.clone()],
        })),
        Arc::new(DirectiveExtension(ExtensionDirective::Metadata {
            value: serde_json::json!({ "source": "second" }),
        })),
    ]);

    let effects = pipeline
        .dispatch(
            &ExtensionEvent::Context,
            &ExtensionContext {
                session_id: SessionId::try_from("session-1")
                    .expect("session id"),
                turn_id: Some(TurnId::try_from("turn-1").expect("turn id")),
            },
        )
        .await
        .expect("event should dispatch");

    assert_eq!(effects.injections, vec![injected]);
    assert_eq!(
        effects.metadata,
        vec![serde_json::json!({ "source": "second" })]
    );
}

use rig_core::test_utils::RecordingHttpClient;
use rig_core::{
    Model,
    completion::{
        Accepts, CompletionRequest, ReplayTarget, ToolDefinition,
        options::{OptionFields, OptionMap},
    },
    error::EncodeError,
    message::{
        Api, AssistantContent, AssistantMessage, Message, Origin, ToolCall,
        ToolFunction, ToolName, ToolResultContent, UserContent,
    },
    providers::openai::{OpenAIConfig, wire::Chat},
    wire::{Descriptor, Mode, Wire},
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Debug)]
struct CountedWire {
    inner: Chat,
    calls: Arc<AtomicUsize>,
}
impl ReplayTarget for CountedWire {
    /// Delegate the actual API identity.
    fn api(&self) -> Api {
        self.inner.api()
    }
    /// Delegate the actual provider identity.
    fn provider(&self) -> &str {
        self.inner.provider()
    }
    /// Delegate the model identity.
    fn model(&self) -> &str {
        self.inner.model()
    }
    /// Delegate the actual message capabilities.
    fn accepts(&self, model: &str) -> Accepts {
        self.inner.accepts(model)
    }
    /// Delegate option mapping without changing request preparation.
    fn map_options(
        &self,
        request: &CompletionRequest,
        fields: OptionFields<'_>,
    ) -> OptionMap {
        self.inner.map_options(request, fields)
    }
    /// Count real historical call normalization inside upstream preparation.
    fn normalize_tool_call_id(
        &self,
        id: &str,
        model: &str,
        source: Option<&Origin>,
    ) -> String {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.normalize_tool_call_id(id, model, source)
    }
}
impl Wire for CountedWire {
    type Op = <Chat as Wire>::Op;
    type Payload = <Chat as Wire>::Payload;
    type Frame = <Chat as Wire>::Frame;
    type Event = <Chat as Wire>::Event;
    type Decoder<'id> = <Chat as Wire>::Decoder<'id>;
    type Reassembler = <Chat as Wire>::Reassembler;
    /// Preserve the built-in descriptor while observing its replay work.
    fn describe(&self) -> Descriptor<'_> {
        self.inner.describe().replay(self)
    }
    /// Delegate request encoding unchanged.
    fn encode(
        &self,
        request: CompletionRequest,
        mode: Mode,
    ) -> Result<Self::Payload, EncodeError> {
        self.inner.encode(request, mode)
    }
    /// Delegate response decoding unchanged.
    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        self.inner.decoder()
    }
}

/// Build a valid history with ten completed tool calls for one session.
fn request(session: &str) -> CompletionRequest {
    let name = ToolName::new("lookup").expect("valid synthetic history");
    let mut history = vec![Message::from(format!("session {session}"))];
    for i in 0..10 {
        let call = ToolCall::from_wire(
            format!("{session}_{i}"),
            ToolFunction::new(name.clone(), json!({"q":i})),
        );
        let result = UserContent::tool_result(
            call.id.clone(),
            name.clone(),
            vec![ToolResultContent::text("ok")],
        );
        history.push(Message::Assistant(AssistantMessage::new(vec![
            AssistantContent::ToolCall(call),
        ])));
        history.push(Message::User {
            content: vec![result],
        });
    }
    CompletionRequest::new(history.pop().expect("valid synthetic history"))
        .messages(history)
        .tools([ToolDefinition::new(
            name,
            "lookup",
            json!({"type":"object","properties":{"q":{"type":"integer"}}}),
        )])
}

/// Compare one warm history with alternating sessions on independent model caches.
#[test]
fn independent_prepared_caches_reuse_each_sessions_history() {
    let calls = Arc::new(AtomicUsize::new(0));
    let wire = CountedWire {
        inner: OpenAIConfig::new("synthetic-review-key").chat("audit"),
        calls: Arc::clone(&calls),
    };
    let model_a = Model::new(wire.clone(), RecordingHttpClient::new("{}"));
    let model_b = Model::new(wire, RecordingHttpClient::new("{}"));
    let mut counts = Vec::new();
    for session in ["A", "A", "B", "A"] {
        let before = calls.load(Ordering::Relaxed);
        let model = if session == "A" { &model_a } else { &model_b };
        drop(
            model
                .clone()
                .erase()
                .stream(request(session))
                .expect("valid synthetic history"),
        );
        counts.push(calls.load(Ordering::Relaxed) - before);
    }
    assert_eq!(counts, vec![10, 1, 10, 1]);
}

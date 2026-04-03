use std::collections::VecDeque;
use std::sync::Arc;

use agent_core::{
    Error, Result,
    events::{AgentEvent, RecordingEventSink},
    model::{AgentModel, ModelRequest, ModelResponse},
    runtime::{AgentLoopConfig, AgentRunner, RunRequest},
    session::{InMemorySessionStore, SessionId, SessionStore, ThreadId},
    tools::{ToolCallRequest, builtin::default_read_only_tools, registry::ToolRegistry},
};
use async_trait::async_trait;
use llm::{completion::Message, usage::Usage};
use tokio::sync::Mutex;

#[derive(Clone)]
struct SequenceModel {
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
}

impl SequenceModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }
}

#[async_trait(?Send)]
impl AgentModel for SequenceModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
        self.responses
            .lock()
            .await
            .pop_front()
            .ok_or(Error::Runtime {
                message: "sequence model exhausted".to_string(),
                stage: "sequence-model-complete".to_string(),
            })
    }
}

fn usage(total_tokens: u64) -> Usage {
    Usage {
        input_tokens: total_tokens / 2,
        output_tokens: total_tokens / 2,
        total_tokens,
        cached_input_tokens: 0,
        cache_creation_input_tokens: 0,
    }
}

#[tokio::test]
async fn runner_executes_tool_calls_and_persists_the_turn() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("calling echo".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("hello", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(ToolRegistry::default());
    for tool in default_read_only_tools() {
        registry.register_arc(tool).await;
    }
    let sink = Arc::new(RecordingEventSink::default());

    let runner = AgentRunner::new(model, store.clone(), registry, sink.clone())
        .with_system_prompt("Use tools when they are helpful.")
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
        });

    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let result = runner
        .run(RunRequest::new(
            session_id.clone(),
            thread_id.clone(),
            "say hello",
        ))
        .await
        .unwrap();

    assert_eq!(result.text, "hello");
    assert_eq!(result.usage.total_tokens, 16);

    let messages = store
        .load_messages(session_id, thread_id, 20)
        .await
        .unwrap();
    assert_eq!(messages[0], Message::user("say hello"));
    assert!(matches!(messages[1], Message::Assistant { .. }));
    assert!(matches!(messages[2], Message::User { .. }));
    assert_eq!(messages[3], Message::assistant("hello"));

    let events = sink.snapshot().await;
    assert!(matches!(&events[0], AgentEvent::RunStarted { .. }));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { name, output } if name == "echo" && output == "hello"
    )));
}

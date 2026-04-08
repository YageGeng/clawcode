use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::{
    Agent, AgentContext, AgentDeps, AgentLoopConfig, AgentRunRequest, Error, Result,
    events::{AgentEvent, AgentStage, RecordingEventSink, ToolStage},
    model::{AgentModel, ModelRequest, ModelResponse},
    runtime::{AgentRunner, RunRequest},
    session::{InMemorySessionStore, SessionId, SessionStore, ThreadId},
    tools::{ToolCallRequest, builtin::default_read_only_tools, registry::ToolRegistry},
};
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
        AgentEvent::ToolStatusUpdated { stage, name, iteration, tool_id, tool_call_id } if *stage == ToolStage::Calling && name == "echo" && *iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStatusUpdated { stage, name, iteration, tool_id, tool_call_id } if *stage == ToolStage::Completed && name == "echo" && *iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { name, output } if name == "echo" && output == "hello"
    )));
}

#[tokio::test]
async fn agent_runs_with_stable_context_and_persists_the_turn() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::text(
        "hello from agent",
        usage(8),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(ToolRegistry::default());
    let sink = Arc::new(RecordingEventSink::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let agent = Agent::new(
        AgentContext::new(session_id.clone(), thread_id.clone()),
        AgentDeps::new(model, store.clone(), registry, sink.clone()),
    )
    .with_system_prompt("You are a helpful agent.");

    let result = agent.run(AgentRunRequest::new("say hello")).await.unwrap();

    assert_eq!(result.text, "hello from agent");
    assert_eq!(result.usage.total_tokens, 8);

    let messages = store
        .load_messages(session_id, thread_id, 20)
        .await
        .unwrap();
    assert_eq!(messages[0], Message::user("say hello"));
    assert_eq!(messages[1], Message::assistant("hello from agent"));

    let events = sink.snapshot().await;
    assert!(matches!(&events[0], AgentEvent::RunStarted { .. }));
    assert!(
        matches!(&events[1], AgentEvent::StatusUpdated { stage, iteration, .. } if *stage == AgentStage::ModelRequesting && *iteration == Some(1))
    );
    assert!(matches!(&events[2], AgentEvent::ModelRequested { .. }));
    assert!(
        matches!(&events[3], AgentEvent::StatusUpdated { stage, iteration, .. } if *stage == AgentStage::Responding && *iteration == Some(1))
    );
    assert!(matches!(&events[4], AgentEvent::TextProduced { .. }));
    assert!(matches!(&events[5], AgentEvent::RunFinished { .. }));
}

#[tokio::test]
async fn agent_context_can_fork_child_identity_for_the_same_thread() {
    let parent = AgentContext::new(SessionId::new(), ThreadId::new()).with_name("planner");
    let child = parent.fork("reviewer");

    assert_eq!(child.session_id, parent.session_id);
    assert_eq!(child.thread_id, parent.thread_id);
    assert_eq!(
        child.parent_agent_id.as_deref(),
        Some(parent.agent_id.as_str())
    );
    assert_eq!(child.name.as_deref(), Some("reviewer"));
    assert_ne!(child.agent_id, parent.agent_id);
}

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::{
    Agent, AgentContext, AgentDeps, AgentLoopConfig, AgentRunRequest, Error, Result,
    events::{AgentEvent, AgentStage, RecordingEventSink, ToolStage},
    model::{AgentModel, ModelRequest, ModelResponse},
    runtime::{AgentRunner, RunRequest},
    session::{InMemorySessionStore, SessionId, SessionStore, ThreadId},
    tools::{
        Tool, ToolCallRequest, ToolInvocation, ToolMetadata, ToolOutput, ToolRouter,
        registry::ToolRegistryBuilder,
    },
};
use llm::{completion::Message, usage::Usage};
use serde_json::json;
use tokio::sync::Mutex;
use tools::{Error as ToolError, Result as ToolResult};

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

/// Minimal echo tool used to keep the agent-loop test independent from removed demo tools.
struct TestEchoTool;

#[async_trait]
impl Tool for TestEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Echoes the provided text for agent-loop tests."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo back in the tool output."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .ok_or(ToolError::Runtime {
                message: "missing text argument".to_string(),
                stage: "test-echo-parse-args".to_string(),
            })?;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: json!({"text": text}),
        })
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
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());

    let runner = AgentRunner::new(model, store.clone(), router, sink.clone())
        .with_system_prompt("Use tools when they are helpful.")
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
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
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolCallCompleted {
                name,
                output,
                structured_output
            } if name == "echo"
                && output == "hello"
                && structured_output.as_ref() == Some(&json!({"text": "hello"}))
        )
    }));
}

#[tokio::test]
async fn runner_emits_tool_status_events_for_each_tool_call_in_a_batch() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("calling echo twice".to_string()),
            vec![
                ToolCallRequest::new("call_1", "echo", serde_json::json!({"text": "hello"})),
                ToolCallRequest::new("call_2", "echo", serde_json::json!({"text": "world"})),
            ],
            usage(10),
        ),
        ModelResponse::text("done", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());

    let runner =
        AgentRunner::new(model, store, router, sink.clone()).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    let calling_events = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ToolStatusUpdated {
                    stage,
                    name,
                    iteration,
                    ..
                } if *stage == ToolStage::Calling && name == "echo" && *iteration == Some(1)
            )
        })
        .collect::<Vec<_>>();
    let completed_events = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ToolStatusUpdated {
                    stage,
                    name,
                    iteration,
                    ..
                } if *stage == ToolStage::Completed && name == "echo" && *iteration == Some(1)
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(calling_events.len(), 2);
    assert!(calling_events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStatusUpdated { tool_id, tool_call_id, .. } if tool_id == "call_1" && tool_call_id == "call_1"
    )));
    assert!(calling_events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStatusUpdated { tool_id, tool_call_id, .. } if tool_id == "call_2" && tool_call_id == "call_2"
    )));

    assert_eq!(completed_events.len(), 2);
    assert!(completed_events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStatusUpdated { tool_id, tool_call_id, .. } if tool_id == "call_1" && tool_call_id == "call_1"
    )));
    assert!(completed_events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolStatusUpdated { tool_id, tool_call_id, .. } if tool_id == "call_2" && tool_call_id == "call_2"
    )));
}

#[tokio::test]
async fn agent_runs_with_stable_context_and_persists_the_turn() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::text(
        "hello from agent",
        usage(8),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(kernel::tools::ToolRegistry::default());
    let sink = Arc::new(RecordingEventSink::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let agent = Agent::new(
        AgentContext::new(session_id.clone(), thread_id.clone()),
        AgentDeps::new(
            model,
            store.clone(),
            Arc::new(ToolRouter::new(registry, Vec::new())),
            sink.clone(),
        ),
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

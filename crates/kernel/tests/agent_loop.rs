use std::collections::VecDeque;
use std::fs;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream;
use kernel::{
    AgentLoopConfig, Error, Result, ThreadHandle, ThreadRunRequest, ThreadRuntime, TurnContext,
    UserInput,
    events::{AgentEvent, AgentStage, RecordingEventSink, ToolStage},
    model::{AgentModel, ModelRequest, ModelResponse, ResponseItem},
    runtime::RunRequest,
    session::{InMemorySessionStore, SessionContinuationRequest, SessionId, ThreadId},
    tools::{
        Tool, ToolCallRequest, ToolInvocation, ToolMetadata, ToolOutput, ToolRouter,
        registry::ToolRegistryBuilder,
    },
    user_inputs_display_text, user_inputs_to_messages,
};
use llm::{
    completion::{Message, message::UserContent},
    usage::Usage,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Barrier;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::{Error as ToolError, Result as ToolResult, StructuredToolOutput};

#[derive(Clone)]
struct SequenceModel {
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
}

#[derive(Clone, Default)]
struct RecordingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
}

#[derive(Clone)]
struct EventSequenceModel {
    events: Arc<Mutex<VecDeque<Vec<kernel::Result<kernel::model::ResponseEvent>>>>>,
}

impl EventSequenceModel {
    /// Builds a model double that returns explicit response-event streams per iteration.
    fn new(events: Vec<Vec<kernel::Result<kernel::model::ResponseEvent>>>) -> Self {
        Self {
            events: Arc::new(Mutex::new(events.into())),
        }
    }
}

impl RecordingModel {
    /// Builds a model double that records each request before returning queued responses.
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }

    /// Returns a snapshot of every request observed by the model.
    async fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().await.clone()
    }
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
                inflight_snapshot: None,
            })
    }
}

#[async_trait(?Send)]
impl AgentModel for RecordingModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        self.requests.lock().await.push(request);
        self.responses
            .lock()
            .await
            .pop_front()
            .ok_or(Error::Runtime {
                message: "recording model exhausted".to_string(),
                stage: "recording-model-complete".to_string(),
                inflight_snapshot: None,
            })
    }
}

#[async_trait(?Send)]
impl AgentModel for EventSequenceModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
        unreachable!("event sequence model should be consumed through streaming")
    }

    async fn stream(&self, _request: ModelRequest) -> Result<kernel::model::ResponseEventStream> {
        let events = self.events.lock().await.pop_front().ok_or(Error::Runtime {
            message: "event sequence model exhausted".to_string(),
            stage: "event-sequence-model-stream".to_string(),
            inflight_snapshot: None,
        })?;
        Ok(Box::pin(stream::iter(events)))
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

/// Extracts plain text from a user message for assertions that inspect prompt context.
fn first_user_text(message: &Message) -> &str {
    let Message::User { content } = message else {
        panic!("expected user message");
    };
    let UserContent::Text(text) = content.first_ref() else {
        panic!("expected text user content");
    };
    text.text()
}

/// Creates a filesystem skill root with one SNAFU skill for runtime integration tests.
fn write_test_skill_root() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let skill_dir = temp.path().join("rust-error-snafu");
    fs::create_dir_all(&skill_dir).expect("skill dir should be created");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rust-error-snafu\ndescription: Typed Rust errors.\n---\nUse SNAFU context.\n",
    )
    .expect("skill file should be written");
    temp
}

/// Verifies skill inputs are display metadata but not normal model messages.
#[test]
fn user_input_helpers_skip_skill_inputs_for_model_messages() {
    let inputs = vec![
        UserInput::skill("alpha-skill", "/tmp/alpha/SKILL.md"),
        UserInput::text("hello"),
    ];

    let messages = user_inputs_to_messages(&inputs);

    assert_eq!(
        user_inputs_display_text(&inputs),
        "[skill:alpha-skill](/tmp/alpha/SKILL.md)\nhello"
    );
    assert_eq!(messages, vec![Message::user("hello")]);
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
            structured: StructuredToolOutput::text(text),
        })
    }
}

/// Minimal read tool used to make the skills index visible in system-prompt tests.
struct PromptReadTool;

#[async_trait]
impl Tool for PromptReadTool {
    fn name(&self) -> &'static str {
        "fs/read_text_file"
    }

    fn description(&self) -> &'static str {
        "Reads text files for prompt-assembly tests."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Unused path argument."
                }
            },
            "required": ["path"]
        })
    }

    async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput::text("unused"))
    }
}

/// Tool that blocks until the test inspects incremental session state.
struct BlockingEchoTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

/// Tool that blocks only the first serial call so tests can inspect in-flight state precisely.
struct FirstCallBlockingEchoTool {
    start_barrier: Arc<Barrier>,
    resume_barrier: Arc<Barrier>,
}

#[async_trait]
impl Tool for FirstCallBlockingEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Blocks the first matching call so serial state transitions can be inspected."
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
                stage: "first-call-blocking-echo-parse-args".to_string(),
            })?;

        if text == "hello" {
            self.start_barrier.wait().await;
            self.resume_barrier.wait().await;
        }

        Ok(ToolOutput {
            text: text.to_string(),
            structured: StructuredToolOutput::text(text),
        })
    }
}

#[async_trait]
impl Tool for BlockingEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Blocks until the test confirms incremental session persistence."
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
                stage: "blocking-echo-parse-args".to_string(),
            })?;

        self.started.notify_waiters();
        self.release.notified().await;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: StructuredToolOutput::text(text),
        })
    }
}

/// Tool that always fails so runtime tests can assert failed in-flight state handling.
struct FailingEchoTool;

#[async_trait]
impl Tool for FailingEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Fails immediately so the runtime can surface failed handle state."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Unused text argument."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        Err(ToolError::Runtime {
            message: "echo failed".to_string(),
            stage: "failing-echo-handle".to_string(),
        })
    }
}

/// Tool that always fails under its own name so mixed-success batches can be tested.
struct NamedFailingEchoTool;

#[async_trait]
impl Tool for NamedFailingEchoTool {
    fn name(&self) -> &'static str {
        "fail_echo"
    }

    fn description(&self) -> &'static str {
        "Fails immediately so mixed tool batches can surface partial completion."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Unused text argument."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        Err(ToolError::Runtime {
            message: "named echo failed".to_string(),
            stage: "named-failing-echo-handle".to_string(),
        })
    }
}

/// Tool that waits on a barrier so the loop test can prove parallel queue draining.
struct BarrierEchoTool {
    barrier: Arc<Barrier>,
}

#[async_trait]
impl Tool for BarrierEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Waits on a barrier before echoing so loop tests can require parallel execution."
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
                stage: "barrier-echo-parse-args".to_string(),
            })?;

        self.barrier.wait().await;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: StructuredToolOutput::text(text),
        })
    }
}

/// Parallel-safe tool that increments a shared active counter before synchronizing.
struct NamedParallelBarrierTool {
    name: &'static str,
    barrier: Arc<Barrier>,
    active_parallel_tools: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for NamedParallelBarrierTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Parallel-safe test tool that synchronizes with another parallel-safe call."
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
                stage: "named-parallel-barrier-parse-args".to_string(),
            })?;

        self.active_parallel_tools.fetch_add(1, Ordering::SeqCst);
        self.barrier.wait().await;
        self.active_parallel_tools.fetch_sub(1, Ordering::SeqCst);

        Ok(ToolOutput {
            text: text.to_string(),
            structured: StructuredToolOutput::text(text),
        })
    }
}

/// Serial-only tool that fails if the loop tries to run it alongside active parallel-safe tools.
struct StrictSerialEchoTool {
    active_parallel_tools: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for StrictSerialEchoTool {
    fn name(&self) -> &'static str {
        "serial_echo"
    }

    fn description(&self) -> &'static str {
        "Serial-only test tool that must not overlap with parallel-safe tools."
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
        if self.active_parallel_tools.load(Ordering::SeqCst) != 0 {
            return Err(ToolError::Runtime {
                message: "serial tool overlapped with parallel tool execution".to_string(),
                stage: "strict-serial-echo-overlap".to_string(),
            });
        }

        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .ok_or(ToolError::Runtime {
                message: "missing text argument".to_string(),
                stage: "strict-serial-echo-parse-args".to_string(),
            })?;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: StructuredToolOutput::text(text),
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

    let runner = ThreadRuntime::new(model, store.clone(), router, sink.clone()).with_config(
        AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        },
    );

    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let thread = ThreadHandle::new(session_id, thread_id.clone())
        .with_system_prompt("Use tools when they are helpful.");
    let result = runner
        .run(&thread, ThreadRunRequest::new("say hello"))
        .await
        .unwrap();

    assert_eq!(result.text, "hello");
    assert_eq!(result.usage.total_tokens, 16);

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
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
        AgentEvent::ModelResponseCreated { iteration } if *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::Message { text },
            iteration,
        } if text.is_empty() && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelTextDelta { text, iteration } if text == "calling echo" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::ToolCall { item_id, .. },
            iteration,
        } if item_id == "call_1" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemUpdated {
            item: ResponseItem::ToolCall { item_id, name, .. },
            iteration,
        } if item_id == "call_1" && name == "echo" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemDone {
            item: ResponseItem::ToolCall { item_id, name, .. },
            iteration,
        } if name == "echo" && item_id == "call_1" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemDone {
            item: ResponseItem::Message { text },
            iteration,
        } if text == "calling echo" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelStreamCompleted { iteration, usage, .. }
            if *iteration == Some(1) && usage.total_tokens == 10
    )));
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
                status,
                name,
                output,
                structured_output,
                ..
            } if name == "echo"
                && *status == kernel::events::ToolCallCompletionStatus::Succeeded
                && output == "hello"
                && structured_output.as_ref()
                    == Some(&StructuredToolOutput::text("hello"))
        )
    }));
}

#[tokio::test]
async fn runner_keeps_completed_message_text_when_tool_calls_have_no_text_deltas() {
    let model = Arc::new(EventSequenceModel::new(vec![
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "hello"})),
                    arguments_text: "{\"text\":\"hello\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "hello"})),
                    arguments_text: "{\"text\":\"hello\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "calling echo".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_1".to_string()),
                usage: usage(10),
            }),
        ],
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputTextDelta(
                "done".to_string(),
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "done".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_2".to_string()),
                usage: usage(6),
            }),
        ],
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    runner
        .run_request(RunRequest::new(session_id, thread_id.clone(), "say hello"))
        .await
        .unwrap();

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
        .await
        .unwrap();
    let Message::Assistant { content, .. } = &messages[1] else {
        panic!("expected assistant tool-call message");
    };
    let content = content.iter().collect::<Vec<_>>();
    assert!(matches!(
        &content[0],
        llm::completion::AssistantContent::Text(text) if text.text() == "calling echo"
    ));
}

#[tokio::test]
async fn runner_rejects_streams_that_end_without_completed_event() {
    let model = Arc::new(EventSequenceModel::new(vec![vec![
        Ok(kernel::model::ResponseEvent::Created),
        Ok(kernel::model::ResponseEvent::OutputItemAdded(
            ResponseItem::Message {
                text: String::new(),
            },
        )),
        Ok(kernel::model::ResponseEvent::OutputTextDelta(
            "partial output".to_string(),
        )),
    ]]));

    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(kernel::tools::ToolRegistry::default());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(
        model,
        Arc::clone(&store),
        Arc::new(ToolRouter::new(registry, Vec::new())),
        sink,
    );
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let error = runner
        .run_request(RunRequest::new(session_id, thread_id.clone(), "say hello"))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("stream-completed"));
    let messages = store
        .load_messages_state(session_id, thread_id, 20)
        .await
        .unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn runner_executes_tool_calls_in_output_completion_order() {
    let model = Arc::new(EventSequenceModel::new(vec![
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputTextDelta(
                "calling tools".to_string(),
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "first"})),
                    arguments_text: "{\"text\":\"first\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_2".to_string(),
                    call_id: Some("call_2".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "second"})),
                    arguments_text: "{\"text\":\"second\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_2".to_string(),
                    call_id: Some("call_2".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "second"})),
                    arguments_text: "{\"text\":\"second\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "first"})),
                    arguments_text: "{\"text\":\"first\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "calling tools".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_1".to_string()),
                usage: usage(10),
            }),
        ],
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputTextDelta(
                "done".to_string(),
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "done".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_2".to_string()),
                usage: usage(6),
            }),
        ],
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let tool_completions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tool_completions,
        vec!["second".to_string(), "first".to_string()]
    );
}

#[tokio::test]
async fn runner_queues_completed_tool_calls_before_stream_completion() {
    let model = Arc::new(EventSequenceModel::new(vec![
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "hello"})),
                    arguments_text: "{\"text\":\"hello\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "hello"})),
                    arguments_text: "{\"text\":\"hello\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "calling echo".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_1".to_string()),
                usage: usage(10),
            }),
        ],
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputTextDelta(
                "done".to_string(),
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "done".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_2".to_string()),
                usage: usage(6),
            }),
        ],
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    let queued_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ToolCallQueued {
                    name,
                    iteration,
                    tool_id,
                    tool_call_id,
                } if name == "echo"
                    && *iteration == Some(1)
                    && tool_id == "call_1"
                    && tool_call_id == "call_1"
            )
        })
        .expect("tool call should be queued as soon as it is completed");
    let completed_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ModelStreamCompleted { iteration, .. } if *iteration == Some(1)
            )
        })
        .expect("stream completion event should be present");

    assert!(queued_index < completed_index);
}

#[tokio::test]
async fn runner_registers_each_completed_tool_call_in_inflight_order() {
    let model = Arc::new(EventSequenceModel::new(vec![
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "first"})),
                    arguments_text: "{\"text\":\"first\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::ToolCall {
                    item_id: "call_2".to_string(),
                    call_id: Some("call_2".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "second"})),
                    arguments_text: "{\"text\":\"second\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_2".to_string(),
                    call_id: Some("call_2".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "second"})),
                    arguments_text: "{\"text\":\"second\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::ToolCall {
                    item_id: "call_1".to_string(),
                    call_id: Some("call_1".to_string()),
                    name: "echo".to_string(),
                    arguments: Some(json!({"text": "first"})),
                    arguments_text: "{\"text\":\"first\"}".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "calling tools".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_1".to_string()),
                usage: usage(10),
            }),
        ],
        vec![
            Ok(kernel::model::ResponseEvent::Created),
            Ok(kernel::model::ResponseEvent::OutputItemAdded(
                ResponseItem::Message {
                    text: String::new(),
                },
            )),
            Ok(kernel::model::ResponseEvent::OutputTextDelta(
                "done".to_string(),
            )),
            Ok(kernel::model::ResponseEvent::OutputItemDone(
                ResponseItem::Message {
                    text: "done".to_string(),
                },
            )),
            Ok(kernel::model::ResponseEvent::Completed {
                message_id: Some("resp_2".to_string()),
                usage: usage(6),
            }),
        ],
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    let in_flight_registrations = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightRegistered {
                tool_id,
                tool_call_id,
                iteration,
                ..
            } if *iteration == Some(1) => Some((tool_id.clone(), tool_call_id.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let completed_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ModelStreamCompleted { iteration, .. } if *iteration == Some(1)
            )
        })
        .expect("stream completion event should be present");
    let second_registered_index = events
        .iter()
        .rposition(|event| {
            matches!(
                event,
                AgentEvent::ToolCallInFlightRegistered { iteration, tool_id, .. }
                    if *iteration == Some(1) && tool_id == "call_1"
            )
        })
        .expect("the second completed tool call should also be registered");

    assert_eq!(
        in_flight_registrations,
        vec![
            ("call_2".to_string(), "call_2".to_string()),
            ("call_1".to_string(), "call_1".to_string())
        ]
    );
    assert!(second_registered_index < completed_index);
}

#[tokio::test]
async fn runner_tracks_inflight_tool_call_state_transitions_per_identity() {
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
        ModelResponse::text("done", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let states = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightStateUpdated {
                iteration,
                tool_id,
                tool_call_id,
                state,
                ..
            } if iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(state)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        states,
        vec![
            kernel::events::ToolCallInFlightState::Queued,
            kernel::events::ToolCallInFlightState::Running,
            kernel::events::ToolCallInFlightState::Completed,
        ]
    );
}

#[tokio::test]
async fn runner_reuses_the_same_execution_handle_across_inflight_state_updates() {
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
        ModelResponse::text("done", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let handle_ids = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightStateUpdated {
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
                ..
            } if iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(handle_id)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(handle_ids.len(), 3);
    assert!(
        handle_ids
            .iter()
            .all(|handle_id| handle_id == &handle_ids[0])
    );
}

#[tokio::test]
async fn runner_reuses_registered_handle_for_tool_completion_events() {
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
        ModelResponse::text("done", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    let registered_handle = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallInFlightRegistered {
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
                ..
            } if *iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(handle_id.clone())
            }
            _ => None,
        })
        .expect("registered handle should be present");
    let completed_handle = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallCompleted {
                name, handle_id, ..
            } if name == "echo" => Some(handle_id.clone()),
            _ => None,
        })
        .expect("tool completion should include execution handle");

    assert_eq!(registered_handle, completed_handle);
}

#[tokio::test]
async fn runner_reuses_registered_handle_for_tool_requested_events() {
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
        ModelResponse::text("done", usage(6)),
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    let registered_handle = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallInFlightRegistered {
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
                ..
            } if *iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(handle_id.clone())
            }
            _ => None,
        })
        .expect("registered handle should be present");
    let requested_handle = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallRequested {
                name, handle_id, ..
            } if name == "echo" => Some(handle_id.clone()),
            _ => None,
        })
        .expect("tool requested event should include execution handle");

    assert_eq!(registered_handle, requested_handle);
}

#[tokio::test]
async fn runner_emits_handle_level_inflight_snapshots_for_partial_completion() {
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
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            tool_execution_mode: kernel::tools::executor::ToolExecutionMode::Serial,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        ))
        .await
        .unwrap();

    let snapshots = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightSnapshot {
                iteration: Some(1),
                completed_handles,
                running_handles,
                ..
            } => Some((completed_handles, running_handles)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(
        snapshots
            .iter()
            .any(|(completed_handles, running_handles)| {
                completed_handles.len() == 1 && running_handles.len() == 1
            }),
        "serial execution should expose a partial-completion snapshot with one completed and one running handle"
    );
}

#[tokio::test]
async fn runner_returns_final_inflight_snapshot_in_run_result() {
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
    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 4,
        max_tool_calls: 4,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        tool_execution_mode: kernel::tools::executor::ToolExecutionMode::Serial,
        ..AgentLoopConfig::default()
    });

    let result = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        ))
        .await
        .unwrap();

    assert_eq!(result.inflight_snapshot.running_handles.len(), 0);
    assert_eq!(result.inflight_snapshot.queued_handles.len(), 0);
    assert_eq!(result.inflight_snapshot.cancelled_handles.len(), 0);
    assert_eq!(result.inflight_snapshot.completed_handles.len(), 2);
    assert_eq!(result.inflight_snapshot.entries.len(), 2);
    assert!(result.inflight_snapshot.entries.iter().all(|entry| {
        !entry.handle_id.is_empty()
            && entry.name == "echo"
            && (entry.tool_id == "call_1" || entry.tool_id == "call_2")
            && (entry.tool_call_id == "call_1" || entry.tool_call_id == "call_2")
            && entry.state == kernel::events::ToolCallInFlightState::Completed
            && (entry.output_summary.as_deref() == Some("hello")
                || entry.output_summary.as_deref() == Some("world"))
            && (entry.structured_output.as_ref() == Some(&StructuredToolOutput::text("hello"))
                || entry.structured_output.as_ref() == Some(&StructuredToolOutput::text("world")))
            && entry.started_at.is_some()
            && entry.finished_at.is_some()
            && entry.duration_ms.is_some()
    }));
    assert!(result.inflight_snapshot.entries.iter().all(|entry| {
        entry.finished_at.unwrap() >= entry.started_at.unwrap() && entry.duration_ms.unwrap() >= 0
    }));
}

#[tokio::test]
async fn runner_persists_tool_call_message_before_tool_completion() {
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
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let started_wait = started.notified();
    let run_future =
        runner.run_request(RunRequest::new(session_id, thread_id.clone(), "say hello"));
    let observer = async {
        started_wait.await;

        let messages = store
            .load_messages_state(session_id, thread_id.clone(), 20)
            .await
            .unwrap();
        assert_eq!(messages[0], Message::user("say hello"));
        assert!(matches!(messages[1], Message::Assistant { .. }));
        assert_eq!(messages.len(), 2);

        release.notify_waiters();
    };

    let (result, ()) = tokio::join!(run_future, observer);
    let result = result.unwrap();
    assert_eq!(result.text, "done");
}

#[tokio::test]
async fn runner_cancels_in_flight_tool_batches_when_loop_token_is_triggered() {
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
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        release,
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let cancellation = CancellationToken::new();
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, sink).with_config(
        AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        }
        .with_cancellation_token(cancellation.clone()),
    );
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let started_wait = started.notified();
    let run_future =
        runner.run_request(RunRequest::new(session_id, thread_id.clone(), "say hello"));
    let canceller = async {
        started_wait.await;
        cancellation.cancel();
    };

    let (result, ()) = tokio::join!(run_future, canceller);
    let error = result.expect_err("runner should surface tool cancellation");
    assert!(matches!(
        error,
        Error::Runtime { ref stage, .. } if stage == "tool-executor-cancelled"
    ));

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
        .await
        .unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn runner_marks_inflight_tool_calls_cancelled_when_loop_token_is_triggered() {
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
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        release,
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let cancellation = CancellationToken::new();
    let runner = ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(
        AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        }
        .with_cancellation_token(cancellation.clone()),
    );

    let started_wait = started.notified();
    let run_future = runner.run_request(RunRequest::new(
        SessionId::new(),
        ThreadId::new(),
        "say hello",
    ));
    let canceller = async {
        started_wait.await;
        cancellation.cancel();
    };

    let (result, ()) = tokio::join!(run_future, canceller);
    let error = result.expect_err("runner should surface tool cancellation");
    assert!(matches!(
        error,
        Error::Runtime { ref stage, .. } if stage == "tool-executor-cancelled"
    ));

    let states = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightStateUpdated {
                iteration,
                tool_id,
                tool_call_id,
                state,
                ..
            } if iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(state)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        states,
        vec![
            kernel::events::ToolCallInFlightState::Queued,
            kernel::events::ToolCallInFlightState::Running,
            kernel::events::ToolCallInFlightState::Cancelled,
        ]
    );
}

#[tokio::test]
async fn runner_marks_inflight_tool_calls_failed_when_tool_execution_errors() {
    let model = RecordingModel::new(vec![
        ModelResponse::tool_calls(
            Some("calling echo".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("tool failure reached the model", usage(6)),
    ]);
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(FailingEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(Arc::new(model.clone()), store, router, Arc::clone(&sink))
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    let result = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .expect("runner should continue after surfacing the failed tool result to the model");
    assert_eq!(result.text, "tool failure reached the model");

    let events = sink.snapshot().await;
    let states = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallInFlightStateUpdated {
                iteration,
                tool_id,
                tool_call_id,
                state,
                ..
            } if *iteration == Some(1) && tool_id == "call_1" && tool_call_id == "call_1" => {
                Some(state.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        vec![
            kernel::events::ToolCallInFlightState::Queued,
            kernel::events::ToolCallInFlightState::Running,
            kernel::events::ToolCallInFlightState::Failed,
        ]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallInFlightStateUpdated {
            iteration,
            tool_id,
            tool_call_id,
            state,
            error_summary,
            ..
        } if *iteration == Some(1)
            && tool_id == "call_1"
            && tool_call_id == "call_1"
            && *state == kernel::events::ToolCallInFlightState::Failed
            && error_summary.as_deref() == Some("tool dispatch failed on `dispatch-tool`, runtime error on `failing-echo-handle`: echo failed")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallInFlightSnapshot {
            iteration,
            failed_handles,
            ..
        } if *iteration == Some(1) && failed_handles.len() == 1
    )));

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| matches!(
        message,
        Message::User { content }
            if content.iter().any(|item| matches!(
                item,
                llm::completion::message::UserContent::ToolResult(tool_result)
                    if format!("{tool_result:?}").contains("echo failed")
            ))
    )));
}

#[tokio::test]
async fn runner_can_return_success_outcome_after_tool_failure_reaches_model() {
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
        ModelResponse::text("tool failure reached the model", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(FailingEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    let outcome = runner
        .run_outcome_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let success = match outcome {
        kernel::runtime::RunOutcome::Success(success) => success,
        other => panic!("expected structured success outcome, got {other:?}"),
    };
    assert_eq!(success.text, "tool failure reached the model");
}

#[tokio::test]
async fn runner_keeps_success_and_failure_states_when_mixed_tool_batch_returns_to_model() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("calling mixed tools".to_string()),
            vec![
                ToolCallRequest::new("call_1", "echo", serde_json::json!({"text": "hello"})),
                ToolCallRequest::new("call_2", "fail_echo", serde_json::json!({"text": "boom"})),
            ],
            usage(10),
        ),
        ModelResponse::text("mixed tool batch handled", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    builder.push_handler_spec(Arc::new(NamedFailingEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    let result = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "run mixed tools",
        ))
        .await
        .expect("runner should continue after a mixed success/failure tool batch");
    assert_eq!(result.text, "mixed tool batch handled");

    let events = sink.snapshot().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallInFlightStateUpdated {
            tool_id,
            state,
            ..
        } if tool_id == "call_1" && *state == kernel::events::ToolCallInFlightState::Completed
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallInFlightStateUpdated {
            tool_id,
            state,
            ..
        } if tool_id == "call_2" && *state == kernel::events::ToolCallInFlightState::Failed
    )));
}

#[tokio::test]
/// Verifies failed tool results are persisted as tool-result messages across multiple turns.
async fn runner_persists_failed_tool_results_across_turns() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("first turn uses tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::tool_calls(
            Some("second turn has a failed tool".to_string()),
            vec![ToolCallRequest::new(
                "call_2",
                "fail_echo",
                serde_json::json!({"text": "boom"}),
            )],
            usage(8),
        ),
        ModelResponse::text("second turn complete", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    store
        .queue_continuation(
            session_id,
            thread_id.clone(),
            SessionContinuationRequest::PendingInput {
                input: "follow up".to_string(),
            },
        )
        .await;
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    builder.push_handler_spec(Arc::new(NamedFailingEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(model, Arc::clone(&store), router, sink).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    let result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("runner should keep going across turns after a failed tool result");
    assert_eq!(result.text, "second turn complete");

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
        .await
        .expect("messages should load after both turns");
    let tool_result_messages = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::User { content }
                    if content
                        .iter()
                        .any(|item| matches!(item, llm::completion::message::UserContent::ToolResult(_)))
            )
        })
        .count();
    assert_eq!(tool_result_messages, 2);
}

#[tokio::test]
/// Verifies that continuation-decision failures become structured task failures and clear the active turn.
async fn runner_returns_structured_failure_and_discards_active_turn_when_continuation_fails() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("second turn complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    store.fail_next_take_continuation();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, sink);

    let outcome = runner
        .run_outcome_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("continuation failures should still return a structured outcome");

    let failure = match outcome {
        kernel::runtime::RunOutcome::Failure(failure) => failure,
        other => panic!("expected structured failure outcome, got {other:?}"),
    };
    assert!(matches!(failure.error, Error::Runtime { .. }));
    assert!(
        failure.continuation_decision_trace.iter().any(|entry| {
            entry.stage == kernel::events::TaskContinuationDecisionStage::BeforeFinalResponseHook
        }),
        "post-loop failures should retain the current turn's hook trace entries"
    );

    let second_result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "second input",
        ))
        .await
        .expect("the failed task should discard its active turn so the next run can begin");
    assert_eq!(second_result.text, "second turn complete");

    let messages = store
        .load_messages_state(session_id, thread_id, 10)
        .await
        .expect("the store should remain readable after the failed task");
    assert_eq!(
        messages,
        vec![
            Message::user("second input"),
            Message::assistant("second turn complete")
        ],
        "the failed turn should have been discarded before the next run started"
    );
}

#[tokio::test]
/// Verifies that discard-turn cleanup failures do not erase the original post-loop task failure.
async fn runner_preserves_primary_error_when_post_loop_cleanup_also_fails() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::text(
        "first turn complete",
        usage(6),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    store.fail_next_take_continuation();
    store.fail_next_discard_turn();
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, sink);

    let outcome = runner
        .run_outcome_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "first input",
        ))
        .await
        .expect("cleanup failures should still produce a structured failure outcome");

    let failure = match outcome {
        kernel::runtime::RunOutcome::Failure(failure) => failure,
        other => panic!("expected structured failure outcome, got {other:?}"),
    };
    let Error::Cleanup {
        source,
        cleanup_error,
        stage,
        ..
    } = failure.error
    else {
        panic!("cleanup failures should be wrapped as cleanup errors");
    };
    assert_eq!(stage, "runner-discard-active-turn");
    assert!(
        matches!(*source, Error::Runtime { ref stage, .. } if stage == "test-continuation-failure"),
        "the cleanup wrapper should preserve the original task failure as its typed source"
    );
    assert!(
        matches!(*cleanup_error, Error::Runtime { ref stage, .. } if stage == "test-discard-failure"),
        "the cleanup wrapper should also expose the cleanup failure separately"
    );
}

#[tokio::test]
/// Verifies that inner turn cleanup failures also preserve the original typed loop error.
async fn runner_preserves_primary_error_when_turn_cleanup_also_fails() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::tool_calls(
        Some("failing tool".to_string()),
        vec![ToolCallRequest::new(
            "call_1",
            "fail_echo",
            serde_json::json!({"text": "boom"}),
        )],
        usage(8),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    store.fail_next_discard_turn();
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(NamedFailingEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, sink);

    let outcome = runner
        .run_outcome_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "first input",
        ))
        .await
        .expect("turn cleanup failures should still produce a structured failure outcome");

    let failure = match outcome {
        kernel::runtime::RunOutcome::Failure(failure) => failure,
        other => panic!("expected structured failure outcome, got {other:?}"),
    };
    let Error::Cleanup {
        source,
        cleanup_error,
        stage,
        ..
    } = failure.error
    else {
        panic!("turn cleanup failures should be wrapped as cleanup errors");
    };
    assert_eq!(stage, "runner-discard-active-turn");
    assert!(
        matches!(*source, Error::Runtime { ref stage, .. } if stage == "sequence-model-complete"),
        "after the failed tool result is returned to the model, the follow-up model failure should remain the primary error"
    );
    assert!(
        matches!(*cleanup_error, Error::Runtime { ref stage, .. } if stage == "test-discard-failure"),
        "the cleanup wrapper should still expose the discard failure"
    );
}

#[tokio::test]
async fn runner_accumulates_inflight_snapshot_entries_across_tool_iterations() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("first tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::tool_calls(
            Some("second tool".to_string()),
            vec![ToolCallRequest::new(
                "call_2",
                "echo",
                serde_json::json!({"text": "world"}),
            )],
            usage(8),
        ),
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 5,
        max_tool_calls: 5,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        ..AgentLoopConfig::default()
    });

    let result = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "run twice",
        ))
        .await
        .expect("runner should succeed after two tool iterations");

    let completed_entries = result
        .inflight_snapshot
        .entries
        .iter()
        .filter(|entry| entry.state == kernel::events::ToolCallInFlightState::Completed)
        .collect::<Vec<_>>();
    assert_eq!(
        completed_entries.len(),
        2,
        "turn-level inflight snapshots should retain completed handles from every tool iteration"
    );
    assert!(
        completed_entries
            .iter()
            .any(|entry| entry.output_summary.as_deref() == Some("hello"))
    );
    assert!(
        completed_entries
            .iter()
            .any(|entry| entry.output_summary.as_deref() == Some("world"))
    );
}

#[tokio::test]
async fn runner_keeps_handle_ids_unique_across_tool_iterations() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("first tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::tool_calls(
            Some("second tool".to_string()),
            vec![ToolCallRequest::new(
                "call_2",
                "echo",
                serde_json::json!({"text": "world"}),
            )],
            usage(8),
        ),
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 5,
        max_tool_calls: 5,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        ..AgentLoopConfig::default()
    });

    let result = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "run twice",
        ))
        .await
        .expect("runner should succeed after two tool iterations");

    let mut handle_ids = result
        .inflight_snapshot
        .entries
        .iter()
        .map(|entry| entry.handle_id.clone())
        .collect::<Vec<_>>();
    handle_ids.sort();
    handle_ids.dedup();

    assert_eq!(
        handle_ids.len(),
        result.inflight_snapshot.entries.len(),
        "turn-level snapshots should not reuse tool execution handles across iterations"
    );
}

#[tokio::test]
async fn runner_emits_one_task_lifecycle_for_one_run_request() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::text(
        "done",
        usage(6),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, Arc::clone(&sink));

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .expect("runner should finish a simple task");

    let events = sink.snapshot().await;
    let run_started_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RunStarted { .. }))
        .count();
    let run_finished_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RunFinished { .. }))
        .count();

    assert_eq!(run_started_count, 1);
    assert_eq!(run_finished_count, 1);
}

#[tokio::test]
async fn runner_drains_pending_inputs_within_one_task_run() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first", usage(6)),
        ModelResponse::text("second", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    store
        .queue_continuation(
            session_id,
            thread_id.clone(),
            SessionContinuationRequest::PendingInput {
                input: "follow up".to_string(),
            },
        )
        .await;
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink));

    let result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("runner should drain queued pending inputs");

    assert_eq!(result.text, "second");

    let messages = store
        .load_messages_state(session_id, thread_id.clone(), 20)
        .await
        .expect("session messages should load");
    let user_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::User { .. }))
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant { .. }))
        .count();
    assert_eq!(user_messages, 2);
    assert_eq!(assistant_messages, 2);

    let events = sink.snapshot().await;
    let run_started_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RunStarted { .. }))
        .count();
    let run_finished_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RunFinished { .. }))
        .count();
    assert_eq!(run_started_count, 1);
    assert_eq!(run_finished_count, 1);
    assert_eq!(
        result.continuation_decision_trace,
        vec![
            kernel::events::TaskContinuationDecisionTraceEntry {
                stage: kernel::events::TaskContinuationDecisionStage::BeforeFinalResponseHook,
                decision: kernel::events::TaskContinuationDecisionKind::Continue,
                source: None,
            },
            kernel::events::TaskContinuationDecisionTraceEntry {
                stage: kernel::events::TaskContinuationDecisionStage::SessionQueue,
                decision: kernel::events::TaskContinuationDecisionKind::Request,
                source: Some(kernel::events::TaskContinuationSource::PendingInput),
            },
            kernel::events::TaskContinuationDecisionTraceEntry {
                stage: kernel::events::TaskContinuationDecisionStage::FinalDecision,
                decision: kernel::events::TaskContinuationDecisionKind::Adopted,
                source: Some(kernel::events::TaskContinuationSource::PendingInput),
            },
            kernel::events::TaskContinuationDecisionTraceEntry {
                stage: kernel::events::TaskContinuationDecisionStage::BeforeFinalResponseHook,
                decision: kernel::events::TaskContinuationDecisionKind::Continue,
                source: None,
            },
            kernel::events::TaskContinuationDecisionTraceEntry {
                stage: kernel::events::TaskContinuationDecisionStage::FinalDecision,
                decision: kernel::events::TaskContinuationDecisionKind::Finished,
                source: Some(kernel::events::TaskContinuationSource::TaskCompleted),
            },
        ]
    );
}

#[tokio::test]
async fn runner_drains_pending_inputs_enqueued_during_an_active_turn() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("need tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("second turn complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink));

    let started_wait = started.notified();
    let run_future = runner.run_request(RunRequest::new(
        session_id,
        thread_id.clone(),
        "first input",
    ));
    let enqueue_follow_up = async {
        started_wait.await;
        store
            .queue_continuation(
                session_id,
                thread_id.clone(),
                SessionContinuationRequest::PendingInput {
                    input: "runtime follow up".to_string(),
                },
            )
            .await;
        release.notify_waiters();
    };

    let (result, ()) = tokio::join!(run_future, enqueue_follow_up);
    let result = result.expect("runner should drain a pending input queued during the task");
    assert_eq!(result.text, "second turn complete");

    let messages = store
        .load_messages_state(session_id, thread_id.clone(), 20)
        .await
        .expect("session messages should load");
    let user_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::User { .. }))
        .count();
    let tool_result_messages = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::User { content }
                    if content
                        .iter()
                        .any(|item| matches!(item, llm::completion::message::UserContent::ToolResult(_)))
            )
        })
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant { .. }))
        .count();
    assert_eq!(user_messages, 3);
    assert_eq!(tool_result_messages, 1);
    assert_eq!(assistant_messages, 3);

    let events = sink.snapshot().await;
    let continuation_decisions = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => {
                Some((action.clone(), source.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::PendingInput,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
async fn runner_distinguishes_system_follow_up_continuations() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first", usage(6)),
        ModelResponse::text("system follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    store
        .queue_continuation(
            session_id,
            thread_id.clone(),
            SessionContinuationRequest::SystemFollowUp {
                input: "system asks for another turn".to_string(),
            },
        )
        .await;
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink));

    let result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("runner should drain queued system follow-up continuations");

    assert_eq!(result.text, "system follow up complete");

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
        .await
        .expect("session messages should load");
    let user_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::User { .. }))
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant { .. }))
        .count();
    assert_eq!(user_messages, 2);
    assert_eq!(assistant_messages, 2);

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided {
                action,
                source,
                decision_trace,
                ..
            } => Some((action, source, decision_trace)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
                vec![
                    kernel::events::TaskContinuationDecisionTraceEntry {
                        stage:
                            kernel::events::TaskContinuationDecisionStage::BeforeFinalResponseHook,
                        decision: kernel::events::TaskContinuationDecisionKind::Continue,
                        source: None,
                    },
                    kernel::events::TaskContinuationDecisionTraceEntry {
                        stage: kernel::events::TaskContinuationDecisionStage::SessionQueue,
                        decision: kernel::events::TaskContinuationDecisionKind::Request,
                        source: Some(kernel::events::TaskContinuationSource::SystemFollowUp),
                    },
                    kernel::events::TaskContinuationDecisionTraceEntry {
                        stage: kernel::events::TaskContinuationDecisionStage::FinalDecision,
                        decision: kernel::events::TaskContinuationDecisionKind::Adopted,
                        source: Some(kernel::events::TaskContinuationSource::SystemFollowUp),
                    },
                ],
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
                vec![
                    kernel::events::TaskContinuationDecisionTraceEntry {
                        stage:
                            kernel::events::TaskContinuationDecisionStage::BeforeFinalResponseHook,
                        decision: kernel::events::TaskContinuationDecisionKind::Continue,
                        source: None,
                    },
                    kernel::events::TaskContinuationDecisionTraceEntry {
                        stage: kernel::events::TaskContinuationDecisionStage::FinalDecision,
                        decision: kernel::events::TaskContinuationDecisionKind::Finished,
                        source: Some(kernel::events::TaskContinuationSource::TaskCompleted),
                    },
                ],
            ),
        ]
    );
}

#[tokio::test]
async fn runner_can_generate_system_follow_up_from_loop_result() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first", usage(6)),
        ModelResponse::text("resolver follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(
            AgentLoopConfig::default().with_continuation_resolver(|loop_result| {
                if loop_result.final_text == "first" {
                    Some(SessionContinuationRequest::SystemFollowUp {
                        input: "resolver asks for another turn".to_string(),
                    })
                } else {
                    None
                }
            }),
        );

    let result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("runner should honor runtime-generated system follow-up continuations");

    assert_eq!(result.text, "resolver follow up complete");

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => Some((action, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
async fn runner_can_generate_system_follow_up_from_turn_completion_hook() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first", usage(6)),
        ModelResponse::text("hook follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig::default().with_continuation_hook(|hook| {
            if hook.phase == kernel::runtime::ContinuationHookPhase::TurnCompleted
                && hook.loop_result.final_text == "first"
            {
                Some(SessionContinuationRequest::SystemFollowUp {
                    input: "hook asks for another turn".to_string(),
                })
            } else {
                None
            }
        }));

    let result = runner
        .run_request(RunRequest::new(
            session_id,
            thread_id.clone(),
            "first input",
        ))
        .await
        .expect("runner should honor turn-completion hook continuations");

    assert_eq!(result.text, "hook follow up complete");

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => Some((action, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
async fn runner_can_generate_system_follow_up_from_tool_batch_completed_hook() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("need tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("tool-batch hook follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig::default().with_continuation_hook(|hook| {
            if hook.phase == kernel::runtime::ContinuationHookPhase::ToolBatchCompleted
                && hook.iteration == 1
                && hook.tool_batch_summary.as_ref().is_some_and(|summary| {
                    summary.entries.len() == 1
                        && summary.entries[0].name == "echo"
                        && summary.entries[0].output_summary == "hello"
                })
            {
                Some(SessionContinuationRequest::SystemFollowUp {
                    input: "tool batch hook asks for another turn".to_string(),
                })
            } else {
                None
            }
        }));

    let result = runner
        .run_request(RunRequest::new(session_id, thread_id, "first input"))
        .await
        .expect("runner should honor tool-batch-completed hook continuations");

    assert_eq!(result.text, "tool-batch hook follow up complete");

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => Some((action, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
async fn runner_can_generate_system_follow_up_from_before_final_response_hook() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::text("first", usage(6)),
        ModelResponse::text("before-final hook follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let router = Arc::new(ToolRegistryBuilder::new().build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(AgentLoopConfig::default().with_continuation_hook(|hook| {
            if hook.phase == kernel::runtime::ContinuationHookPhase::BeforeFinalResponse
                && hook.loop_result.final_text == "first"
            {
                Some(SessionContinuationRequest::SystemFollowUp {
                    input: "before final response hook asks for another turn".to_string(),
                })
            } else {
                None
            }
        }));

    let result = runner
        .run_request(RunRequest::new(session_id, thread_id, "first input"))
        .await
        .expect("runner should honor before-final-response hook continuations");

    assert_eq!(result.text, "before-final hook follow up complete");

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => Some((action, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
/// Verifies that later hook phases can inspect an earlier requested continuation and inflight state.
async fn runner_exposes_existing_requested_continuation_to_later_hook_phases() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("need tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("later-phase hook follow up complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let observations = Arc::new(std::sync::Mutex::new(Vec::<(String, bool, usize)>::new()));
    let observed_hook_state = Arc::clone(&observations);
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(
            AgentLoopConfig::default().with_continuation_hook(move |hook| {
                if hook.phase == kernel::runtime::ContinuationHookPhase::ToolBatchCompleted {
                    return Some(SessionContinuationRequest::SystemFollowUp {
                        input: "earlier hook asks for another turn".to_string(),
                    });
                }

                if hook.phase == kernel::runtime::ContinuationHookPhase::BeforeFinalResponse {
                    observed_hook_state
                        .lock()
                        .expect("hook observations should lock")
                        .push((
                            hook.loop_result.final_text.clone(),
                            hook.requested_continuation.is_some(),
                            hook.inflight_snapshot.completed_handles.len(),
                        ));
                }

                None
            }),
        );

    let result = runner
        .run_request(RunRequest::new(session_id, thread_id, "first input"))
        .await
        .expect("runner should preserve earlier hook continuations for later phases");

    assert_eq!(result.text, "later-phase hook follow up complete");
    assert!(
        observations
            .lock()
            .expect("hook observations should lock")
            .contains(&("first turn complete".to_string(), true, 1)),
        "later hook phases should observe the continuation requested earlier in the same turn"
    );
}

#[tokio::test]
/// Verifies that a later hook phase can explicitly replace an earlier continuation request.
async fn runner_allows_later_hook_phases_to_replace_existing_continuations() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("need tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("replacement continuation complete", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(
            AgentLoopConfig::default().with_continuation_decision_hook(|hook| {
                if hook.phase == kernel::runtime::ContinuationHookPhase::ToolBatchCompleted {
                    return kernel::runtime::ContinuationHookDecision::Request(
                        SessionContinuationRequest::SystemFollowUp {
                            input: "earlier hook request".to_string(),
                        },
                    );
                }

                if hook.phase == kernel::runtime::ContinuationHookPhase::BeforeFinalResponse
                    && hook.loop_result.final_text == "first turn complete"
                {
                    return kernel::runtime::ContinuationHookDecision::Replace(
                        SessionContinuationRequest::SystemFollowUp {
                            input: "replacement continuation".to_string(),
                        },
                    );
                }

                kernel::runtime::ContinuationHookDecision::Continue
            }),
        );

    let result = runner
        .run_request(RunRequest::new(session_id, thread_id, "first input"))
        .await
        .expect("later hook phases should be able to replace earlier continuations");

    assert_eq!(result.text, "replacement continuation complete");

    let continuation_decisions = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { action, source, .. } => Some((action, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_decisions,
        vec![
            (
                kernel::events::TaskContinuationAction::Continue,
                kernel::events::TaskContinuationSource::SystemFollowUp,
            ),
            (
                kernel::events::TaskContinuationAction::Finish,
                kernel::events::TaskContinuationSource::TaskCompleted,
            ),
        ]
    );
}

#[tokio::test]
/// Verifies that the turn-completed hook can still replace an earlier continuation request.
async fn runner_turn_completed_hook_can_replace_existing_requested_continuation() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("need tool".to_string()),
            vec![ToolCallRequest::new(
                "call_1",
                "echo",
                serde_json::json!({"text": "hello"}),
            )],
            usage(10),
        ),
        ModelResponse::text("first turn complete", usage(6)),
        ModelResponse::text("turn completed replacement", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, Arc::clone(&store), router, Arc::clone(&sink))
        .with_config(
            AgentLoopConfig::default().with_continuation_decision_hook(|hook| {
                if hook.phase == kernel::runtime::ContinuationHookPhase::ToolBatchCompleted {
                    return kernel::runtime::ContinuationHookDecision::Request(
                        SessionContinuationRequest::SystemFollowUp {
                            input: "earlier hook request".to_string(),
                        },
                    );
                }

                if hook.phase == kernel::runtime::ContinuationHookPhase::TurnCompleted
                    && hook.loop_result.final_text == "first turn complete"
                {
                    return kernel::runtime::ContinuationHookDecision::Replace(
                        SessionContinuationRequest::SystemFollowUp {
                            input: "replacement from turn completed".to_string(),
                        },
                    );
                }

                kernel::runtime::ContinuationHookDecision::Continue
            }),
        );

    let result = runner
        .run_request(RunRequest::new(session_id, thread_id, "first input"))
        .await
        .expect("turn-completed hook should be able to replace earlier continuations");

    assert_eq!(result.text, "turn completed replacement");

    let continuation_traces = sink
        .snapshot()
        .await
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::TaskContinuationDecided { decision_trace, .. } => Some(decision_trace),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(continuation_traces.iter().any(|trace| {
        trace.iter().any(|entry| {
            entry.stage == kernel::events::TaskContinuationDecisionStage::TurnCompletedHook
                && entry.decision == kernel::events::TaskContinuationDecisionKind::Replace
                && entry.source == Some(kernel::events::TaskContinuationSource::SystemFollowUp)
        })
    }));
}

#[tokio::test]
async fn runner_only_marks_the_active_serial_tool_call_running() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("serial tools".to_string()),
            vec![
                ToolCallRequest::new("call_1", "echo", serde_json::json!({"text": "hello"})),
                ToolCallRequest::new("call_2", "echo", serde_json::json!({"text": "world"})),
            ],
            usage(10),
        ),
        ModelResponse::text("done", usage(6)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let start_barrier = Arc::new(Barrier::new(2));
    let resume_barrier = Arc::new(Barrier::new(2));
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(FirstCallBlockingEchoTool {
        start_barrier: Arc::clone(&start_barrier),
        resume_barrier: Arc::clone(&resume_barrier),
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, Arc::clone(&sink)).with_config(
        AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        }
        .with_tool_execution_mode(kernel::tools::executor::ToolExecutionMode::Serial),
    );

    let run_future = runner.run_request(RunRequest::new(
        SessionId::new(),
        ThreadId::new(),
        "serial tools",
    ));
    let observer = async {
        start_barrier.wait().await;
        let events = sink.snapshot().await;
        let running_handles = events
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCallInFlightStateUpdated {
                    iteration,
                    tool_id,
                    state,
                    ..
                } if iteration == Some(1)
                    && state == kernel::events::ToolCallInFlightState::Running =>
                {
                    Some(tool_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        resume_barrier.wait().await;
        running_handles
    };

    let (result, running_handles) = tokio::join!(run_future, observer);
    result.expect("runner should finish once the blocking tool is released");
    assert_eq!(
        running_handles,
        vec!["call_1".to_string()],
        "serial execution should only mark the currently executing call as running"
    );
}

#[tokio::test]
async fn runner_attaches_inflight_snapshot_to_max_iteration_errors() {
    let model = Arc::new(SequenceModel::new(vec![ModelResponse::tool_calls(
        Some("first tool".to_string()),
        vec![ToolCallRequest::new(
            "call_1",
            "echo",
            serde_json::json!({"text": "hello"}),
        )],
        usage(10),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 1,
        max_tool_calls: 4,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        ..AgentLoopConfig::default()
    });

    let error = runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "run once",
        ))
        .await
        .expect_err("runner should hit the max-iterations guard after the first tool iteration");

    let inflight_snapshot = match error {
        Error::Runtime {
            stage,
            inflight_snapshot: Some(snapshot),
            ..
        } if stage == "agent-loop-max-iterations" => snapshot,
        other => panic!("expected max-iterations runtime error with snapshot, got {other:?}"),
    };
    assert_eq!(inflight_snapshot.completed_handles.len(), 1);
    assert_eq!(inflight_snapshot.entries.len(), 1);
    assert_eq!(
        inflight_snapshot.entries[0].output_summary.as_deref(),
        Some("hello")
    );
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
        ThreadRuntime::new(model, store, router, sink.clone()).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        ))
        .await
        .unwrap();

    let events = sink.snapshot().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelResponseCreated { iteration } if *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::Message { text },
            iteration,
        } if text.is_empty() && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelTextDelta { text, iteration } if text == "calling echo twice" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::ToolCall { item_id, .. },
            iteration,
        } if item_id == "call_1" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemUpdated {
            item: ResponseItem::ToolCall { item_id, name, .. },
            iteration,
        } if item_id == "call_1" && name == "echo" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemDone {
            item: ResponseItem::ToolCall { item_id, .. },
            iteration,
        } if item_id == "call_1" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::ToolCall { item_id, .. },
            iteration,
        } if item_id == "call_2" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemUpdated {
            item: ResponseItem::ToolCall { item_id, name, .. },
            iteration,
        } if item_id == "call_2" && name == "echo" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemDone {
            item: ResponseItem::ToolCall { item_id, .. },
            iteration,
        } if item_id == "call_2" && *iteration == Some(1)
    )));
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
async fn runner_can_drain_tool_queue_in_parallel_when_configured() {
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

    let barrier = Arc::new(Barrier::new(2));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_configured_handler_spec(
        Arc::new(BarrierEchoTool {
            barrier: Arc::clone(&barrier),
        }),
        true,
    );
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());

    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 4,
        max_tool_calls: 4,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        tool_execution_mode: kernel::tools::executor::ToolExecutionMode::Parallel,
        ..AgentLoopConfig::default()
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runner.run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        )),
    )
    .await
    .expect("parallel tool drain should not deadlock")
    .unwrap();

    assert_eq!(result.text, "done");
}

#[tokio::test]
async fn runner_parallel_mode_only_parallelizes_tools_marked_safe() {
    let model = Arc::new(SequenceModel::new(vec![
        ModelResponse::tool_calls(
            Some("calling mixed tools".to_string()),
            vec![
                ToolCallRequest::new(
                    "call_1",
                    "parallel_echo_a",
                    serde_json::json!({"text": "hello"}),
                ),
                ToolCallRequest::new(
                    "call_2",
                    "parallel_echo_b",
                    serde_json::json!({"text": "world"}),
                ),
                ToolCallRequest::new(
                    "call_3",
                    "serial_echo",
                    serde_json::json!({"text": "after"}),
                ),
            ],
            usage(10),
        ),
        ModelResponse::text("done", usage(6)),
    ]));

    let barrier = Arc::new(Barrier::new(2));
    let active_parallel_tools = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_configured_handler_spec(
        Arc::new(NamedParallelBarrierTool {
            name: "parallel_echo_a",
            barrier: Arc::clone(&barrier),
            active_parallel_tools: Arc::clone(&active_parallel_tools),
        }),
        true,
    );
    builder.push_configured_handler_spec(
        Arc::new(NamedParallelBarrierTool {
            name: "parallel_echo_b",
            barrier: Arc::clone(&barrier),
            active_parallel_tools: Arc::clone(&active_parallel_tools),
        }),
        true,
    );
    builder.push_handler_spec(Arc::new(StrictSerialEchoTool {
        active_parallel_tools: Arc::clone(&active_parallel_tools),
    }));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());

    let runner = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
        max_iterations: 4,
        max_tool_calls: 4,
        recent_message_limit: 20,
        tool_choice: llm::completion::message::ToolChoice::Auto,
        tool_execution_mode: kernel::tools::executor::ToolExecutionMode::Parallel,
        ..AgentLoopConfig::default()
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runner.run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello twice",
        )),
    )
    .await
    .expect("mixed parallel drain should not deadlock")
    .unwrap();

    assert_eq!(result.text, "done");
}

#[tokio::test]
async fn runner_passes_previous_response_id_to_follow_up_requests() {
    let model = Arc::new(RecordingModel::new(vec![
        ModelResponse {
            output: kernel::model::ModelOutput::ToolCalls {
                text: Some("calling echo".to_string()),
                calls: vec![ToolCallRequest::new(
                    "call_1",
                    "echo",
                    serde_json::json!({"text": "hello"}),
                )],
            },
            usage: usage(10),
            message_id: Some("resp_1".to_string()),
        },
        ModelResponse {
            output: kernel::model::ModelOutput::Text("done".to_string()),
            usage: usage(6),
            message_id: Some("resp_2".to_string()),
        },
    ]));

    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runner =
        ThreadRuntime::new(Arc::clone(&model), store, router, sink).with_config(AgentLoopConfig {
            max_iterations: 4,
            max_tool_calls: 4,
            recent_message_limit: 20,
            tool_choice: llm::completion::message::ToolChoice::Auto,
            ..AgentLoopConfig::default()
        });

    runner
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "say hello",
        ))
        .await
        .unwrap();

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].previous_response_id, None);
    assert_eq!(requests[1].previous_response_id.as_deref(), Some("resp_1"));
}

/// Verifies explicit skill mentions append the skill list and inject selected skill contents.
#[tokio::test]
async fn runner_adds_available_skills_to_system_prompt_and_injects_explicit_mentions() {
    let skill_root = write_test_skill_root();
    let model = Arc::new(RecordingModel::new(vec![ModelResponse::text(
        "done",
        usage(4),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(PromptReadTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runtime =
        ThreadRuntime::new(Arc::clone(&model), store, router, sink).with_config(AgentLoopConfig {
            skills: skills::SkillConfig {
                roots: vec![skill_root.path().to_path_buf()],
                cwd: None,
                enabled: true,
            },
            ..AgentLoopConfig::default()
        });

    runtime
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "Use $rust-error-snafu for this error type",
        ))
        .await
        .unwrap();

    let requests = model.requests().await;
    let request = requests.first().expect("model should receive one request");
    let system_prompt = request
        .system_prompt
        .as_ref()
        .expect("skills should create a system prompt");

    assert!(system_prompt.contains("<available_skills>"));
    assert!(system_prompt.contains("<name>rust-error-snafu</name>"));
    assert!(system_prompt.contains("<description>Typed Rust errors.</description>"));
    assert!(
        request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("<skill_instructions"))
    );
    assert!(
        request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("Use SNAFU context."))
    );
    assert_eq!(
        request.messages.last(),
        Some(&Message::user("Use $rust-error-snafu for this error type"))
    );
}

/// Verifies unmentioned skills are listed but their full bodies are not injected.
#[tokio::test]
async fn runner_lists_available_skills_without_injecting_unmentioned_skill_bodies() {
    let skill_root = write_test_skill_root();
    let model = Arc::new(RecordingModel::new(vec![ModelResponse::text(
        "done",
        usage(4),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(PromptReadTool));
    let router = Arc::new(builder.build_router());
    let sink = Arc::new(RecordingEventSink::default());
    let runtime =
        ThreadRuntime::new(Arc::clone(&model), store, router, sink).with_config(AgentLoopConfig {
            skills: skills::SkillConfig {
                roots: vec![skill_root.path().to_path_buf()],
                cwd: None,
                enabled: true,
            },
            ..AgentLoopConfig::default()
        });

    runtime
        .run_request(RunRequest::new(
            SessionId::new(),
            ThreadId::new(),
            "Explain a normal Rust error enum",
        ))
        .await
        .unwrap();

    let requests = model.requests().await;
    let request = requests.first().expect("model should receive one request");

    assert!(
        request
            .system_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.contains("rust-error-snafu"))
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("<skill_instructions"))
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("Use SNAFU context."))
    );
}

/// Verifies one thread reuses its built system prompt until the cache is explicitly invalidated.
#[tokio::test]
async fn runner_reuses_cached_system_prompt_across_turns() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    fs::write(temp.path().join("AGENTS.md"), "project instructions v1").expect("AGENTS.md");
    let model = Arc::new(RecordingModel::new(vec![
        ModelResponse::text("first", usage(4)),
        ModelResponse::text("second", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::clone(&model),
        Arc::clone(&store),
        Arc::new(ToolRouter::new(
            Arc::new(kernel::tools::ToolRegistry::default()),
            Vec::new(),
        )),
        Arc::new(RecordingEventSink::default()),
    );
    let thread =
        ThreadHandle::new(SessionId::new(), ThreadId::new()).with_cwd(temp.path().to_path_buf());

    runtime
        .run(&thread, ThreadRunRequest::new("first"))
        .await
        .unwrap();
    fs::write(temp.path().join("AGENTS.md"), "project instructions v2").expect("AGENTS.md");
    runtime
        .run(&thread, ThreadRunRequest::new("second"))
        .await
        .unwrap();

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].system_prompt, requests[1].system_prompt);
    assert!(
        requests[1]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v1"))
    );
    assert!(
        !requests[1]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v2"))
    );
}

/// Verifies invalidating a thread cache forces the next turn to rebuild the system prompt.
#[tokio::test]
async fn runner_rebuilds_system_prompt_after_cache_invalidation() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    fs::write(temp.path().join("AGENTS.md"), "project instructions v1").expect("AGENTS.md");
    let model = Arc::new(RecordingModel::new(vec![
        ModelResponse::text("first", usage(4)),
        ModelResponse::text("second", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::clone(&model),
        Arc::clone(&store),
        Arc::new(ToolRouter::new(
            Arc::new(kernel::tools::ToolRegistry::default()),
            Vec::new(),
        )),
        Arc::new(RecordingEventSink::default()),
    );
    let thread =
        ThreadHandle::new(SessionId::new(), ThreadId::new()).with_cwd(temp.path().to_path_buf());

    runtime
        .run(&thread, ThreadRunRequest::new("first"))
        .await
        .unwrap();
    fs::write(temp.path().join("AGENTS.md"), "project instructions v2").expect("AGENTS.md");
    runtime.expire_system_prompt(&thread).await;
    runtime
        .run(&thread, ThreadRunRequest::new("second"))
        .await
        .unwrap();

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v1"))
    );
    assert!(
        requests[1]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v2"))
    );
}

/// Verifies one-shot prompt overrides bypass the cache without replacing the cached default prompt.
#[tokio::test]
async fn runner_request_prompt_overrides_do_not_replace_cached_system_prompt() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    fs::write(temp.path().join("AGENTS.md"), "project instructions v1").expect("AGENTS.md");
    let model = Arc::new(RecordingModel::new(vec![
        ModelResponse::text("first", usage(4)),
        ModelResponse::text("override", usage(4)),
        ModelResponse::text("third", usage(4)),
    ]));
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::clone(&model),
        Arc::clone(&store),
        Arc::new(ToolRouter::new(
            Arc::new(kernel::tools::ToolRegistry::default()),
            Vec::new(),
        )),
        Arc::new(RecordingEventSink::default()),
    );
    let thread =
        ThreadHandle::new(SessionId::new(), ThreadId::new()).with_cwd(temp.path().to_path_buf());

    runtime
        .run(&thread, ThreadRunRequest::new("first"))
        .await
        .unwrap();
    fs::write(temp.path().join("AGENTS.md"), "project instructions v2").expect("AGENTS.md");
    runtime
        .run(
            &thread,
            ThreadRunRequest {
                inputs: vec![UserInput::text("override")],
                system_prompt_override: Some("override prompt".to_string()),
                append_system_prompt_override: None,
            },
        )
        .await
        .unwrap();
    runtime
        .run(&thread, ThreadRunRequest::new("third"))
        .await
        .unwrap();

    let requests = model.requests().await;
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.starts_with("override prompt"))
    );
    assert!(
        requests[2]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v1"))
    );
    assert!(
        !requests[2]
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("project instructions v2"))
    );
}

/// Verifies structured skill input injects a skill without requiring `$skill-name` text.
#[tokio::test]
async fn runner_injects_structured_skill_input_without_text_mention() {
    let skill_root = write_test_skill_root();
    let model = Arc::new(RecordingModel::new(vec![ModelResponse::text(
        "done",
        usage(4),
    )]));
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(ToolRouter::new(
        Arc::new(kernel::tools::ToolRegistry::default()),
        Vec::new(),
    ));
    let sink = Arc::new(RecordingEventSink::default());
    let skill_path = skill_root.path().join("rust-error-snafu/SKILL.md");
    let runtime =
        ThreadRuntime::new(Arc::clone(&model), store, router, sink).with_config(AgentLoopConfig {
            skills: skills::SkillConfig {
                roots: vec![skill_root.path().to_path_buf()],
                cwd: None,
                enabled: true,
            },
            ..AgentLoopConfig::default()
        });

    runtime
        .run_request(RunRequest::from_inputs(
            SessionId::new(),
            ThreadId::new(),
            vec![
                UserInput::skill("rust-error-snafu", skill_path),
                UserInput::text("create an error enum"),
            ],
        ))
        .await
        .unwrap();

    let requests = model.requests().await;
    let request = requests.first().expect("model should receive one request");

    assert!(
        request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("<skill_instructions"))
    );
    assert!(
        request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("Use SNAFU context."))
    );
    assert_eq!(
        request.messages.last(),
        Some(&Message::user("create an error enum"))
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|message| first_user_text(message).contains("[skill:rust-error-snafu]"))
    );
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
    let thread = ThreadHandle::new(session_id, thread_id.clone())
        .with_system_prompt("You are a helpful agent.");
    let runtime = ThreadRuntime::new(
        model,
        store.clone(),
        Arc::new(ToolRouter::new(registry, Vec::new())),
        sink.clone(),
    );

    let result = runtime
        .run(&thread, ThreadRunRequest::new("say hello"))
        .await
        .unwrap();

    assert_eq!(result.text, "hello from agent");
    assert_eq!(result.usage.total_tokens, 8);

    let messages = store
        .load_messages_state(session_id, thread_id, 20)
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
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelResponseCreated { iteration } if *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemAdded {
            item: ResponseItem::Message { text },
            iteration,
        } if text.is_empty() && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelTextDelta { text, iteration }
            if text == "hello from agent" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelOutputItemDone {
            item: ResponseItem::Message { text },
            iteration,
        } if text == "hello from agent" && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelStreamCompleted { usage, iteration, .. }
            if usage.total_tokens == 8 && *iteration == Some(1)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusUpdated { stage, iteration, .. }
            if *stage == AgentStage::Responding && *iteration == Some(1)
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextProduced { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::RunFinished { .. }))
    );
}

#[tokio::test]
async fn agent_context_can_fork_child_identity_for_a_dedicated_thread() {
    let parent = TurnContext::new(SessionId::new(), ThreadId::new()).with_name("planner");
    let child = parent.fork("reviewer");

    assert_eq!(child.session_id, parent.session_id);
    assert_eq!(
        child.parent_agent_id.as_deref(),
        Some(parent.agent_id.as_str())
    );
    assert_eq!(child.name.as_deref(), Some("reviewer"));
    assert_ne!(child.agent_id, parent.agent_id);
    assert_ne!(child.thread_id, parent.thread_id);
}

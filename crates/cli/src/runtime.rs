use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use kernel::{
    AgentLoopConfig, Result, SessionTaskContext, ThreadHandle, ThreadRunRequest, ThreadRuntime,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    model::AgentModel,
    session::{SessionId, ThreadId},
    tools::router::ToolRouter,
};
use tracing::{info, trace};

/// Builds a short prompt preview so tracing stays readable for long requests.
pub fn prompt_preview(prompt: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut preview = prompt.trim().replace('\n', " ");
    if preview.chars().count() > MAX_CHARS {
        preview = format!("{}...", preview.chars().take(MAX_CHARS).collect::<String>());
    }
    preview
}

/// Formats the continuation decision trace as a compact CLI log field.
fn format_continuation_decision_trace(
    decision_trace: &[TaskContinuationDecisionTraceEntry],
) -> String {
    decision_trace
        .iter()
        .map(|entry| match &entry.source {
            Some(source) => format!("{:?}:{:?}:{:?}", entry.stage, entry.decision, source),
            None => format!("{:?}:{:?}", entry.stage, entry.decision),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Formats the continuation decision trace as a multi-line CLI summary.
fn format_continuation_decision_trace_multiline(
    decision_trace: &[TaskContinuationDecisionTraceEntry],
) -> String {
    if decision_trace.is_empty() {
        return "  (no continuation trace)".to_string();
    }

    decision_trace
        .iter()
        .enumerate()
        .map(|(index, entry)| match &entry.source {
            Some(source) => format!(
                "  {}. {:?} -> {:?} ({:?})",
                index + 1,
                entry.stage,
                entry.decision,
                source
            ),
            None => format!("  {}. {:?} -> {:?}", index + 1, entry.stage, entry.decision),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
#[derive(Default)]
struct CliPresentationState {
    text_line_open: bool,
    reasoning_line_open: bool,
}

type SharedCliWriter = Arc<Mutex<Box<dyn Write + Send>>>;

impl TracingEventSink {
    /// Builds the default sink that streams live output to stdout.
    pub(crate) fn stdout() -> Self {
        Self::with_writer(Arc::new(Mutex::new(Box::new(std::io::stdout()))))
    }

    /// Builds a sink backed by the provided writer so tests can capture rendered output.
    pub(crate) fn with_writer(writer: SharedCliWriter) -> Self {
        Self {
            writer,
            state: Mutex::new(CliPresentationState::default()),
        }
    }

    /// Writes streamed assistant text directly to the terminal and keeps the line open.
    fn write_text_delta(&self, text: &str) {
        if text.is_empty() {
            return;
        }

        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.reasoning_line_open {
            let _ = writeln!(writer);
            let _ = writeln!(writer, "[think end]");
            let _ = writeln!(writer, "[answer]");
            state.reasoning_line_open = false;
        }
        let _ = write!(writer, "{text}");
        let _ = writer.flush();
        state.text_line_open = true;
    }

    /// Writes streamed reasoning content before visible answer text.
    fn write_reasoning_delta(&self, text: &str) {
        if text.is_empty() {
            return;
        }

        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.text_line_open {
            let _ = writeln!(writer);
            state.text_line_open = false;
        }
        if !state.reasoning_line_open {
            let _ = writeln!(writer, "[think]");
        }
        let _ = write!(writer, "{text}");
        let _ = writer.flush();
        state.reasoning_line_open = true;
    }

    /// Writes a standalone status line, first closing any open streamed text line.
    fn write_status_line(&self, line: &str) {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.text_line_open || state.reasoning_line_open {
            let _ = writeln!(writer);
        }
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
        state.text_line_open = false;
        state.reasoning_line_open = false;
    }

    /// Closes the current streamed text line so the next prompt starts on a clean line.
    fn finish_text_line_if_needed(&self) {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.text_line_open || state.reasoning_line_open {
            let _ = writeln!(writer);
            let _ = writer.flush();
            state.text_line_open = false;
            state.reasoning_line_open = false;
        }
    }
}

pub(crate) struct TracingEventSink {
    writer: SharedCliWriter,
    state: Mutex<CliPresentationState>,
}

#[async_trait::async_trait]
impl EventSink for TracingEventSink {
    /// Emits runtime events into the CLI tracing stream for interactive debugging.
    async fn publish(&self, event: AgentEvent) {
        // Emit the raw event first so `RUST_LOG` alone is enough to inspect the
        // exact runtime protocol without adding a separate CLI mode or flag.
        info!(event = ?event, "agent event");
        match event {
            AgentEvent::RunStarted {
                session_id,
                thread_id,
                input,
            } => {
                info!(session_id, thread_id, prompt = %prompt_preview(&input), "agent run started");
            }
            AgentEvent::StatusUpdated {
                stage,
                message,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(stage = ?stage, iteration, message, tool_id, tool_call_id, "agent status updated");
            }
            AgentEvent::ToolStatusUpdated {
                stage,
                name,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(stage = ?stage, name, iteration, tool_id, tool_call_id, "tool status updated");
            }
            AgentEvent::ModelRequested {
                message_count,
                tool_count,
            } => {
                info!(message_count, tool_count, "requesting model completion");
            }
            AgentEvent::ModelResponseCreated { iteration } => {
                info!(iteration, "model response stream created");
            }
            AgentEvent::ModelTextDelta { text, iteration } => {
                trace!(iteration, text = %text, "model text delta");
                self.write_text_delta(&text);
            }
            AgentEvent::ModelReasoningSummaryDelta {
                id,
                text,
                summary_index,
                iteration,
            } => {
                info!(iteration, id, summary_index, text = %text, "model reasoning summary delta");
            }
            AgentEvent::ModelReasoningContentDelta {
                id,
                text,
                content_index,
                iteration,
            } => {
                info!(iteration, id, content_index, text = %text, "model reasoning content delta");
                self.write_reasoning_delta(&text);
            }
            AgentEvent::ModelToolCallNameDelta {
                tool_id,
                tool_call_id,
                delta,
                iteration,
            } => {
                info!(
                    iteration,
                    tool_id, tool_call_id, delta, "model tool call name delta"
                );
            }
            AgentEvent::ModelToolCallArgumentsDelta {
                tool_id,
                tool_call_id,
                delta,
                iteration,
            } => {
                info!(
                    iteration,
                    tool_id, tool_call_id, delta, "model tool call arguments delta"
                );
            }
            AgentEvent::ModelOutputItemAdded { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item added"
                );
            }
            AgentEvent::ModelOutputItemUpdated { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item updated"
                );
            }
            AgentEvent::ModelOutputItemDone { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item completed"
                );
            }
            AgentEvent::ModelStreamCompleted {
                message_id,
                usage,
                iteration,
            } => {
                info!(
                    iteration,
                    message_id,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "model stream completed"
                );
            }
            AgentEvent::ToolCallQueued {
                name,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(name, iteration, tool_id, tool_call_id, "tool call queued");
            }
            AgentEvent::ToolCallInFlightRegistered {
                name,
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
            } => {
                info!(
                    name,
                    iteration, tool_id, tool_call_id, handle_id, "tool call registered in-flight"
                );
            }
            AgentEvent::ToolCallInFlightStateUpdated {
                name,
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
                state,
                error_summary,
            } => {
                info!(
                    name,
                    iteration,
                    tool_id,
                    tool_call_id,
                    handle_id,
                    state = ?state,
                    error_summary,
                    "tool call in-flight state updated"
                );
            }
            AgentEvent::ToolCallInFlightSnapshot {
                iteration,
                queued_handles,
                running_handles,
                completed_handles,
                cancelled_handles,
                failed_handles,
            } => {
                info!(
                    iteration,
                    queued = queued_handles.len(),
                    running = running_handles.len(),
                    completed = completed_handles.len(),
                    cancelled = cancelled_handles.len(),
                    failed = failed_handles.len(),
                    "tool call in-flight snapshot"
                );
            }
            AgentEvent::ToolCallRequested {
                name,
                handle_id,
                arguments,
            } => {
                info!(tool = %name, handle_id, arguments = %arguments, "tool requested");
                self.write_status_line(&format!("[tool] {name} started"));
            }
            AgentEvent::ToolCallCompleted {
                name,
                handle_id,
                output,
                structured_output,
            } => {
                if let Some(structured_output) = structured_output {
                    info!(
                        tool = %name,
                        handle_id,
                        output = %output,
                        structured_output = %structured_output,
                        "tool completed"
                    );
                } else {
                    info!(tool = %name, handle_id, output = %output, "tool completed");
                }
                self.write_status_line(&format!("[tool] {name} completed"));
            }
            AgentEvent::TextProduced { text } => {
                info!(text = %text, "model produced final text");
            }
            AgentEvent::TaskContinuationDecided {
                turn_index,
                action,
                source,
                decision_trace,
            } => {
                let trace = format_continuation_decision_trace(&decision_trace);
                let trace_pretty = format_continuation_decision_trace_multiline(&decision_trace);
                info!(
                    turn_index,
                    action = ?action,
                    source = ?source,
                    trace_len = decision_trace.len(),
                    trace = %trace,
                    trace_pretty = %trace_pretty,
                    "task continuation decided"
                );
            }
            AgentEvent::RunFinished { text, usage } => {
                self.finish_text_line_if_needed();
                info!(
                    text = %text,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "agent run finished"
                );
            }
        }
    }
}

/// Builds the default CLI thread handle so one-shot and REPL modes share the same defaults.
pub fn build_cli_thread_handle() -> ThreadHandle {
    ThreadHandle::new(SessionId::new(), ThreadId::new()).with_system_prompt(
        "You are a helpful agent. Use `apply_patch` for file edits and use `exec_command` or `write_stdin` only when command execution is required. Keep file changes inside the workspace, avoid paths containing `..`, and answer directly only when no tool action is needed.",
    )
}

/// Runs one CLI turn through an existing thread/runtime pair.
pub async fn run_cli_turn<M, E>(
    runtime: &ThreadRuntime<M, E>,
    thread: &ThreadHandle,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    let result = runtime.run(thread, ThreadRunRequest::new(prompt)).await?;
    Ok(result.text)
}

/// Runs one CLI prompt through the kernel runtime so CLI and kernel share the same path.
pub async fn run_cli_prompt<M>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    prompt: String,
    skills: skills::SkillConfig,
) -> Result<String>
where
    M: AgentModel + 'static,
{
    let thread = build_cli_thread_handle();
    let runtime = ThreadRuntime::new(model, store, router, Arc::new(TracingEventSink::stdout()))
        .with_config(AgentLoopConfig {
            skills,
            ..AgentLoopConfig::default()
        });
    run_cli_turn(&runtime, &thread, prompt).await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs, io,
        sync::{Arc, Mutex as StdMutex},
    };

    use async_trait::async_trait;
    use kernel::{
        Result, ThreadRuntime,
        events::{AgentEvent, EventSink},
        events::{
            TaskContinuationDecisionKind, TaskContinuationDecisionStage,
            TaskContinuationDecisionTraceEntry, TaskContinuationSource,
        },
        model::{AgentModel, ModelRequest, ModelResponse},
        session::InMemorySessionStore,
        tools::ToolRouter,
    };
    use llm::usage::Usage;
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::{
        TracingEventSink, build_cli_thread_handle, format_continuation_decision_trace,
        format_continuation_decision_trace_multiline, run_cli_prompt, run_cli_turn,
    };

    #[derive(Clone, Default)]
    struct SharedBufferWriter {
        buffer: Arc<StdMutex<Vec<u8>>>,
    }

    impl SharedBufferWriter {
        /// Returns the captured UTF-8 text written through this shared writer.
        fn rendered(&self) -> String {
            String::from_utf8(
                self.buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            )
            .expect("buffer should be utf8")
        }
    }

    impl io::Write for SharedBufferWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct StubAgentModel;

    #[derive(Clone)]
    struct RecordingTwoTurnModel {
        requests: Arc<Mutex<Vec<ModelRequest>>>,
        responses: Arc<Mutex<VecDeque<ModelResponse>>>,
    }

    impl RecordingTwoTurnModel {
        /// Builds a model double that records both turn requests and returns queued responses.
        fn new() -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(
                    vec![
                        ModelResponse::text("first reply", Usage::default()),
                        ModelResponse::text("second reply", Usage::default()),
                    ]
                    .into(),
                )),
            }
        }

        /// Returns the requests observed across both submitted CLI turns.
        async fn requests(&self) -> Vec<ModelRequest> {
            self.requests.lock().await.clone()
        }
    }

    /// Creates a filesystem skill root with one test skill for CLI integration checks.
    fn write_cli_test_skill_root() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let skill_dir = temp.path().join("alpha-skill");
        fs::create_dir_all(&skill_dir).expect("skill dir should be created");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: alpha-skill\ndescription: Alpha test skill.\n---\nUse alpha skill instructions.\n",
        )
        .expect("skill file should be written");
        temp
    }

    /// Extracts the first text block from a user message so tests can inspect prompt context.
    fn first_user_text(message: &llm::completion::Message) -> &str {
        let llm::completion::Message::User { content } = message else {
            panic!("expected user message");
        };
        let llm::completion::message::UserContent::Text(text) = content.first_ref() else {
            panic!("expected text user content");
        };
        text.text()
    }

    #[async_trait(?Send)]
    impl AgentModel for StubAgentModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
            Ok(ModelResponse::text(
                "hello from runtime",
                Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    total_tokens: 3,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            ))
        }
    }

    #[async_trait(?Send)]
    impl AgentModel for RecordingTwoTurnModel {
        async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
            self.requests.lock().await.push(request);
            self.responses
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| kernel::Error::Runtime {
                    message: "recording two-turn model exhausted".to_string(),
                    stage: "cli-two-turn-model-complete".to_string(),
                    inflight_snapshot: None,
                })
        }
    }

    #[tokio::test]
    async fn runs_prompt_through_kernel_agent_runtime() {
        let text = run_cli_prompt(
            Arc::new(StubAgentModel),
            Arc::new(InMemorySessionStore::default()),
            Arc::new(ToolRouter::new(
                Arc::new(kernel::tools::ToolRegistry::default()),
                Vec::new(),
            )),
            "say hello".to_string(),
            skills::SkillConfig::default(),
        )
        .await
        .unwrap();

        assert_eq!(text, "hello from runtime");
    }

    #[tokio::test]
    async fn run_cli_prompt_uses_configured_skill_roots() {
        let skill_root = write_cli_test_skill_root();
        let model = Arc::new(RecordingTwoTurnModel::new());

        run_cli_prompt(
            Arc::clone(&model),
            Arc::new(InMemorySessionStore::default()),
            Arc::new(ToolRouter::new(
                Arc::new(kernel::tools::ToolRegistry::default()),
                Vec::new(),
            )),
            "Use $alpha-skill".to_string(),
            skills::SkillConfig {
                roots: vec![skill_root.path().to_path_buf()],
                cwd: None,
                enabled: true,
            },
        )
        .await
        .unwrap();

        let requests = model.requests().await;
        let request = requests.first().expect("model should receive a request");

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
                .any(|message| first_user_text(message).contains("Use alpha skill instructions."))
        );
    }

    #[tokio::test]
    async fn reuses_one_thread_for_multiple_cli_turns() {
        let model = Arc::new(RecordingTwoTurnModel::new());
        let store = Arc::new(InMemorySessionStore::default());
        let runtime = ThreadRuntime::new(
            Arc::clone(&model),
            Arc::clone(&store),
            Arc::new(ToolRouter::new(
                Arc::new(kernel::tools::ToolRegistry::default()),
                Vec::new(),
            )),
            Arc::new(TracingEventSink::stdout()),
        );
        let thread = build_cli_thread_handle();

        let first = run_cli_turn(&runtime, &thread, "hello".to_string())
            .await
            .unwrap();
        let second = run_cli_turn(&runtime, &thread, "follow up".to_string())
            .await
            .unwrap();

        assert_eq!(first, "first reply");
        assert_eq!(second, "second reply");

        let requests = model.requests().await;
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.len() >= 3);
    }

    #[tokio::test]
    async fn streams_text_deltas_without_waiting_for_run_finished() {
        let writer = SharedBufferWriter::default();
        let sink = TracingEventSink::with_writer(Arc::new(StdMutex::new(
            Box::new(writer.clone()) as Box<dyn io::Write + Send>
        )));

        sink.publish(AgentEvent::ModelTextDelta {
            text: "hel".to_string(),
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::ModelTextDelta {
            text: "lo".to_string(),
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::RunFinished {
            text: "hello".to_string(),
            usage: Usage::default(),
        })
        .await;

        assert_eq!(writer.rendered(), "hello\n");
    }

    #[tokio::test]
    async fn streams_reasoning_content_before_answer_text() {
        let writer = SharedBufferWriter::default();
        let sink = TracingEventSink::with_writer(Arc::new(StdMutex::new(
            Box::new(writer.clone()) as Box<dyn io::Write + Send>
        )));

        sink.publish(AgentEvent::ModelReasoningContentDelta {
            id: None,
            text: "think".to_string(),
            content_index: 0,
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::ModelReasoningContentDelta {
            id: None,
            text: "ing".to_string(),
            content_index: 0,
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::ModelTextDelta {
            text: "answer".to_string(),
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::RunFinished {
            text: "answer".to_string(),
            usage: Usage::default(),
        })
        .await;

        assert_eq!(
            writer.rendered(),
            "[think]\nthinking\n[think end]\n[answer]\nanswer\n"
        );
    }

    #[tokio::test]
    async fn prints_tool_status_lines_on_separate_lines() {
        let writer = SharedBufferWriter::default();
        let sink = TracingEventSink::with_writer(Arc::new(StdMutex::new(
            Box::new(writer.clone()) as Box<dyn io::Write + Send>
        )));

        sink.publish(AgentEvent::ModelTextDelta {
            text: "answer".to_string(),
            iteration: Some(1),
        })
        .await;
        sink.publish(AgentEvent::ToolCallRequested {
            name: "exec_command".to_string(),
            handle_id: "h1".to_string(),
            arguments: json!({}),
        })
        .await;
        sink.publish(AgentEvent::ToolCallCompleted {
            name: "exec_command".to_string(),
            handle_id: "h1".to_string(),
            output: "ok".to_string(),
            structured_output: None,
        })
        .await;

        assert_eq!(
            writer.rendered(),
            "answer\n[tool] exec_command started\n[tool] exec_command completed\n"
        );
    }

    #[test]
    fn formats_continuation_decision_trace_for_cli_logs() {
        let trace = format_continuation_decision_trace(&[
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::ToolBatchCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
        ]);

        assert_eq!(
            trace,
            "ToolBatchCompletedHook:Request:SystemFollowUp | FinalDecision:Adopted:SystemFollowUp"
        );
    }

    #[test]
    fn formats_continuation_decision_trace_for_human_readable_cli_logs() {
        let trace = format_continuation_decision_trace_multiline(&[
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::ToolBatchCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
        ]);

        assert_eq!(
            trace,
            "  1. ToolBatchCompletedHook -> Request (SystemFollowUp)\n  2. FinalDecision -> Adopted (SystemFollowUp)"
        );
    }
}

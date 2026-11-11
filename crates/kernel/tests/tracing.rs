//! Run spans: every top-level Run opens one trace, and queued input stays in its Run.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionError,
    ExtensionModule, ExtensionModuleFactory, ExtensionRegistrar,
    StaticExtensionFactory,
};
use futures::stream;
use kernel::{
    EventSink, InsertionNotice, Kernel, KernelError, KernelFactory, Model,
    ModelError, ModelFactory, ModelStream, NanoidIdGenerator, SinkError,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, ExtensionCommandDefinition, ExtensionDescriptor, ExtensionId,
    ExtensionUserMessage, IdGenerator, IdKind, ModelFinal, ModelProfile,
    ModelRequest, ModelStreamEvent, ModelUsage, QueueKind, RunRequest,
    SessionId, StaticExtensionRegistration, StopReason, ToolCall, ToolCallId,
    UserBashRequest,
};
use store::{JsonlStoreFactory, SessionCreateOptions, SystemClock};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

#[path = "support/spans.rs"]
mod spans;
mod support;

use spans::{line_with, run_fields};
use support::CapturedLogs;

/// Deterministic capabilities of the scripted model.
static PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("scripted".to_string())
        .display_name("Scripted tracing model".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Builds the terminal event of one model reply.
fn finished(stop_reason: StopReason) -> ModelStreamEvent {
    ModelStreamEvent::Finished(
        ModelFinal::builder()
            .stop_reason(stop_reason)
            .raw_stop_reason(None)
            .usage(ModelUsage::default())
            .build(),
    )
}

/// Builds the events of one plain text answer that ends the Turn.
fn answer(text: &str) -> Vec<ModelStreamEvent> {
    vec![
        ModelStreamEvent::TextDelta(text.to_string()),
        finished(StopReason::EndTurn),
    ]
}

/// Builds a reply that runs one harmless `bash` Tool call.
fn bash_call(call_id: &str) -> Vec<ModelStreamEvent> {
    vec![
        ModelStreamEvent::ToolCall(
            ToolCall::builder()
                .tool_call_id(ToolCallId::try_from(call_id).expect("call id"))
                .name("exec_command".to_string())
                .arguments(serde_json::json!({ "cmd": "true", "login": false }))
                .build(),
        ),
        finished(StopReason::ToolUse),
    ]
}

/// One scripted model reply, optionally held until the test releases it.
struct Reply {
    hold: Option<Arc<Notify>>,
    events: Vec<ModelStreamEvent>,
}

/// Answers each request with the next scripted reply, or a plain answer once none is left.
struct ScriptedModel {
    replies: Mutex<VecDeque<Reply>>,
    reached: Arc<Notify>,
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the deterministic profile shared by every request.
    fn profile(&self) -> &ModelProfile {
        &PROFILE
    }

    /// The scripted model needs no readiness work.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Signals that a request arrived, waits if its reply is held, then streams it.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        let reply = self.replies.lock().expect("reply lock").pop_front();
        self.reached.notify_one();
        let events = match reply {
            Some(Reply { hold, events }) => {
                if let Some(hold) = hold {
                    hold.notified().await;
                }
                events
            }
            None => answer("done"),
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Hands every Session the same scripted model.
struct SharedModel(Arc<ScriptedModel>);

impl ModelFactory for SharedModel {
    /// Returns the shared scripted model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }
}

/// Sends one user message from a direct slash command, outside any agent loop.
#[derive(Clone)]
struct ForwardCommand;

#[async_trait]
impl ExtensionCommandHandler for ForwardCommand {
    /// Queues a follow-up so the extension host starts a Run of its own.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        context
            .event
            .send_user_message(ExtensionUserMessage {
                text: "hello from an extension".to_string(),
                delivery: QueueKind::FollowUp,
                parent_entry_id: None,
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))
    }
}

impl ExtensionModule for ForwardCommand {
    /// Declares the extension whose command forwards one user message.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("forwarder").expect("extension id"),
            name: "Forwarder".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the `forward` command.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            ExtensionCommandDefinition {
                name: "forward".to_string(),
                description: Some("Forward one user message".to_string()),
                argument_hint: None,
            },
            self.clone(),
        )
    }
}

/// Accepts every event; these tests read logs, not events.
struct DiscardSink;

#[async_trait]
impl EventSink for DiscardSink {
    /// Drops the event.
    async fn emit(&self, _event: AgentEvent) -> Result<(), SinkError> {
        Ok(())
    }
}

/// A production Kernel whose only scripted boundary is the model.
struct Fixture {
    kernel: Kernel,
    model: Arc<ScriptedModel>,
    root: tempfile::TempDir,
}

/// Production ids, except that every trace id is empty and therefore invalid.
struct NoTraceIds;

impl IdGenerator for NoTraceIds {
    /// Returns an empty trace id and delegates every other kind.
    fn next(&self, kind: IdKind) -> String {
        match kind {
            IdKind::Trace => String::new(),
            kind => NanoidIdGenerator.next(kind),
        }
    }
}

impl Fixture {
    /// Builds the Kernel with the given scripted replies and extension modules.
    fn new(
        replies: Vec<Reply>,
        modules: Vec<Arc<dyn ExtensionModule>>,
    ) -> Self {
        Self::with_ids(replies, modules, Arc::new(NanoidIdGenerator))
    }

    /// Builds the Kernel like `new`, minting ids with the given generator.
    fn with_ids(
        replies: Vec<Reply>,
        modules: Vec<Arc<dyn ExtensionModule>>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let model = Arc::new(ScriptedModel {
            replies: Mutex::new(replies.into()),
            reached: Arc::new(Notify::new()),
        });
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let kernel = KernelFactory::builder()
            .model_factory(Arc::new(SharedModel(Arc::clone(&model))))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                StaticExtensionRegistration::default(),
                modules
                    .into_iter()
                    .map(|module| {
                        Arc::new(move || Ok(Arc::clone(&module)))
                            as ExtensionModuleFactory
                    })
                    .collect(),
            )))
            .clock(clock)
            .id_generator(ids)
            .build()
            .build()
            .expect("build kernel");
        Self {
            kernel,
            model,
            root,
        }
    }

    /// Creates one Session with its own workspace directory.
    async fn session(&self, name: &str) -> SessionId {
        let cwd = self.root.path().join(name);
        std::fs::create_dir_all(&cwd).expect("workspace");
        let session_id = SessionId::try_from(name).expect("session id");
        self.kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd,
                parent_session_id: None,
            })
            .await
            .expect("create session");
        session_id
    }

    /// Runs one input to settlement through `Kernel::run`.
    async fn run(&self, session_id: &SessionId, input: &str) {
        let _result = self
            .kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: input.into(),
                },
                Arc::new(DiscardSink),
            )
            .await;
    }
}

/// Polls the shared capture until a line containing `needle` appears.
async fn wait_for_line(
    logs: &CapturedLogs,
    needle: &str,
) -> Result<String, String> {
    for _ in 0..200 {
        if let Ok(line) = line_with(&logs.content(), needle) {
            return Ok(line);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    line_with(&logs.content(), needle)
}

/// A Run whose identity cannot be minted still rejects its prompt with the real error.
#[tokio::test]
async fn failed_run_identity_rejects_the_prompt() {
    let fixture =
        Fixture::with_ids(Vec::new(), Vec::new(), Arc::new(NoTraceIds));
    let session = fixture.session("tracing-no-trace-ids").await;
    let (notice, insertion) = InsertionNotice::new();
    let result = fixture
        .kernel
        .run_with_insertion(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            Arc::new(DiscardSink),
            Some(notice),
        )
        .await;

    assert!(
        matches!(result, Err(KernelError::Protocol(_))),
        "{result:?}"
    );
    let reported = insertion.await.expect("the notice reports the rejection");
    assert!(
        matches!(reported, Err(KernelError::Protocol(_))),
        "{reported:?}"
    );
}

/// `Kernel::run` opens one top-level Run span that its Turns share.
#[tokio::test]
async fn kernel_run_lines_share_one_top_level_run_span() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(Vec::new(), Vec::new());
    let session = fixture.session("tracing-kernel-run").await;
    fixture.run(&session, "hello").await;

    let output = logs.content();
    let start = line_with(
        &output,
        &format!("started Kernel Run for session {session}"),
    )
    .expect("log line");
    assert!(start.contains("from Rpc"), "{start}");
    let (trace, run) = run_fields(&start);
    let trace = trace.expect("trace on the start line");
    let run = run.expect("run on the start line");
    assert!(trace.starts_with("trace-") && run.starts_with("run-"));
    let turn = output
        .lines()
        .find(|line| {
            line.contains("started Turn")
                && run_fields(line).1.as_deref() == Some(run.as_str())
        })
        .expect("a Turn line inside the Run");
    assert_eq!(run_fields(turn).0.as_deref(), Some(trace.as_str()));
}

/// A user `!` command is a Run of its own with its own trace.
#[tokio::test]
async fn user_bash_runs_in_a_run_span() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(Vec::new(), Vec::new());
    let session = fixture.session("tracing-user-bash").await;
    fixture.run(&session, "!true").await;

    let line = line_with(
        &logs.content(),
        &format!("started user bash for session {session}"),
    )
    .expect("log line");
    let trace = run_fields(&line).0.expect("trace on the user bash line");
    assert!(trace.starts_with("trace-"), "{line}");
}

/// A direct slash command is a Run of its own with its own trace.
#[tokio::test]
async fn direct_slash_command_runs_in_a_run_span() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(Vec::new(), Vec::new());
    let session = fixture.session("tracing-slash-command").await;
    fixture.run(&session, "/name traced").await;

    let line = line_with(
        &logs.content(),
        &format!("started Slash Command /name for session {session}"),
    )
    .expect("log line");
    let trace = run_fields(&line)
        .0
        .expect("trace on the slash command line");
    assert!(trace.starts_with("trace-"), "{line}");
}

/// A message an extension command sends starts a Run with a trace of its own.
#[tokio::test]
async fn extension_message_starts_its_own_trace() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(
        Vec::new(),
        vec![Arc::new(ForwardCommand) as Arc<dyn ExtensionModule>],
    );
    let session = fixture.session("tracing-extension-message").await;
    fixture.run(&session, "/forward").await;

    let command = wait_for_line(
        &logs,
        &format!("started Slash Command /forward for session {session}"),
    )
    .await
    .expect("log line");
    let start = wait_for_line(
        &logs,
        &format!("started Kernel Run for session {session}"),
    )
    .await
    .expect("log line");
    assert!(start.contains("from Extension"), "{start}");
    let trace = run_fields(&start).0.expect("trace on the start line");
    assert!(trace.starts_with("trace-"), "{start}");
    assert_ne!(Some(trace), run_fields(&command).0, "{command}");
}

/// Input submitted while a Run is busy is processed by that Run, under its trace.
#[tokio::test]
async fn queued_follow_up_runs_under_the_running_trace() {
    let logs = CapturedLogs::default();
    let gate = Arc::new(Notify::new());
    let fixture = Fixture::new(
        vec![Reply {
            hold: Some(Arc::clone(&gate)),
            events: answer("first"),
        }],
        Vec::new(),
    );
    let session = fixture.session("tracing-queued-follow-up").await;
    let enqueued = format!("for session {session} and Kernel Run");

    let run = fixture.run(&session, "first input");
    let follow_up = async {
        fixture.model.reached.notified().await;
        fixture
            .kernel
            .queue_message(
                &session,
                protocol::QueueKind::FollowUp,
                "queued input",
            )
            .await
            .expect("queue follow-up");
    };
    let release = async {
        wait_for_line(&logs, &enqueued).await.expect("log line");
        gate.notify_one();
    };
    tokio::join!(run, follow_up, release);

    let output = logs.content();
    let starts = output
        .lines()
        .filter(|line| {
            line.contains(&format!("started Kernel Run for session {session}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{starts:?}");
    let fields = run_fields(starts[0]);
    let turns = output
        .lines()
        .filter(|line| {
            line.contains("started Turn") && run_fields(line) == fields
        })
        .count();
    assert!(turns >= 2, "the follow-up Turn runs inside the first Run");
}

/// A busy Slash Command logs no start line, so its rejection names the Session.
#[tokio::test]
async fn busy_slash_command_rejection_names_the_session() {
    let logs = CapturedLogs::default();
    let gate = Arc::new(Notify::new());
    let fixture = Fixture::new(
        vec![Reply {
            hold: Some(Arc::clone(&gate)),
            events: answer("first"),
        }],
        Vec::new(),
    );
    let session = fixture.session("tracing-busy-slash-command").await;

    let run = fixture.run(&session, "keep running");
    let busy = async {
        fixture.model.reached.notified().await;
        fixture.run(&session, "/session").await;
        gate.notify_one();
    };
    tokio::join!(run, busy);

    line_with(
        &logs.content(),
        &format!("rejected busy Slash Command /session for session {session}"),
    )
    .expect("log line");
}

/// Lines inside a Run carry its ids only on the span, and name the Session once.
#[tokio::test]
async fn run_messages_do_not_repeat_correlation_ids() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(
        vec![
            Reply {
                hold: None,
                events: bash_call("call-dedupe"),
            },
            Reply {
                hold: None,
                events: answer("done"),
            },
        ],
        Vec::new(),
    );
    let session = fixture.session("tracing-dedupe").await;
    fixture.run(&session, "use one tool").await;

    let output = logs.content();
    let start = line_with(
        &output,
        &format!("started Kernel Run for session {session}"),
    )
    .expect("log line");
    let (trace, run) = run_fields(&start);
    let (trace, run) = (trace.expect("trace"), run.expect("run"));
    let lines = output
        .lines()
        .filter(|line| run_fields(line).1.as_deref() == Some(run.as_str()))
        .collect::<Vec<_>>();
    assert!(lines.iter().any(|line| line.contains("started Tool call")));
    for line in &lines {
        assert_eq!(line.matches(trace.as_str()).count(), 1, "{line}");
        assert_eq!(line.matches(run.as_str()).count(), 1, "{line}");
    }
    let naming_session = lines
        .iter()
        .filter(|line| line.contains(session.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(naming_session.len(), 1, "{naming_session:#?}");
}

/// A server-side shell request (the WebUI `!` path) is a top-level Run of its own.
#[tokio::test]
async fn server_side_user_bash_runs_in_a_run_span() {
    let logs = CapturedLogs::default();
    let fixture = Fixture::new(Vec::new(), Vec::new());
    let session = fixture.session("tracing-server-bash").await;
    fixture
        .kernel
        .execute_user_bash(
            UserBashRequest {
                session_id: session.clone(),
                command: "true".to_string(),
                exclude_from_context: false,
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("user bash");

    let line = line_with(
        &logs.content(),
        &format!("started user bash for session {session}"),
    )
    .expect("log line");
    let trace = run_fields(&line).0.expect("trace on the user bash line");
    assert!(trace.starts_with("trace-"), "{line}");
}

/// A spawned child's Run starts a new trace whose start line names the owner's.
#[tokio::test]
async fn spawned_child_links_the_owner_trace() {
    let logs = CapturedLogs::default();
    let spawn = vec![
        ModelStreamEvent::ToolCall(
            ToolCall::builder()
                .tool_call_id(
                    ToolCallId::try_from("call-spawn").expect("call id"),
                )
                .name(protocol::SPAWN_AGENT_TOOL.to_string())
                .arguments(serde_json::json!({
                    "task_name": "tracer",
                    "message": "Trace me."
                }))
                .build(),
        ),
        finished(StopReason::ToolUse),
    ];
    let fixture = Fixture::new(
        vec![Reply {
            hold: None,
            events: spawn,
        }],
        Vec::new(),
    );
    let owner = fixture.session("tracing-spawn-owner").await;
    fixture.run(&owner, "spawn").await;
    let child = fixture
        .kernel
        .list_sessions(Some(&fixture.root.path().join("tracing-spawn-owner")))
        .expect("list")
        .into_iter()
        .find(|summary| summary.ownership.is_some())
        .expect("child")
        .session_id;

    let owner_start = wait_for_line(
        &logs,
        &format!("started Kernel Run for session {owner} "),
    )
    .await
    .expect("owner start");
    let parent_trace = run_fields(&owner_start).0.expect("owner trace");
    let child_start = wait_for_line(
        &logs,
        &format!("started Kernel Run for session {child} "),
    )
    .await
    .expect("child start");
    assert!(
        child_start
            .contains(&format!("as subagent linked to trace {parent_trace}")),
        "{child_start}"
    );
    assert_ne!(run_fields(&child_start).0, Some(parent_trace));
    fixture.kernel.close_session(&owner).await.expect("close");
}

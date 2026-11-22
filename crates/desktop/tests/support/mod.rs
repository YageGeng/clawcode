//! Scripted Kernel behind a real in-process ACP connection, for integration tests.
#![allow(dead_code)] // Each test binary uses a different subset of the fixture.

pub mod shell;
pub mod trajectory;

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::v2::schema as wire;
use async_trait::async_trait;
use desktop::backend::{
    AcpClient, AcpConnection, AgentRequest, BackendEvent, RequestId, SessionId,
};
use extension::StaticExtensionFactory;
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    ContentBlock, MessageContent, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, StaticExtensionRegistration, StopReason, ToolCall,
    ToolCallId,
};
use store::{JsonlStoreFactory, SystemClock};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;
use typed_builder::TypedBuilder;

/// Bounds every wait so a regression fails instead of hanging.
pub const WAIT: Duration = Duration::from_secs(5);

/// Runs the app until `check` holds; fails after [`WAIT`].
pub fn eventually(
    cx: &mut gpui_kit::TestAppContext,
    what: &str,
    check: impl Fn(&mut gpui_kit::App) -> bool,
) {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if cx.update(|cx| check(cx)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Model whose reply is chosen by the newest user text.
struct ScriptedModel {
    profile: ModelProfile,
}

impl ScriptedModel {
    /// Builds the terminal event of one reply, with a fixed one-token usage.
    fn finished(stop: StopReason) -> ModelStreamEvent {
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(stop)
                .raw_stop_reason(None)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(1)
                        .output_tokens(1)
                        .cached_input_tokens(0)
                        .cache_creation_input_tokens(0)
                        .total_tokens(2),
                ))
                .build(),
        )
    }

    /// Builds a reply that requests one tool call and stops for tool use.
    fn tool_reply(
        id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> ModelStream {
        let call = ToolCall::builder()
            .tool_call_id(ToolCallId::try_from(id).expect("tool call id"))
            .name(name.to_string())
            .arguments(arguments)
            .build();
        let finished = Self::finished(StopReason::ToolUse);
        Box::pin(stream::iter(vec![
            Ok(ModelStreamEvent::ToolCall(call)),
            Ok(finished),
        ]))
    }

    /// Whether the newest message is the result of tool call `id`.
    fn answers_call(request: &ModelRequest, id: &str) -> bool {
        matches!(
            request.messages.last().map(|message| &message.content),
            Some(MessageContent::ToolResult { tool_call_id, .. })
                if tool_call_id.as_str() == id
        )
    }

    /// Wraps reply events and the terminal event into one stream.
    fn reply(events: Vec<ModelStreamEvent>) -> ModelStream {
        let mut events: Vec<_> = events.into_iter().map(Ok).collect();
        events.push(Ok(Self::finished(StopReason::EndTurn)));
        Box::pin(stream::iter(events))
    }
}

/// Reads the newest user text of a provider request.
fn last_user_text(request: &ModelRequest) -> String {
    match request.messages.last().map(|message| &message.content) {
        Some(MessageContent::User { blocks }) => blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Always ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Answers `hello`, `think`, `slow`, and the tool scripts; anything else gets `ok`.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        // Like a real provider, a slow reply gives up as soon as the run is cancelled.
        let wait = |millis: u64| {
            let cancellation = &cancellation;
            async move {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(millis)) => Ok(()),
                    () = cancellation.cancelled() => Err(ModelError::Cancelled),
                }
            }
        };
        let text = last_user_text(&request);
        let script = if Self::answers_call(&request, "askwork-1") {
            // The run that asked keeps working, so an answer waits in the queue.
            "working"
        } else {
            text.as_str()
        };
        Ok(match script {
            "hello" => Self::reply(vec![
                ModelStreamEvent::TextDelta("Hi ".to_string()),
                ModelStreamEvent::TextDelta("there".to_string()),
            ]),
            "think" => Self::reply(vec![
                ModelStreamEvent::ReasoningDelta("pondering".to_string()),
                ModelStreamEvent::TextDelta("done".to_string()),
            ]),
            "read" => Self::tool_reply(
                "read-1",
                "read",
                serde_json::json!({ "path": "notes.txt" }),
            ),
            "edit" => Self::tool_reply(
                "edit-1",
                "edit",
                serde_json::json!({
                    "path": "notes.txt",
                    "edits": [{ "oldText": "alpha", "newText": "beta" }]
                }),
            ),
            "exec" => Self::tool_reply(
                "exec-1",
                "exec_command",
                serde_json::json!({ "cmd": "printf 'one\\ntwo\\n'", "login": false }),
            ),
            "exec30" => Self::tool_reply(
                "exec-30",
                "exec_command",
                serde_json::json!({ "cmd": "seq 1 30", "login": false }),
            ),
            // A command spanning several lines, as models write shell scripts.
            "execml" => Self::tool_reply(
                "exec-ml",
                "exec_command",
                serde_json::json!({
                    "cmd": "echo first\necho second\necho third",
                    "login": false
                }),
            ),
            // Submits a plan; only accepted while the session is in plan mode.
            "plan" => Self::tool_reply(
                "plan-1",
                "exit_plan_mode",
                serde_json::json!({ "plan": "1. step one" }),
            ),
            // A plan far taller than the panel above the composer may grow.
            "longplan" => Self::tool_reply(
                "plan-long",
                "exit_plan_mode",
                serde_json::json!({ "plan": (1..=60)
                    .map(|step| format!("{step}. step {step}"))
                    .collect::<Vec<_>>()
                    .join("\n") }),
            ),
            // Asks two questions without waiting for the answers.
            "ask" => Self::tool_reply(
                "ask-1",
                "request_user_input_async",
                serde_json::json!({ "questions": [
                    { "title": "Pick one", "options": ["a", "b"] },
                    { "title": "Anything else?" }
                ] }),
            ),
            // Asks the same two questions, then keeps the run working.
            "askwork" => Self::tool_reply(
                "askwork-1",
                "request_user_input_async",
                serde_json::json!({ "questions": [
                    { "title": "Pick one", "options": ["a", "b"] },
                    { "title": "Anything else?" }
                ] }),
            ),
            "working" => {
                wait(5000).await?;
                Self::reply(vec![ModelStreamEvent::TextDelta(
                    "worked".to_string(),
                )])
            }
            // Publishes a two-item todo list.
            "todo" => Self::tool_reply(
                "todo-1",
                "todo_write",
                serde_json::json!({ "todos": [
                    { "content": "Write code", "status": "in_progress", "activeForm": "Writing code" },
                    { "content": "Test it", "status": "pending", "activeForm": "Testing it" }
                ] }),
            ),
            // Long enough that a test can read the queue before the run settles.
            "slower" => {
                wait(1500).await?;
                Self::reply(vec![ModelStreamEvent::TextDelta(
                    "late".to_string(),
                )])
            }
            "slow" => {
                wait(400).await?;
                Self::reply(vec![ModelStreamEvent::TextDelta(
                    "late".to_string(),
                )])
            }
            _ => {
                Self::reply(vec![ModelStreamEvent::TextDelta("ok".to_string())])
            }
        })
    }
}

/// Offers the one scripted model.
struct ScriptedModelFactory(Arc<ScriptedModel>);

impl ModelFactory for ScriptedModelFactory {
    /// Returns the shared scripted model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }

    /// Builds a one-model catalog.
    fn create_catalog(
        &self,
        _registration: &StaticExtensionRegistration,
    ) -> Result<kernel::ModelCatalog, ModelError> {
        kernel::ModelCatalog::new("fixture", "scripted", vec![self.create()?])
    }
}

/// Builds a production Kernel around the scripted model, persisting under
/// `root`, with the history database `state` when given.
fn scripted_kernel(
    root: &Path,
    state: Option<Arc<state::StateRuntime>>,
) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let model = Arc::new(ScriptedModel {
        profile: ModelProfile::builder()
            .provider_id("fixture".to_string())
            .model_id("scripted".to_string())
            .display_name("Scripted fixture".to_string())
            .context_tokens(1_000_000)
            .max_output_tokens(1_024)
            .build(),
    });
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(ScriptedModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .state(state)
            // Keeps nothing back, so even a one-turn session can be compacted.
            .compaction_policy(
                protocol::CompactionPolicy::builder()
                    .enabled(true)
                    .reserve_tokens(16_384)
                    .keep_recent_tokens(0)
                    .build(),
            )
            // Enables `exec_command`; shell snapshots would only slow the fixture.
            .exec(Some(Arc::new(tools::exec::ExecSettings {
                config: config::ExecConfig::builder()
                    .shell_snapshot(false)
                    .build(),
                snapshot_dir: root.join("shell_snapshots"),
            })))
            .build()
            .build()
            .expect("scripted kernel"),
    )
}

/// A connected desktop client in front of a scripted Kernel.
#[derive(TypedBuilder)]
pub struct ScriptedBackend {
    pub client: AcpClient,
    pub connection: AcpConnection,
    /// Owns the projection registry the Kernel's sinks publish into.
    _server: Arc<AcpServerFactory>,
    pub events: UnboundedReceiver<BackendEvent>,
    pub workspace: TempDir,
    pub root: TempDir,
    pub runtime: tokio::runtime::Runtime,
}

impl ScriptedBackend {
    /// Builds the Kernel and connects a desktop `AcpClient` to it in-process.
    pub fn start() -> ScriptedBackend {
        Self::start_with(false)
    }

    /// Like [`Self::start`], with a ready history database behind the
    /// Kernel, as the trajectory view needs.
    pub fn start_with_history() -> ScriptedBackend {
        Self::start_with(true)
    }

    /// Builds the Kernel, with a history database when `history`, and
    /// connects a desktop `AcpClient` to it in-process.
    fn start_with(history: bool) -> ScriptedBackend {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let root = tempfile::tempdir().expect("store root");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("notes.txt"), "alpha\n")
            .expect("fixture notes");
        let state = history.then(|| {
            runtime.block_on(async {
                let state = state::StateRuntime::init(
                    &state::SqliteConfig::new(root.path().join("state")),
                )
                .await
                .expect("state runtime");
                state
                    .backfill(root.path(), &[])
                    .await
                    .expect("empty backfill");
                state
            })
        });
        let kernel = scripted_kernel(root.path(), state);
        let server = Arc::new(AcpServerFactory::new(
            Arc::clone(&kernel),
            NonZeroUsize::new(128).expect("positive batch size"),
        ));
        let (events_tx, events) = unbounded();
        let (client, connection) = runtime
            .block_on(AcpClient::connect(
                server.component(AcpTransportKind::InProcess),
                events_tx,
            ))
            .expect("in-process ACP connection");
        ScriptedBackend::builder()
            .client(client)
            .connection(connection)
            ._server(server)
            .events(events)
            .workspace(workspace)
            .root(root)
            .runtime(runtime)
            .build()
    }

    /// Runs one future on the fixture runtime.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Polls the pending queue every 20 ms until a follow-up is queued; fails after `WAIT`.
    pub fn wait_follow_up(&self, id: &SessionId) -> protocol::PendingMessages {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let pending = self
                .block_on(self.client.pending_messages(id))
                .expect("pending");
            if pending.follow_up.len() == 1 {
                return pending;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for a queued follow-up"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Creates a session in the fixture workspace.
    pub fn new_session(&self) -> SessionId {
        self.block_on(self.client.new_session(self.workspace.path()))
            .expect("new session")
            .id
    }

    /// Collects this session's updates until `done` matches one; fails after `WAIT`.
    pub fn wait_for(
        &mut self,
        id: &SessionId,
        done: impl Fn(&wire::SessionUpdate) -> bool,
    ) -> Vec<wire::SessionUpdate> {
        let events = &mut self.events;
        self.runtime
            .block_on(async {
                tokio::time::timeout(WAIT, async {
                    let mut seen = Vec::new();
                    while let Some(event) = events.next().await {
                        if let BackendEvent::Update(notification) = event
                            && &notification.session_id == id
                        {
                            let finished = done(&notification.update);
                            seen.push(notification.update);
                            if finished {
                                return Some(seen);
                            }
                        }
                    }
                    None
                })
                .await
                .expect("expected update in time")
            })
            .expect("event stream ended before the expected update")
    }

    /// Collects updates until the session goes from running back to idle.
    pub fn wait_idle(&mut self, id: &SessionId) -> Vec<wire::SessionUpdate> {
        let mut seen = self.wait_for(id, |update| {
            matches!(
                update,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_))
            )
        });
        seen.extend(self.wait_for(id, |update| {
            matches!(
                update,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
            )
        }));
        seen
    }

    /// Waits for the next agent→client request; fails after `WAIT`.
    pub fn wait_request(&mut self) -> AgentRequest {
        self.wait_event(|event| match event {
            BackendEvent::Request(request) => Some(*request),
            _ => None,
        })
    }

    /// Waits until the backend reports that this request was withdrawn; fails after `WAIT`.
    pub fn wait_request_cancelled(&mut self, id: RequestId) {
        self.wait_event(|event| match event {
            BackendEvent::RequestCancelled(cancelled) if cancelled == id => {
                Some(())
            }
            _ => None,
        })
    }

    /// Consumes backend events until `pick` accepts one; fails after `WAIT`.
    fn wait_event<T>(&mut self, pick: impl Fn(BackendEvent) -> Option<T>) -> T {
        let events = &mut self.events;
        self.runtime
            .block_on(async {
                tokio::time::timeout(WAIT, async {
                    while let Some(event) = events.next().await {
                        if let Some(found) = pick(event) {
                            return Some(found);
                        }
                    }
                    None
                })
                .await
                .expect("expected backend event in time")
            })
            .expect("event stream ended before the expected event")
    }

    /// Collects the replayed history up to the server's `replay_complete` marker.
    pub fn wait_replay_complete(
        &mut self,
        id: &SessionId,
    ) -> Vec<wire::SessionUpdate> {
        self.wait_for(id, |update| {
            matches!(update, wire::SessionUpdate::ReplayComplete(_))
        })
    }

    /// Discards updates already delivered for this session, waiting briefly for stragglers.
    pub fn drain(&mut self, id: &SessionId) -> Vec<wire::SessionUpdate> {
        let events = &mut self.events;
        self.runtime.block_on(async {
            let mut seen = Vec::new();
            while let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(200), events.next())
                    .await
            {
                if let BackendEvent::Update(notification) = event
                    && &notification.session_id == id
                {
                    seen.push(notification.update);
                }
            }
            seen
        })
    }
}

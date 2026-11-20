//! Scripted Kernel behind a real in-process ACP connection, for integration tests.
#![allow(dead_code)] // Each test binary uses a different subset of the fixture.

pub mod shell;

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::v2::schema as wire;
use async_trait::async_trait;
use desktop::backend::{AcpClient, AcpConnection, BackendEvent, SessionId};
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

/// Model whose reply is chosen by the newest user text.
struct ScriptedModel {
    profile: ModelProfile,
}

impl ScriptedModel {
    /// Builds the terminal event of one reply.
    fn finished() -> ModelStreamEvent {
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(StopReason::EndTurn)
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
        let finished = ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(StopReason::ToolUse)
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
        );
        Box::pin(stream::iter(vec![
            Ok(ModelStreamEvent::ToolCall(call)),
            Ok(finished),
        ]))
    }

    /// Wraps reply events and the terminal event into one stream.
    fn reply(events: Vec<ModelStreamEvent>) -> ModelStream {
        let mut events: Vec<_> = events.into_iter().map(Ok).collect();
        events.push(Ok(Self::finished()));
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
        Ok(match last_user_text(&request).as_str() {
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

/// Builds a production Kernel around the scripted model, persisting under `root`.
fn scripted_kernel(root: &Path) -> Arc<Kernel> {
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
    pub kernel: Arc<Kernel>,
    pub client: AcpClient,
    pub connection: AcpConnection,
    pub server: Arc<AcpServerFactory>,
    pub events: UnboundedReceiver<BackendEvent>,
    pub workspace: TempDir,
    pub root: TempDir,
    pub runtime: tokio::runtime::Runtime,
}

impl ScriptedBackend {
    /// Builds the Kernel and connects a desktop `AcpClient` to it in-process.
    pub fn start() -> ScriptedBackend {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let root = tempfile::tempdir().expect("store root");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("notes.txt"), "alpha\n")
            .expect("fixture notes");
        let kernel = scripted_kernel(root.path());
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
            .kernel(kernel)
            .client(client)
            .connection(connection)
            .server(server)
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

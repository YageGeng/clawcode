use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{Agent, Client, V2ConnectionTo};
use async_trait::async_trait;
use extension::{
    BeforeAgentStartPoint, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, InputPoint, StaticExtensionFactory,
};
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    BeforeAgentStartEvent, BeforeAgentStartResult, ContentBlock,
    ExtensionDescriptor, ExtensionId, InputEvent, InputResult, MessageContent,
    ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent,
    StaticExtensionRegistration, StopReason, ThinkingLevel,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Gives a request time to reach the Kernel gate before the test acts.
const SETTLE: Duration = Duration::from_millis(100);

/// Bounds every wait for a notification so a regression fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);

type Updates = mpsc::UnboundedReceiver<wire::UpdateSessionNotification>;

/// Model routed by the last user text: "hang" waits for cancellation, "hold" for a release.
struct ScriptedModel {
    profile: ModelProfile,
    release: Arc<Notify>,
    preflight_failure: Mutex<Option<String>>,
}

impl ScriptedModel {
    /// Creates a deterministic model with no scripted failures.
    fn new() -> Arc<Self> {
        Arc::new(Self {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("scripted".to_string())
                .display_name("Scripted fixture".to_string())
                .context_tokens(1_000_000)
                .max_output_tokens(1_024)
                .build(),
            release: Arc::new(Notify::new()),
            preflight_failure: Mutex::new(None),
        })
    }

    /// Builds one complete answer stream.
    fn answer() -> ModelStream {
        Box::pin(stream::iter([
            Ok(ModelStreamEvent::TextDelta("answer".to_string())),
            Ok(ModelStreamEvent::Finished(
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
            )),
        ]))
    }
}

/// Reports whether a provider request is the dedicated compaction summary protocol.
fn is_summary_request(request: &ModelRequest) -> bool {
    request.messages.first().is_some_and(|message| {
        matches!(
            &message.content,
            MessageContent::System { blocks } if blocks.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text } if text.contains("context summarization assistant")
            ))
        )
    })
}

/// Reads the visible text of the newest message in a provider request.
fn last_text(request: &ModelRequest) -> String {
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
    /// Returns the deterministic fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Fails readiness only when a test scripts a provider outage.
    async fn preflight(&self) -> Result<(), ModelError> {
        match self.preflight_failure.lock().expect("preflight").clone() {
            Some(message) => Err(ModelError::Unavailable(message)),
            None => Ok(()),
        }
    }

    /// Streams an answer after the routed wait, honoring cancellation.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        // Compaction summaries hang so a test can cancel a running /compact.
        if is_summary_request(&request) {
            cancellation.cancelled().await;
            return Err(ModelError::Cancelled);
        }
        match last_text(&request).as_str() {
            "hang" => {
                cancellation.cancelled().await;
                Err(ModelError::Cancelled)
            }
            "tool" => Ok(Box::pin(stream::iter([
                Ok(ModelStreamEvent::ToolCall(
                    protocol::ToolCall::builder()
                        .tool_call_id(
                            protocol::ToolCallId::try_from("bash-call")
                                .expect("tool call id"),
                        )
                        .name("bash".to_string())
                        .arguments(serde_json::json!({ "command": "sleep 5" }))
                        .build(),
                )),
                Ok(ModelStreamEvent::Finished(
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
                )),
            ]))),
            "hold" => {
                tokio::select! {
                    () = self.release.notified() => Ok(Self::answer()),
                    () = cancellation.cancelled() => Err(ModelError::Cancelled),
                }
            }
            _ => Ok(Self::answer()),
        }
    }
}

/// Second catalog model; its id contains `/` and it always answers "second".
struct SecondModel(ModelProfile);

#[async_trait]
impl Model for SecondModel {
    /// Returns the second fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.0
    }

    /// The fixture is always ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Answers with a text that identifies this model.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Ok(Box::pin(stream::iter([
            Ok(ModelStreamEvent::TextDelta("second".to_string())),
            Ok(ModelStreamEvent::Finished(
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
            )),
        ])))
    }
}

/// Shares one scripted model across every Session.
struct ScriptedModelFactory(Arc<ScriptedModel>);

impl ModelFactory for ScriptedModelFactory {
    /// Returns the shared scripted model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }

    /// Adds a second model so Session config options have a real choice.
    fn create_catalog(
        &self,
        _registration: &StaticExtensionRegistration,
    ) -> Result<kernel::ModelCatalog, ModelError> {
        kernel::ModelCatalog::new(
            "fixture",
            "scripted",
            vec![
                self.create()?,
                Arc::new(SecondModel(
                    ModelProfile::builder()
                        .provider_id("openrouter".to_string())
                        .model_id("deepseek/deepseek-chat".to_string())
                        .display_name("DeepSeek Chat".to_string())
                        .context_tokens(64_000)
                        .max_output_tokens(1_024)
                        .build(),
                )),
            ],
        )
    }
}

/// Builds a production Kernel around the scripted model and optional extensions.
fn alignment_kernel(
    root: &Path,
    model: &Arc<ScriptedModel>,
    extensions: Arc<dyn extension::ExtensionFactory>,
) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(ScriptedModelFactory(Arc::clone(model))))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(extensions)
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("alignment kernel"),
    )
}

/// Builds one extension module from a single handler registration.
struct HookModule<F>(F);

impl<F> ExtensionModule for HookModule<F>
where
    F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionError> + Send + Sync,
{
    /// Identifies the alignment test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("acp-alignment-hooks").expect("id"),
            name: "ACP alignment hooks".into(),
            version: "1".into(),
        }
    }

    /// Registers the test's handler under the module identity.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        (self.0)(registrar)
    }
}

/// Wraps a registration closure into a static extension factory.
fn hooks<F>(register: F) -> Arc<dyn extension::ExtensionFactory>
where
    F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionError>
        + Clone
        + Send
        + Sync
        + 'static,
{
    Arc::new(StaticExtensionFactory::new(
        StaticExtensionRegistration::default(),
        vec![Arc::new(move || {
            Ok(Arc::new(HookModule(register.clone()))
                as Arc<dyn ExtensionModule>)
        })],
    ))
}

/// Input handler that always fails.
#[derive(Clone)]
struct FailingInput;

#[async_trait]
impl ExtensionHandler<InputPoint> for FailingInput {
    /// Fails so the Kernel reports a diagnostic and keeps the input.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        Err(ExtensionError::Handler("input fixture failure".into()))
    }
}

/// Agent-start hook that ignores cancellation until the test releases it.
#[derive(Clone)]
struct BlockingAgentStart {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for BlockingAgentStart {
    /// Blocks a "block" prompt after admission but before insertion; other prompts pass.
    async fn handle(
        &self,
        event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        if event.prompt == "block" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(BeforeAgentStartResult::default())
    }
}

/// Builds the one ACP server factory a test shares across its connections.
///
/// A Kernel accepts one event-sink installation, so every connection of a test
/// must come from the same factory to observe the same projections.
fn acp(kernel: &Arc<Kernel>) -> Arc<AcpServerFactory> {
    Arc::new(AcpServerFactory::new(
        Arc::clone(kernel),
        Arc::new(NanoidIdGenerator),
        NonZeroUsize::new(128).expect("positive batch size"),
    ))
}

/// Runs `body` on an initialized client connection.
async fn with_connection<B>(factory: &Arc<AcpServerFactory>, body: B)
where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        &mut Updates,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let server = factory.component(AcpTransportKind::Stdio);
    let (updates_tx, mut updates) = mpsc::unbounded_channel();
    Client
        .v2()
        .on_receive_notification(
            async move |notification: wire::UpdateSessionNotification,
                        _connection| {
                // A closed receiver only means the test body already finished.
                let _ignored = updates_tx.send(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(server, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new("v2-alignment-test", "0.1.0"),
                ))
                .block_task()
                .await?;
            body(&connection, &mut updates).await
        })
        .await
        .expect("ACP connection");
}

/// Runs `body` on an initialized client connection that already created one Session.
async fn with_session<B>(
    factory: &Arc<AcpServerFactory>,
    workspace: &Path,
    body: B,
) where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        wire::SessionId,
        &mut Updates,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let workspace = workspace.to_path_buf();
    with_connection(factory, async move |connection, updates| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        body(connection, created.session_id, updates).await
    })
    .await;
}

/// Builds a one-block text prompt request.
fn prompt(session_id: &wire::SessionId, text: &str) -> wire::PromptRequest {
    wire::PromptRequest::new(
        session_id.clone(),
        vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
    )
}

/// Serializes one update for structural assertions.
fn json(update: &wire::UpdateSessionNotification) -> serde_json::Value {
    serde_json::to_value(&update.update).expect("serialize update")
}

/// Waits for the first update that satisfies `check`, failing after `WAIT`.
async fn next_matching(
    updates: &mut Updates,
    check: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    tokio::time::timeout(WAIT, async {
        loop {
            let update = updates.recv().await.expect("update stream");
            let value = json(&update);
            if check(&value) {
                return value;
            }
        }
    })
    .await
    .expect("matching update")
}

/// Collects updates that arrive within `SETTLE`.
async fn drain(updates: &mut Updates) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    while let Ok(Some(update)) =
        tokio::time::timeout(SETTLE, updates.recv()).await
    {
        values.push(json(&update));
    }
    values
}

/// Reports whether an update is the user message with this id.
fn is_user_message(value: &serde_json::Value, id: &str) -> bool {
    value["sessionUpdate"] == "user_message" && value["messageId"] == id
}

/// The prompt response and its user message update carry the same inserted id.
#[tokio::test]
async fn prompt_response_carries_inserted_message_id() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let kernel = alignment_kernel(
        state.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            let response = connection
                .send_request(prompt(&session_id, "hello"))
                .block_task()
                .await?;
            let id = response.message_id.to_string();
            next_matching(updates, |value| is_user_message(value, &id)).await;
            Ok(())
        },
    )
    .await;
}

/// A provider failure before insertion returns an error and never shows the user message.
#[tokio::test]
async fn prompt_rejected_before_insertion_emits_no_user_message() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    *model.preflight_failure.lock().expect("preflight") =
        Some("provider offline".into());
    let kernel = alignment_kernel(
        state.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            let error = connection
                .send_request(prompt(&session_id, "hello"))
                .block_task()
                .await
                .expect_err("preflight failure rejects the prompt");
            assert_eq!(error.code, (-32603).into());
            assert!(
                drain(updates)
                    .await
                    .iter()
                    .all(|value| value["sessionUpdate"] != "user_message")
            );
            Ok(())
        },
    )
    .await;
}

/// A failing input handler reports a diagnostic while preserving successful prompt acceptance.
#[tokio::test]
async fn input_hook_error_does_not_reject_prompt() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let kernel = alignment_kernel(
        state.path(),
        &model,
        hooks(|registrar| registrar.on::<InputPoint, _>(FailingInput)),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            let response = connection
                .send_request(prompt(&session_id, "hello"))
                .block_task()
                .await?;
            let id = response.message_id.to_string();
            next_matching(updates, |value| is_user_message(value, &id)).await;
            Ok(())
        },
    )
    .await;
}

/// The prompt handler must not hold the dispatch loop: cancel arrives on the same connection.
#[tokio::test]
async fn prompt_waiting_for_gate_is_cancelled_with_32800() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let kernel = alignment_kernel(
        state.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(&session_id, "hang"))
                .block_task()
                .await?;
            let waiting = connection
                .send_request(prompt(&session_id, "second"))
                .block_task();
            let cancel = async {
                tokio::time::sleep(SETTLE).await;
                connection.send_notification(
                    wire::CancelSessionNotification::new(session_id.clone()),
                )
            };
            let (waiting, cancelled) = tokio::join!(waiting, cancel);
            cancelled?;
            let error = waiting.expect_err("waiting prompt is cancelled");
            assert_eq!(error.code, (-32800).into());
            Ok(())
        },
    )
    .await;
}

/// A cancel during a blocked post-admission hook is still a pre-insertion cancel.
#[tokio::test]
async fn prompt_cancelled_during_hook_returns_32800() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = BlockingAgentStart {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let kernel = alignment_kernel(
        state.path(),
        &model,
        hooks(move |registrar| {
            registrar.on::<BeforeAgentStartPoint, _>(hook.clone())
        }),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            let waiting = connection
                .send_request(prompt(&session_id, "block"))
                .block_task();
            let cancel = async {
                entered.notified().await;
                connection.send_notification(
                    wire::CancelSessionNotification::new(session_id.clone()),
                )?;
                tokio::time::sleep(SETTLE).await;
                release.notify_one();
                Ok::<_, agent_client_protocol::Error>(())
            };
            let (waiting, cancelled) = tokio::join!(waiting, cancel);
            cancelled?;
            let error =
                waiting.expect_err("hook-time cancel rejects the prompt");
            assert_eq!(error.code, (-32800).into());
            let idle = next_matching(updates, |value| {
                value["sessionUpdate"] == "state_update"
                    && value["state"] == "idle"
            })
            .await;
            assert_eq!(idle["stopReason"], "cancelled");
            assert!(
                drain(updates)
                    .await
                    .iter()
                    .all(|value| value["sessionUpdate"] != "user_message")
            );
            Ok(())
        },
    )
    .await;
}

/// Concurrent waiting prompts are each accepted; insertion order is not asserted in this phase.
#[tokio::test]
async fn two_waiting_prompts_each_receive_their_own_message_id() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let release = Arc::clone(&model.release);
    let kernel = alignment_kernel(
        state.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(&session_id, "hold"))
                .block_task()
                .await?;
            let second = connection
                .send_request(prompt(&session_id, "second"))
                .block_task();
            let third = connection
                .send_request(prompt(&session_id, "third"))
                .block_task();
            let unblock = async {
                tokio::time::sleep(SETTLE).await;
                release.notify_one();
            };
            let (second, third, ()) = tokio::join!(second, third, unblock);
            let mut pending = vec![
                second?.message_id.to_string(),
                third?.message_id.to_string(),
            ];
            assert_ne!(pending[0], pending[1]);
            while !pending.is_empty() {
                let seen = next_matching(updates, |value| {
                    pending.iter().any(|id| is_user_message(value, id))
                })
                .await;
                pending.retain(|id| seen["messageId"] != id.as_str());
            }
            Ok(())
        },
    )
    .await;
}

/// Builds a Kernel whose "block" prompt ignores cancellation until released.
fn blocking_kernel(
    root: &Path,
    entered: &Arc<Notify>,
    release: &Arc<Notify>,
) -> Arc<Kernel> {
    let hook = BlockingAgentStart {
        entered: Arc::clone(entered),
        release: Arc::clone(release),
    };
    alignment_kernel(
        root,
        &ScriptedModel::new(),
        hooks(move |registrar| {
            registrar.on::<BeforeAgentStartPoint, _>(hook.clone())
        }),
    )
}

/// Reports whether an update is an idle state with this stop reason.
fn is_idle(value: &serde_json::Value, stop_reason: &str) -> bool {
    value["sessionUpdate"] == "state_update"
        && value["state"] == "idle"
        && value["stopReason"] == stop_reason
}

/// Drains notifications already dispatched, without waiting for new ones.
fn dispatched(updates: &mut Updates) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    while let Ok(update) = updates.try_recv() {
        values.push(json(&update));
    }
    values
}

/// A connection that never created or resumed a Session cannot prompt it.
#[tokio::test]
async fn prompt_on_inactive_session_returns_32002() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let session_id = kernel
        .create_generated_session(workspace.path().into())
        .await
        .expect("session");
    with_connection(&acp(&kernel), async |connection, _updates| {
        let error = connection
            .send_request(prompt(
                &wire::SessionId::new(session_id.to_string()),
                "hello",
            ))
            .block_task()
            .await
            .expect_err("inactive Session is rejected");
        assert_eq!(error.code, (-32002).into());
        assert_eq!(error.data.expect("data")["sessionId"], session_id.as_str());
        Ok(())
    })
    .await;
}

/// Cancel is a notification, so it still stops a Session this connection never attached.
#[tokio::test]
async fn cancel_for_inactive_session_still_cancels() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let session_id = kernel
        .create_generated_session(workspace.path().into())
        .await
        .expect("session");
    let running = Arc::clone(&kernel);
    let run_session = session_id.clone();
    let (events, _receiver) = tokio::sync::mpsc::channel(1024);
    let run = tokio::spawn(async move {
        running
            .run(
                protocol::RunRequest {
                    session_id: run_session,
                    input: "hang".into(),
                },
                Arc::new(kernel::ChannelEventSink::new(events)),
            )
            .await
    });
    tokio::time::sleep(SETTLE).await;
    with_connection(&acp(&kernel), async |connection, _updates| {
        connection.send_notification(wire::CancelSessionNotification::new(
            session_id.to_string(),
        ))?;
        // Keep the connection open until the notification has been dispatched.
        tokio::time::sleep(SETTLE).await;
        Ok(())
    })
    .await;
    tokio::time::timeout(WAIT, run)
        .await
        .expect("cancel stops the run")
        .expect("join")
        .expect("cancelled run settles");
}

/// Close cancels foreground work and delivers idle(cancelled) before its response.
#[tokio::test]
async fn close_projects_idle_cancelled_before_response() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(&session_id, "hang"))
                .block_task()
                .await?;
            connection
                .send_request(wire::CloseSessionRequest::new(
                    session_id.clone(),
                ))
                .block_task()
                .await?;
            assert!(
                dispatched(updates)
                    .iter()
                    .any(|value| is_idle(value, "cancelled"))
            );
            Ok(())
        },
    )
    .await;
}

/// Shell prompts waiting behind another run are cancelled before process creation.
#[tokio::test]
async fn waiting_user_bash_cancel_and_close_return_32800() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let markers = [
        workspace.path().join("bang"),
        workspace.path().join("excluded"),
        workspace.path().join("closed"),
    ];
    let commands = [
        format!("!touch {}", markers[0].display()),
        format!("!!touch {}", markers[1].display()),
        format!("!touch {}", markers[2].display()),
    ];
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(&session_id, "hang"))
                .block_task()
                .await?;
            let bang = connection
                .send_request(prompt(&session_id, &commands[0]))
                .block_task();
            let excluded = connection
                .send_request(prompt(&session_id, &commands[1]))
                .block_task();
            let cancel = async {
                tokio::time::sleep(SETTLE).await;
                connection.send_notification(
                    wire::CancelSessionNotification::new(session_id.clone()),
                )
            };
            let (bang, excluded, cancelled) =
                tokio::join!(bang, excluded, cancel);
            cancelled?;
            for result in [bang, excluded] {
                let error = result.expect_err("waiting shell is cancelled");
                assert_eq!(error.code, (-32800).into());
            }

            connection
                .send_request(prompt(&session_id, "hang"))
                .block_task()
                .await?;
            let closed = connection
                .send_request(prompt(&session_id, &commands[2]))
                .block_task();
            let close = async {
                tokio::time::sleep(SETTLE).await;
                connection
                    .send_request(wire::CloseSessionRequest::new(
                        session_id.clone(),
                    ))
                    .block_task()
                    .await
            };
            let (closed, close) = tokio::join!(closed, close);
            close?;
            let error = closed.expect_err("waiting shell is rejected by close");
            assert_eq!(error.code, (-32800).into());
            Ok(())
        },
    )
    .await;
    tokio::time::sleep(SETTLE).await;
    assert!(markers.iter().all(|marker| !marker.exists()));
}

/// An already idle session closes without waiting for a nonexistent terminal event.
#[tokio::test]
async fn idle_close_does_not_wait_for_new_terminal_event() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(&session_id, "hello"))
                .block_task()
                .await?;
            next_matching(updates, |value| is_idle(value, "end_turn")).await;
            tokio::time::timeout(
                WAIT,
                connection
                    .send_request(wire::CloseSessionRequest::new(
                        session_id.clone(),
                    ))
                    .block_task(),
            )
            .await
            .expect("idle close returns promptly")?;
            Ok(())
        },
    )
    .await;
}

/// A close stuck behind a run that ignores cancellation does not freeze the connection.
#[tokio::test]
async fn pending_close_does_not_block_other_sessions_on_connection() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = blocking_kernel(state.path(), &entered, &release);
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, blocked, updates| {
            let blocked_prompt = connection
                .send_request(prompt(&blocked, "block"))
                .block_task();
            let rest = async {
                entered.notified().await;
                let close = connection
                    .send_request(wire::CloseSessionRequest::new(
                        blocked.clone(),
                    ))
                    .block_task();
                let other = async {
                    tokio::time::sleep(SETTLE).await;
                    let created = connection
                        .send_request(wire::NewSessionRequest::new(
                            workspace.path().to_path_buf(),
                        ))
                        .block_task()
                        .await?;
                    let answered = connection
                        .send_request(prompt(&created.session_id, "hello"))
                        .block_task()
                        .await?;
                    release.notify_one();
                    Ok::<_, agent_client_protocol::Error>(answered)
                };
                let (close, other) = tokio::join!(close, other);
                other?;
                close?;
                Ok::<_, agent_client_protocol::Error>(())
            };
            let (blocked_prompt, rest) = tokio::join!(blocked_prompt, rest);
            rest?;
            let error = blocked_prompt.expect_err("closed before insertion");
            assert_eq!(error.code, (-32800).into());
            assert!(
                dispatched(updates)
                    .iter()
                    .any(|value| is_idle(value, "cancelled"))
            );
            Ok(())
        },
    )
    .await;
}

/// A close that outlives the grace period answers with a closing error and finishes in the background.
#[tokio::test(start_paused = true)]
async fn close_past_grace_period_reports_closing_then_completes() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = blocking_kernel(state.path(), &entered, &release);
    let closed_kernel = Arc::clone(&kernel);
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            let blocked_prompt = connection
                .send_request(prompt(&session_id, "block"))
                .block_task();
            let close = async {
                entered.notified().await;
                let error = connection
                    .send_request(wire::CloseSessionRequest::new(
                        session_id.clone(),
                    ))
                    .block_task()
                    .await
                    .expect_err("close outlives its grace period");
                assert_eq!(error.code, (-32603).into());
                let data = error.data.expect("data");
                assert_eq!(data["sessionId"], session_id.to_string());
                assert_eq!(data["closing"], true);
                release.notify_one();
            };
            let (blocked_prompt, ()) = tokio::join!(blocked_prompt, close);
            blocked_prompt.expect_err("closed before insertion");
            let closed_session =
                protocol::SessionId::try_from(session_id.to_string())
                    .expect("id");
            // Only a live Session accepts cancel; the background close removes it.
            while closed_kernel.cancel_session(&closed_session).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        },
    )
    .await;
}

/// Closing during /compact reports idle(cancelled) before the close response.
#[tokio::test]
async fn compact_cancel_projects_idle_cancelled() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            // Two exchanges larger than the 20k-token recent window give /compact
            // an older Turn to summarize.
            for _ in 0..2 {
                connection
                    .send_request(prompt(&session_id, &"x".repeat(100_000)))
                    .block_task()
                    .await?;
                next_matching(updates, |value| is_idle(value, "end_turn"))
                    .await;
            }
            connection
                .send_request(prompt(&session_id, "/compact"))
                .block_task()
                .await?;
            tokio::time::sleep(SETTLE).await;
            connection
                .send_request(wire::CloseSessionRequest::new(
                    session_id.clone(),
                ))
                .block_task()
                .await?;
            assert!(
                dispatched(updates)
                    .iter()
                    .any(|value| is_idle(value, "cancelled"))
            );
            Ok(())
        },
    )
    .await;
}

/// Waits for the first notification that satisfies `check`, keeping its metadata.
async fn next_notification(
    updates: &mut Updates,
    check: impl Fn(&wire::UpdateSessionNotification) -> bool,
) -> wire::UpdateSessionNotification {
    tokio::time::timeout(WAIT, async {
        loop {
            let update = updates.recv().await.expect("update stream");
            if check(&update) {
                return update;
            }
        }
    })
    .await
    .expect("matching notification")
}

/// Reads the recovery cursor carried by an Operation start notification.
fn start_cursor(
    update: &wire::UpdateSessionNotification,
) -> Option<(String, u64)> {
    let product = update.meta.as_ref()?.get("clawcode")?;
    let recovery = product.get("sessionRecovery")?;
    (recovery.get("operationPhase")?.as_str()? == "start").then_some(())?;
    Some((
        recovery.get("runId")?.as_str()?.to_string(),
        product.get("sequence")?.as_u64()?,
    ))
}

/// Builds a resume request that replays from the Session start.
fn resume_from_start(
    session_id: &wire::SessionId,
    workspace: &Path,
) -> wire::ResumeSessionRequest {
    wire::ResumeSessionRequest::new(session_id.clone(), workspace.to_path_buf())
        .replay_from(wire::ReplayFrom::from(wire::ReplayFromStart::new()))
}

/// Builds a resume request that replays one Operation journal from a cursor.
fn resume_from_cursor(
    session_id: &wire::SessionId,
    workspace: &Path,
    (run_id, sequence): &(String, u64),
) -> wire::ResumeSessionRequest {
    wire::ResumeSessionRequest::new(session_id.clone(), workspace.to_path_buf())
        .replay_from(wire::ReplayFrom::Other(wire::OtherReplayFrom::new(
            "_clawcode/event",
            std::collections::BTreeMap::from([
                ("runId".to_string(), serde_json::json!(run_id)),
                ("sequence".to_string(), serde_json::json!(sequence)),
            ]),
        )))
}

/// Resumes on a fresh connection and returns every notification sent before the response.
async fn resume_notifications(
    factory: &Arc<AcpServerFactory>,
    request: wire::ResumeSessionRequest,
) -> Vec<serde_json::Value> {
    let mut received = Vec::new();
    with_connection(factory, async |connection, updates| {
        connection.send_request(request).block_task().await?;
        received = dispatched(updates);
        Ok(())
    })
    .await;
    received
}

/// Returns the positions and values of state updates in a notification list.
fn state_updates(
    values: &[serde_json::Value],
) -> Vec<(usize, &serde_json::Value)> {
    values
        .iter()
        .enumerate()
        .filter(|(_index, value)| value["sessionUpdate"] == "state_update")
        .collect()
}

/// Creates a Session on its own connection, runs `body`, and returns the Session id.
async fn prepared_session<B>(
    factory: &Arc<AcpServerFactory>,
    workspace: &Path,
    body: B,
) -> wire::SessionId
where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        &wire::SessionId,
        &mut Updates,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let mut prepared = None;
    with_session(
        factory,
        workspace,
        async |connection, session_id, updates| {
            body(connection, &session_id, updates).await?;
            prepared = Some(session_id);
            Ok(())
        },
    )
    .await;
    prepared.expect("prepared Session")
}

/// Replay from start carries no historical state, only the current snapshot after the messages.
#[tokio::test]
async fn resume_replay_excludes_historical_state_updates() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(session_id, "hello"))
                .block_task()
                .await?;
            next_matching(updates, |value| is_idle(value, "end_turn")).await;
            Ok(())
        },
    )
    .await;
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    let states = state_updates(&replayed);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1["state"], "idle");
    assert!(states[0].1.get("stopReason").is_none());
    let last_message = replayed
        .iter()
        .rposition(|value| value["sessionUpdate"] == "agent_message")
        .expect("replayed answer");
    assert!(states[0].0 > last_message);
}

/// Resuming a running Session reports a running snapshot as its current state.
#[tokio::test]
async fn resume_of_running_session_reports_running_snapshot() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(session_id, "hang"))
                .block_task()
                .await?;
            Ok(())
        },
    )
    .await;
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    let states = state_updates(&replayed);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1["state"], "running");
    assert_eq!(states[0].0, replayed.len() - 1);
    kernel
        .cancel_session(
            &protocol::SessionId::try_from(session_id.to_string()).expect("id"),
        )
        .expect("cancel");
}

/// Cursor replay of a completed journal also drops its historical running and idle states.
#[tokio::test]
async fn cursor_resume_excludes_historical_state_updates() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut cursor = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(session_id, "hello"))
                .block_task()
                .await?;
            let started = next_notification(updates, |update| {
                start_cursor(update).is_some()
            })
            .await;
            cursor = start_cursor(&started);
            next_matching(updates, |value| is_idle(value, "end_turn")).await;
            Ok(())
        },
    )
    .await;
    let cursor = cursor.expect("cursor");
    let replayed = resume_notifications(
        &factory,
        resume_from_cursor(&session_id, workspace.path(), &cursor),
    )
    .await;
    let states = state_updates(&replayed);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1["state"], "idle");
    assert!(states[0].1.get("stopReason").is_none());
}

/// A running user-bash journal reconstructs acceptance before its result is durable.
#[tokio::test]
async fn resume_running_user_bash_restores_message_and_running_state() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut accepted = None;
    let mut cursor = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            let response = connection
                .send_request(prompt(session_id, "!sleep 5"))
                .block_task()
                .await?;
            accepted = Some(response.message_id.to_string());
            let started = next_notification(updates, |update| {
                start_cursor(update).is_some()
            })
            .await;
            cursor = start_cursor(&started);
            Ok(())
        },
    )
    .await;
    let accepted = accepted.expect("accepted id");
    for request in [
        resume_from_start(&session_id, workspace.path()),
        resume_from_cursor(
            &session_id,
            workspace.path(),
            cursor.as_ref().expect("cursor"),
        ),
    ] {
        let replayed = resume_notifications(&factory, request).await;
        let message = replayed
            .iter()
            .position(|value| is_user_message(value, &accepted))
            .expect("restored shell message");
        let states = state_updates(&replayed);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].1["state"], "running");
        assert!(states[0].0 > message);
    }
    kernel
        .cancel_session(
            &protocol::SessionId::try_from(session_id.to_string()).expect("id"),
        )
        .expect("cancel");
}

/// Reconnection does not own or cancel the shell operation.
#[tokio::test]
async fn user_bash_survives_disconnect_and_resumes_idle_after_completion() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut accepted = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, _updates| {
            let response = connection
                .send_request(prompt(session_id, "!sleep 0.3; echo done"))
                .block_task()
                .await?;
            accepted = Some(response.message_id.to_string());
            Ok(())
        },
    )
    .await;
    let accepted = accepted.expect("accepted id");
    let id = protocol::SessionId::try_from(session_id.to_string()).expect("id");
    tokio::time::timeout(WAIT, async {
        while kernel.session_runtime(&id).expect("runtime").running {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shell completes after disconnect");
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    assert!(
        replayed
            .iter()
            .any(|value| is_user_message(value, &accepted))
    );
    let states = state_updates(&replayed);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1["state"], "idle");
}

/// Sends one initialize request with an explicit protocol version.
async fn initialize_with(
    version: u16,
) -> Result<wire::InitializeResponse, agent_client_protocol::Error> {
    let state = tempfile::tempdir().expect("state");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let mut outcome = None;
    Client
        .v2()
        .connect_with(
            acp(&kernel).component(AcpTransportKind::Stdio),
            async |connection| {
                outcome = Some(
                    connection
                        .send_request(wire::InitializeRequest::new(
                            ProtocolVersion::from(version),
                            wire::Implementation::new(
                                "negotiation-test",
                                "0.1.0",
                            ),
                        ))
                        .block_task()
                        .await,
                );
                Ok(())
            },
        )
        .await
        .expect("ACP connection");
    outcome.expect("initialize outcome")
}

/// A client asking for a newer version is answered with the highest supported one.
#[tokio::test]
async fn initialize_selects_v2_for_newer_request() {
    let response = initialize_with(3).await.expect("initialize");
    let json = serde_json::to_value(&response).expect("serialize");
    assert_eq!(json["protocolVersion"], 2);
    let methods = json["_meta"]["clawcode"]["methods"]
        .as_array()
        .expect("advertised methods");
    assert!(
        methods
            .iter()
            .any(|value| value == "_clawcode/session/pending_message/promote")
    );
    assert!(!methods.iter().any(|value| value == "_clawcode/followUp"));
}

/// A client that only speaks an older version is rejected with the supported list.
#[tokio::test]
async fn initialize_rejects_older_version_with_32022() {
    let state = tempfile::tempdir().expect("state");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let mut outcome = None;
    // A v2 client rewrites its own request version, so a v1 client sends the old one.
    Client
        .builder()
        .connect_with(
            acp(&kernel).component(AcpTransportKind::Stdio),
            async |connection| {
                outcome = Some(
                    connection
                        .send_request(
                            agent_client_protocol::schema::v1::InitializeRequest::new(
                                ProtocolVersion::V1,
                            ),
                        )
                        .block_task()
                        .await,
                );
                Ok(())
            },
        )
        .await
        .expect("ACP connection");
    let error = outcome
        .expect("initialize outcome")
        .expect_err("unsupported version");
    assert_eq!(error.code, (-32022).into());
    assert_eq!(
        error.data.expect("data"),
        serde_json::json!({"requested": 1, "supported": [2]})
    );
}

/// A connection that resumes a running shell receives its live completion.
#[tokio::test]
async fn resumed_running_user_bash_receives_live_completion() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(session_id, "!sleep 1; echo live-done"))
                .block_task()
                .await?;
            Ok(())
        },
    )
    .await;
    with_connection(&factory, async |connection, updates| {
        connection
            .send_request(resume_from_start(&session_id, workspace.path()))
            .block_task()
            .await?;
        next_matching(updates, |value| is_idle(value, "end_turn")).await;
        Ok(())
    })
    .await;
}

/// A second connection resuming while the first still watches also gets the live completion.
#[tokio::test]
async fn concurrent_resume_of_running_user_bash_receives_live_completion() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    with_session(
        &factory,
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(&session_id, "!sleep 1; echo live-done"))
                .block_task()
                .await?;
            with_connection(&factory, async |second, updates| {
                second
                    .send_request(resume_from_start(
                        &session_id,
                        workspace.path(),
                    ))
                    .block_task()
                    .await?;
                next_matching(updates, |value| is_idle(value, "end_turn"))
                    .await;
                Ok(())
            })
            .await;
            Ok(())
        },
    )
    .await;
}

/// Replayed projection groups stay complete after historical states are dropped.
#[tokio::test]
async fn replayed_projection_groups_stay_complete() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(session_id, "!sleep 5"))
                .block_task()
                .await?;
            Ok(())
        },
    )
    .await;
    let mut groups: std::collections::BTreeMap<(String, u64), (u64, usize)> =
        std::collections::BTreeMap::new();
    with_connection(&factory, async |connection, updates| {
        connection
            .send_request(resume_from_start(&session_id, workspace.path()))
            .block_task()
            .await?;
        while let Ok(update) = updates.try_recv() {
            let Some(product) =
                update.meta.as_ref().and_then(|meta| meta.get("clawcode"))
            else {
                continue;
            };
            let Some(recovery) = product.get("sessionRecovery") else {
                continue;
            };
            let key = (
                recovery["runId"].as_str().expect("run id").to_string(),
                product["sequence"].as_u64().expect("sequence"),
            );
            let count = recovery["projectionCount"].as_u64().expect("count");
            groups.entry(key).or_insert((count, 0)).1 += 1;
        }
        Ok(())
    })
    .await;
    assert!(!groups.is_empty());
    for ((run_id, sequence), (count, received)) in groups {
        assert_eq!(
            count as usize, received,
            "group {run_id}@{sequence} declared {count} notifications"
        );
    }
    kernel
        .cancel_session(
            &protocol::SessionId::try_from(session_id.to_string()).expect("id"),
        )
        .expect("cancel");
}

/// Session setup responses advertise the initial command list.
#[tokio::test]
async fn session_new_returns_available_commands() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_connection(&acp(&kernel), async |connection, _updates| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(
                workspace.path().to_path_buf(),
            ))
            .block_task()
            .await?;
        let json = serde_json::to_value(&created).expect("serialize");
        assert!(
            json["availableCommands"]
                .as_array()
                .expect("commands")
                .iter()
                .any(|command| command["name"] == "session")
        );
        Ok(())
    })
    .await;
}

/// Resume reports the current title before the current state, both after replay.
#[tokio::test]
async fn resume_sends_session_info_before_state_snapshot() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(session_id, "titled prompt"))
                .block_task()
                .await?;
            next_matching(updates, |value| is_idle(value, "end_turn")).await;
            Ok(())
        },
    )
    .await;
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    let tail: Vec<_> = replayed.iter().rev().take(2).rev().collect();
    assert_eq!(tail[0]["sessionUpdate"], "session_info_update");
    assert_eq!(tail[0]["title"], "titled prompt");
    assert_eq!(tail[1]["sessionUpdate"], "state_update");
}

/// The cancellation marker survives storage and a fresh connection replay.
#[tokio::test]
async fn cancelled_tool_remains_cancelled_after_durable_resume() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(session_id, "tool"))
                .block_task()
                .await?;
            next_matching(updates, |value| {
                value["sessionUpdate"] == "tool_call_update"
                    && value["status"] == "in_progress"
            })
            .await;
            tokio::time::sleep(SETTLE).await;
            connection.send_notification(
                wire::CancelSessionNotification::new(session_id.clone()),
            )?;
            next_matching(updates, |value| is_idle(value, "cancelled")).await;
            Ok(())
        },
    )
    .await;
    // Closing drops the retained journal, so resume replays the stored message.
    kernel
        .close_session(
            &protocol::SessionId::try_from(session_id.to_string()).expect("id"),
        )
        .await
        .expect("close");
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    let tool = replayed
        .iter()
        .rev()
        .find(|value| value["sessionUpdate"] == "tool_call_update")
        .expect("replayed tool");
    assert_eq!(tool["status"], "cancelled");
}

/// A live notice is shown once and never returns through replay.
#[tokio::test]
async fn notices_are_not_replayed_on_resume() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        hooks(|registrar| registrar.on::<InputPoint, _>(FailingInput)),
    );
    let factory = acp(&kernel);
    let mut cursor = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(session_id, "hello"))
                .block_task()
                .await?;
            // The input hook runs before RunStart, so its notice arrives first.
            next_matching(updates, |value| value["sessionUpdate"] == "notice")
                .await;
            let started = next_notification(updates, |update| {
                start_cursor(update).is_some()
            })
            .await;
            cursor = start_cursor(&started);
            next_matching(updates, |value| is_idle(value, "end_turn")).await;
            Ok(())
        },
    )
    .await;
    for request in [
        resume_from_start(&session_id, workspace.path()),
        resume_from_cursor(
            &session_id,
            workspace.path(),
            cursor.as_ref().expect("cursor"),
        ),
    ] {
        let replayed = resume_notifications(&factory, request).await;
        assert!(
            replayed
                .iter()
                .all(|value| value["sessionUpdate"] != "notice")
        );
    }
}

/// Startup prompt diagnostics arrive as notices after the Session id is known.
#[tokio::test]
async fn prompt_diagnostics_follow_session_new_response() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    // An instruction file that is not UTF-8 makes prompt loading report a warning.
    std::fs::write(workspace.path().join("AGENTS.md"), [0xff, 0xfe, 0xfd])
        .expect("write instruction file");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_connection(&acp(&kernel), async |connection, updates| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(
                workspace.path().to_path_buf(),
            ))
            .block_task()
            .await?;
        // The server answers first, so the notice names a Session the client already knows.
        let notice = next_notification(updates, |update| {
            json(update)["sessionUpdate"] == "notice"
        })
        .await;
        assert_eq!(notice.session_id, created.session_id);
        assert_eq!(json(&notice)["title"], "Prompt resource issue");
        Ok(())
    })
    .await;
}

/// Re-attaching a Session on the same connection does not repeat its startup diagnostics.
#[tokio::test]
async fn resume_on_attached_connection_skips_repeated_prompt_notices() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("AGENTS.md"), [0xff, 0xfe, 0xfd])
        .expect("write instruction file");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut session_id = None;
    let mut first_attach = 0;
    with_connection(&factory, async |connection, updates| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(
                workspace.path().to_path_buf(),
            ))
            .block_task()
            .await?;
        // The WebUI attaches a Session it just created through session/resume.
        connection
            .send_request(resume_from_start(
                &created.session_id,
                workspace.path(),
            ))
            .block_task()
            .await?;
        let notices = drain(updates)
            .await
            .into_iter()
            .filter(|update| update["sessionUpdate"] == "notice")
            .count();
        first_attach = notices;
        session_id = Some(created.session_id);
        Ok(())
    })
    .await;
    let session_id = session_id.expect("created Session");
    // A fresh connection, such as a reloaded page, still sees them once.
    with_connection(&factory, async |connection, updates| {
        connection
            .send_request(resume_from_start(&session_id, workspace.path()))
            .block_task()
            .await?;
        let notices = drain(updates)
            .await
            .into_iter()
            .filter(|update| update["sessionUpdate"] == "notice")
            .count();
        assert!(notices > 0);
        // new + resume on one connection sent what one fresh attach sends.
        assert_eq!(first_attach, notices);
        Ok(())
    })
    .await;
}

/// Sets one Session config option and returns the full option list.
async fn set_config(
    connection: &V2ConnectionTo<Agent>,
    session_id: &wire::SessionId,
    config_id: &str,
    value: &str,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let response = connection
        .send_request(wire::SetSessionConfigOptionRequest::new(
            session_id.clone(),
            config_id.to_string(),
            wire::SessionConfigOptionValue::Id {
                value: value.to_string().into(),
            },
        ))
        .block_task()
        .await?;
    Ok(serde_json::to_value(response).expect("serialize response"))
}

/// Returns the current value of one option in a serialized option list.
fn current(options: &serde_json::Value, config_id: &str) -> serde_json::Value {
    options
        .as_array()
        .expect("config options")
        .iter()
        .find(|option| option["configId"] == config_id)
        .map(|option| option["currentValue"].clone())
        .expect("config option")
}

/// session/new offers the model and thinking selects with their current values.
#[tokio::test]
async fn session_new_returns_model_and_thinking_options() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_connection(&acp(&kernel), async |connection, _updates| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(
                workspace.path().to_path_buf(),
            ))
            .block_task()
            .await?;
        let json = serde_json::to_value(&created).expect("serialize");
        let options = &json["configOptions"];
        assert_eq!(options[0]["configId"], "model");
        assert_eq!(options[0]["category"], "model");
        assert_eq!(options[0]["type"], "select");
        assert_eq!(options[0]["currentValue"], "fixture/scripted");
        assert_eq!(options[0]["options"].as_array().expect("models").len(), 2);
        assert_eq!(options[1]["configId"], "thinking_level");
        assert_eq!(options[1]["category"], "thought_level");
        assert_eq!(options[1]["currentValue"], "off");
        Ok(())
    })
    .await;
}

/// Unknown option ids and values the option does not offer are invalid params.
#[tokio::test]
async fn invalid_config_value_returns_32602() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            for (config_id, value) in [
                ("model", "fixture/missing"),
                ("thinking_level", "extreme"),
                ("mode", "fast"),
            ] {
                let error =
                    set_config(connection, &session_id, config_id, value)
                        .await
                        .expect_err("invalid config value");
                assert_eq!(
                    i32::from(error.code),
                    -32602,
                    "{config_id}={value}"
                );
            }
            Ok(())
        },
    )
    .await;
}

/// A model id containing `/` is matched as a whole value, not split.
#[tokio::test]
async fn model_id_with_slash_matches() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            let response = set_config(
                connection,
                &session_id,
                "model",
                "openrouter/deepseek/deepseek-chat",
            )
            .await?;
            assert_eq!(
                current(&response["configOptions"], "model"),
                "openrouter/deepseek/deepseek-chat"
            );
            Ok(())
        },
    )
    .await;
}

/// The change's config_option_update reaches this client before the response.
#[tokio::test]
async fn config_option_update_precedes_response() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            drain(updates).await;
            set_config(connection, &session_id, "thinking_level", "high")
                .await?;
            // Already queued when the response resolved: no waiting here.
            let mut seen = Vec::new();
            while let Ok(update) = updates.try_recv() {
                seen.push(json(&update));
            }
            let update = seen
                .iter()
                .find(|update| {
                    update["sessionUpdate"] == "config_option_update"
                })
                .expect("config_option_update before response");
            assert_eq!(
                current(&update["configOptions"], "thinking_level"),
                "high"
            );
            Ok(())
        },
    )
    .await;
}

/// An unchanged value cannot create a wait for a SettingsChanged event that will never exist.
#[tokio::test]
async fn unchanged_config_does_not_wait_for_new_event() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            drain(updates).await;
            let response = tokio::time::timeout(
                WAIT,
                set_config(connection, &session_id, "thinking_level", "off"),
            )
            .await
            .expect("unchanged value answers promptly")?;
            assert_eq!(
                current(&response["configOptions"], "thinking_level"),
                "off"
            );
            assert!(drain(updates).await.iter().all(
                |update| update["sessionUpdate"] != "config_option_update"
            ));
            Ok(())
        },
    )
    .await;
}

/// A model chosen while a Run is active answers the next prompt.
#[tokio::test]
async fn config_change_during_run_applies_to_next_request() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let model = ScriptedModel::new();
    let kernel = alignment_kernel(
        state.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, updates| {
            connection
                .send_request(prompt(&session_id, "hold"))
                .block_task()
                .await?;
            set_config(
                connection,
                &session_id,
                "model",
                "openrouter/deepseek/deepseek-chat",
            )
            .await?;
            model.release.notify_one();
            next_matching(updates, |update| update["state"] == "idle").await;
            connection
                .send_request(prompt(&session_id, "next"))
                .block_task()
                .await?;
            let answer = next_matching(updates, |update| {
                update["sessionUpdate"] == "agent_message"
            })
            .await;
            assert_eq!(answer["content"][0]["text"], "second");
            Ok(())
        },
    )
    .await;
}

/// Concurrent changes from two connections and the Kernel never leave a client on a stale value.
#[tokio::test]
async fn concurrent_config_changes_converge_on_every_connection() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let levels = ["low", "medium", "high", "minimal", "x_high", "off"];
    with_session(
        &factory,
        workspace.path(),
        async |first, session_id, first_updates| {
            with_connection(&factory, async |second, second_updates| {
                second
                    .send_request(resume_from_start(
                        &session_id,
                        workspace.path(),
                    ))
                    .block_task()
                    .await?;
                let kernel_session =
                    protocol::SessionId::try_from(session_id.to_string())
                        .expect("Session id");
                let mut changes = Vec::new();
                for (index, level) in levels.iter().enumerate() {
                    changes.push(set_config(
                        first,
                        &session_id,
                        "thinking_level",
                        level,
                    ));
                    changes.push(set_config(
                        second,
                        &session_id,
                        "thinking_level",
                        levels[(index + 2) % levels.len()],
                    ));
                }
                let direct = async {
                    for level in [ThinkingLevel::Medium, ThinkingLevel::XHigh] {
                        kernel
                            .set_session_thinking_level(&kernel_session, level)
                            .await
                            .expect("direct change");
                    }
                };
                let (responses, ()) =
                    tokio::join!(futures::future::join_all(changes), direct);
                for response in responses {
                    response?;
                }
                let expected = serde_json::to_value(
                    kernel
                        .session_settings(&kernel_session)
                        .expect("settings")
                        .thinking_level,
                )
                .expect("level");
                // The last update each connection received equals the settled value.
                for updates in [&mut *first_updates, &mut *second_updates] {
                    let last = drain(updates)
                        .await
                        .into_iter()
                        .rfind(|update| {
                            update["sessionUpdate"] == "config_option_update"
                        })
                        .expect("config updates");
                    assert_eq!(
                        current(&last["configOptions"], "thinking_level"),
                        expected
                    );
                }
                Ok(())
            })
            .await;
            Ok(())
        },
    )
    .await;
}

/// Decodes one terminal update's base64 output.
fn terminal_text(update: &serde_json::Value) -> String {
    use base64::Engine as _;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(
                update
                    .pointer("/output/data")
                    .and_then(serde_json::Value::as_str)
                    .expect("output data"),
            )
            .expect("base64"),
    )
    .expect("utf-8")
}

/// Resuming a live shell rebuilds its tool and terminal from the same journal as its message.
#[tokio::test]
async fn running_user_bash_resume_restores_latest_terminal_output() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut accepted = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            let response = connection
                .send_request(prompt(
                    session_id,
                    "!echo first; sleep 0.3; echo second; sleep 5",
                ))
                .block_task()
                .await?;
            accepted = Some(response.message_id.to_string());
            // Wait until the second line reached this client live.
            next_notification(updates, |update| {
                let value = json(update);
                value["sessionUpdate"] == "terminal_update"
                    && value["output"]["data"].is_string()
                    && terminal_text(&value).contains("second")
            })
            .await;
            Ok(())
        },
    )
    .await;
    let accepted = accepted.expect("accepted id");
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    assert!(
        replayed
            .iter()
            .any(|value| is_user_message(value, &accepted))
    );
    let tool = replayed
        .iter()
        .find(|value| value["sessionUpdate"] == "tool_call_update")
        .expect("restored tool");
    assert_eq!(tool["toolCallId"], accepted);
    assert_eq!(tool["status"], "in_progress");
    let outputs = replayed
        .iter()
        .filter(|value| {
            value["sessionUpdate"] == "terminal_update"
                && value["terminalId"] == accepted.as_str()
                && value["output"]["data"].is_string()
        })
        .collect::<Vec<_>>();
    // Only the newest whole-tail snapshot is retained and replayed.
    assert_eq!(outputs.len(), 1);
    let text = terminal_text(outputs[0]);
    assert!(text.contains("first") && text.contains("second"));
    assert_eq!(state_updates(&replayed)[0].1["state"], "running");
    kernel
        .cancel_session(
            &protocol::SessionId::try_from(session_id.to_string()).expect("id"),
        )
        .expect("cancel");
}

/// Durable replay restores the saved directory and signal rather than current state.
#[cfg(unix)]
#[tokio::test]
async fn user_bash_terminal_replay_preserves_cwd_and_signal() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let kernel = alignment_kernel(
        state.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    let factory = acp(&kernel);
    let mut accepted = None;
    let session_id = prepared_session(
        &factory,
        workspace.path(),
        async |connection, session_id, updates| {
            let response = connection
                .send_request(prompt(session_id, "!echo going; kill -TERM $$"))
                .block_task()
                .await?;
            accepted = Some(response.message_id.to_string());
            next_matching(updates, |update| update["state"] == "idle").await;
            Ok(())
        },
    )
    .await;
    let accepted = accepted.expect("accepted id");
    let kernel_session =
        protocol::SessionId::try_from(session_id.to_string()).expect("id");
    // Closing drops the in-memory journal, so resume replays the stored message.
    kernel.close_session(&kernel_session).await.expect("close");
    let replayed = resume_notifications(
        &factory,
        resume_from_start(&session_id, workspace.path()),
    )
    .await;
    let terminal = replayed
        .iter()
        .find(|value| {
            value["sessionUpdate"] == "terminal_update"
                && value["terminalId"] == accepted.as_str()
        })
        .expect("replayed terminal");
    assert_eq!(terminal["exitStatus"]["signal"], "SIGTERM");
    let saved =
        std::path::PathBuf::from(terminal["cwd"].as_str().expect("saved cwd"));
    assert_eq!(
        saved.canonicalize().expect("saved cwd exists"),
        workspace.path().canonicalize().expect("workspace")
    );
    assert!(terminal_text(terminal).contains("going"));
    let tool = replayed
        .iter()
        .find(|value| value["sessionUpdate"] == "tool_call_update")
        .expect("replayed tool");
    assert_eq!(tool["status"], "failed");
}

/// A close keeps going after the connection that asked for it disconnects.
#[tokio::test]
async fn close_completes_after_requesting_connection_drops() {
    let state = tempfile::tempdir().expect("state");
    let workspace = tempfile::tempdir().expect("workspace");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = blocking_kernel(state.path(), &entered, &release);
    let mut closing = None;
    with_session(
        &acp(&kernel),
        workspace.path(),
        async |connection, session_id, _updates| {
            connection
                .send_request(prompt(&session_id, "block"))
                .detach();
            entered.notified().await;
            // The close waits on the blocked Run; this client leaves meanwhile.
            connection
                .send_request(wire::CloseSessionRequest::new(
                    session_id.clone(),
                ))
                .detach();
            tokio::time::sleep(SETTLE).await;
            closing = Some(session_id);
            Ok(())
        },
    )
    .await;
    release.notify_one();
    let closing = protocol::SessionId::try_from(
        closing.expect("closing Session").to_string(),
    )
    .expect("id");
    // Only a live Session accepts cancel; the finished close removes it.
    tokio::time::timeout(WAIT, async {
        while kernel.cancel_session(&closing).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("close finishes without its connection");
}

/// Metadata validation happens before the prompt can run or enter the queue.
#[tokio::test]
async fn invalid_busy_options_are_rejected_before_insertion() {
    let root = tempfile::tempdir().unwrap();
    let kernel = alignment_kernel(
        root.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        root.path(),
        async |connection, session, _updates| {
            for product in [
                serde_json::json!({"whenBusy":"follow_up"}),
                serde_json::json!({"whenBusy":7}),
                serde_json::json!({"submissionId":""}),
                serde_json::json!({"submissionId":"x".repeat(129)}),
            ] {
                let request = prompt(&session, "must not insert").meta(
                    wire::Meta::from_iter([("clawcode".into(), product)]),
                );
                let error = connection
                    .send_request(request)
                    .block_task()
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    agent_client_protocol::ErrorCode::InvalidParams
                );
            }
            assert!(
                kernel
                    .session_transcript(
                        &protocol::SessionId::try_from(session.to_string())
                            .unwrap()
                    )
                    .unwrap()
                    .is_empty()
            );
            Ok(())
        },
    )
    .await;
}

/// Rapid requests keep transport arrival order, insertion ids, and queue correlation together.
#[tokio::test]
async fn rapid_busy_prompts_insert_in_send_order() {
    let root = tempfile::tempdir().unwrap();
    let model = ScriptedModel::new();
    let kernel = alignment_kernel(
        root.path(),
        &model,
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(&acp(&kernel), root.path(), async |connection, session, updates| {
        connection.send_request(prompt(&session, "hold")).block_task().await?;
        let requests = (1..=5).map(|n| connection.send_request(prompt(&session, &format!("p{n}")).meta(wire::Meta::from_iter([("clawcode".into(), serde_json::json!({"submissionId":format!("submission-{n}")}))]))).block_task()).collect::<Vec<_>>();
        let release = async {
            let queued = next_matching(updates, |value| value["payload"]["event"] == "queue_changed" && value["payload"]["pending"]["followUp"].as_array().is_some_and(|items| items.len() == 5)).await;
            assert_eq!(queued["payload"]["pending"]["followUp"][4]["submissionId"], "submission-5");
            model.release.notify_one();
        };
        let (answers, ()) = tokio::join!(futures::future::join_all(requests), release);
        let messages = kernel.session_transcript(&protocol::SessionId::try_from(session.to_string()).unwrap()).unwrap();
        let users = messages.iter().filter(|m| matches!(m.content, MessageContent::User { .. })).collect::<Vec<_>>();
        assert_eq!(users.len(), 6);
        for (n, answer) in answers.into_iter().enumerate() {
            assert_eq!(answer?.message_id.to_string(), users[n+1].identity.message_id.to_string());
            assert_eq!(users[n+1].content.text_content().unwrap(), format!("p{}", n+1));
        }
        Ok(())
    }).await;
}

/// Close delivers an empty queue snapshot and cancellation for every queued request before returning.
#[tokio::test]
async fn close_delivers_final_queue_and_cancels_all_busy_prompts() {
    let root = tempfile::tempdir().unwrap();
    let kernel = alignment_kernel(
        root.path(),
        &ScriptedModel::new(),
        Arc::new(StaticExtensionFactory::default()),
    );
    with_session(
        &acp(&kernel),
        root.path(),
        async |connection, session, updates| {
            connection
                .send_request(prompt(&session, "hang"))
                .block_task()
                .await?;
            let requests = (1..=3)
                .map(|n| {
                    connection
                        .send_request(prompt(&session, &format!("q{n}")))
                        .block_task()
                })
                .collect::<Vec<_>>();
            let close = async {
                next_matching(updates, |value| {
                    value["payload"]["event"] == "queue_changed"
                        && value["payload"]["pending"]["followUp"]
                            .as_array()
                            .is_some_and(|items| items.len() == 3)
                })
                .await;
                connection
                    .send_request(wire::CloseSessionRequest::new(
                        session.clone(),
                    ))
                    .block_task()
                    .await
                    .unwrap();
                let delivered = dispatched(updates);
                let last = delivered
                    .iter()
                    .rfind(|v| v["payload"]["event"] == "queue_changed")
                    .expect("queue snapshot delivered before close response");
                assert_eq!(
                    last["payload"]["pending"],
                    serde_json::json!({"steering":[],"followUp":[]})
                );
            };
            let (answers, ()) =
                tokio::join!(futures::future::join_all(requests), close);
            for answer in answers {
                assert_eq!(
                    answer.unwrap_err().code,
                    agent_client_protocol::ErrorCode::RequestCancelled
                );
            }
            Ok(())
        },
    )
    .await;
}

#[path = "../../kernel/tests/support/failing_queue_store.rs"]
mod failing_queue_store;

/// Partial durable queue writes retain the outcomeUnknown marker through the real ACP insertion channel.
#[tokio::test]
async fn uncertain_queue_persistence_reaches_the_prompt_response() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for failure in ["enqueue", "consume", "remove", "clear", "promote"] {
        let root = tempfile::tempdir().unwrap();
        let model = ScriptedModel::new();
        let fail_sync = Arc::new(AtomicBool::new(false));
        let fail_consumed = Arc::new(AtomicBool::new(false));
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let kernel = Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(ScriptedModelFactory(Arc::clone(
                    &model,
                ))))
                .tool_factory(Arc::new(BuiltinToolFactory::new()))
                .store_factory(Arc::new(
                    failing_queue_store::FailingQueueStoreFactory {
                        inner: JsonlStoreFactory::new(
                            root.path(),
                            Arc::clone(&clock),
                        ),
                        fail_sync: Arc::clone(&fail_sync),
                        fail_consumed: Arc::clone(&fail_consumed),
                    },
                ))
                .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                    root.path().join("config"),
                    protocol::PromptPolicy::default(),
                )))
                .extension_factory(Arc::new(StaticExtensionFactory::default()))
                .clock(clock)
                .id_generator(Arc::new(NanoidIdGenerator))
                .build()
                .build()
                .unwrap(),
        );
        with_session(&acp(&kernel), root.path(), async |connection, session, updates| {
            connection.send_request(prompt(&session, "hold")).block_task().await?;
            if failure == "enqueue" { fail_sync.store(true, Ordering::SeqCst); }
            let queued = connection.send_request(prompt(&session, "uncertain")).block_task();
            let release = async {
                if failure != "enqueue" {
                    let snapshot = next_matching(updates, |value| value["payload"]["event"] == "queue_changed" && value["payload"]["pending"]["followUp"].as_array().is_some_and(|q| q.len() == 1)).await;
                    if failure == "consume" {
                        fail_consumed.store(true, Ordering::SeqCst);
                        model.release.notify_one();
                    } else {
                        let queue_id = &snapshot["payload"]["pending"]["followUp"][0]["queueId"];
                        let (method, params) = match failure {
                            "remove" => (protocol::AcpExtensionMethod::PendingMessageRemove, serde_json::json!({"sessionId": session, "queueId": queue_id})),
                            "promote" => (protocol::AcpExtensionMethod::PendingMessagePromote, serde_json::json!({"sessionId": session, "queueId": queue_id})),
                            _ => (protocol::AcpExtensionMethod::ClearQueue, serde_json::json!({"sessionId": session})),
                        };
                        fail_sync.store(true, Ordering::SeqCst);
                        let error = connection.send_request(agent_client_protocol::UntypedMessage::new(method.as_str(), &params).unwrap()).block_task().await.unwrap_err();
                        assert_eq!(error.code, agent_client_protocol::ErrorCode::InternalError);
                        assert_eq!(error.data.as_ref().unwrap()["outcomeUnknown"], true);
                        // A poisoned transcript also prevents stop from reporting a safe withdrawal.
                        assert!(matches!(kernel.cancel_session(&protocol::SessionId::try_from(session.to_string()).unwrap()), Err(kernel::KernelError::SessionPersistenceUncertain)));
                    }
                }
            };
            let (answer, ()) = tokio::join!(queued, release);
            let error = answer.unwrap_err();
            assert_eq!(error.code, agent_client_protocol::ErrorCode::InternalError);
            assert_eq!(error.data.as_ref().unwrap()["outcomeUnknown"], true);
            if failure == "enqueue" {
                tokio::time::sleep(SETTLE).await;
                assert!(!dispatched(updates).iter().any(|value| value["sessionUpdate"] == "state_update" && value["state"] == "idle"), "a rejected queued prompt must not settle the active Run");
            }
            kernel.cancel_session(&protocol::SessionId::try_from(session.to_string()).unwrap()).unwrap();
            model.release.notify_one();
            connection.send_request(wire::CloseSessionRequest::new(session)).block_task().await?;
            Ok(())
        }).await;
    }
}

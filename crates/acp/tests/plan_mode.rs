//! Plan mode over ACP: the mode option, plan updates, and plan approval.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::{
    Agent, Client, ErrorCode,
    v2::{ConnectionTo as V2ConnectionTo, Responder},
};
use agent_client_protocol::{schema::ProtocolVersion, v2::schema as wire};
use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, StaticExtensionFactory, ToolCallPoint,
};
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    ContentBlock, ExtensionDescriptor, ExtensionId, MessageContent, ModelFinal,
    ModelProfile, ModelRequest, ModelStreamEvent, StaticExtensionRegistration,
    StopReason, ToolBlock, ToolCallEvent, ToolCallResult,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Bounds every wait so a regression fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);
/// Gives work that must not happen time to happen anyway.
const SETTLE: Duration = Duration::from_millis(200);
/// The plan the scripted model submits.
const PLAN: &str = "1. Read the code.\n2. Write the change.";

type Updates = mpsc::UnboundedReceiver<wire::UpdateSessionNotification>;
type Asked = (
    wire::RequestPermissionRequest,
    Responder<wire::RequestPermissionResponse>,
);

/// Model routed by the newest user text; anything else is answered.
struct PlanModel {
    profile: ModelProfile,
}

impl PlanModel {
    /// Builds one stream that calls a tool and stops for its result.
    fn calls(calls: Vec<(&str, &str, serde_json::Value)>) -> ModelStream {
        let mut events = calls
            .into_iter()
            .map(|(id, name, arguments)| {
                Ok(ModelStreamEvent::ToolCall(
                    protocol::ToolCall::builder()
                        .tool_call_id(
                            protocol::ToolCallId::try_from(id).expect("id"),
                        )
                        .name(name.to_string())
                        .arguments(arguments)
                        .build(),
                ))
            })
            .collect::<Vec<_>>();
        events.push(Ok(Self::finished(StopReason::ToolUse)));
        Box::pin(stream::iter(events))
    }

    /// Builds one terminal event.
    fn finished(stop_reason: StopReason) -> ModelStreamEvent {
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(stop_reason)
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
}

/// Reads the newest user text of a provider request.
fn last_user_text(request: &ModelRequest) -> String {
    match request.messages.last().map(|message| &message.content) {
        // A spawned child starts from its `new_task` mail; route on the payload.
        Some(MessageContent::Extension { extension })
            if extension.custom_type == protocol::AGENT_MESSAGE_TYPE =>
        {
            extension
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => {
                        text.split("\nPayload:\n").nth(1)
                    }
                    _ => None,
                })
                .collect()
        }
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

/// One todo list with a finished, a current, and a waiting item.
fn todos(first: &str) -> serde_json::Value {
    serde_json::json!({ "todos": [
        { "content": first, "status": "completed", "activeForm": "Reading" },
        { "content": "Write the change", "status": "in_progress", "activeForm": "Writing the change" },
        { "content": "Run the tests", "status": "pending", "activeForm": "Running the tests" },
    ]})
}

#[async_trait]
impl Model for PlanModel {
    /// Returns the deterministic fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Always ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Calls the tool the prompt names, or answers.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Ok(match last_user_text(&request).as_str() {
            "plan it" => Self::calls(vec![(
                "call-exit",
                "exit_plan_mode",
                serde_json::json!({ "plan": PLAN }),
            )]),
            "todo" => Self::calls(vec![(
                "call-todo",
                "todo_write",
                todos("Read the code"),
            )]),
            "todo bad" => Self::calls(vec![(
                "call-todo-bad",
                "todo_write",
                serde_json::json!({ "todos": [{ "content": "a", "status": "blocked", "activeForm": "a" }] }),
            )]),
            "todo again" => Self::calls(vec![(
                "call-todo-again",
                "todo_write",
                todos("Read the tests"),
            )]),
            "delegate" => Self::calls(vec![(
                "call-child",
                "spawn_agent",
                serde_json::json!({
                    "task_name": "tracker",
                    "message": "Track it.",
                    "fork_turns": "none"
                }),
            )]),
            "Track it." => Self::calls(vec![(
                "call-child-todo",
                "todo_write",
                todos("Child step"),
            )]),
            _ => Box::pin(stream::iter([
                Ok(ModelStreamEvent::TextDelta("done".to_string())),
                Ok(Self::finished(StopReason::EndTurn)),
            ])),
        })
    }
}

/// Offers the one scripted model.
struct PlanModelFactory(Arc<PlanModel>);

impl ModelFactory for PlanModelFactory {
    /// Returns the shared scripted model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }

    /// Builds a one-model catalog.
    fn create_catalog(
        &self,
        _registration: &StaticExtensionRegistration,
    ) -> Result<kernel::ModelCatalog, ModelError> {
        kernel::ModelCatalog::new("fixture", "plan", vec![self.create()?])
    }
}

/// Extension wrapping one registration closure.
struct HookModule<F>(F);

impl<F> ExtensionModule for HookModule<F>
where
    F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionError> + Send + Sync,
{
    /// Identifies the plan-mode test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("plan-mode-hooks").expect("id"),
            name: "Plan mode hooks".into(),
            version: "1".into(),
        }
    }

    /// Registers the test's handlers.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        (self.0)(registrar)
    }
}

/// Rewrites submitted plans and blocks todo lists, as a policy extension could.
#[derive(Clone)]
struct ToolPolicy {
    rewrite_plan: bool,
    block_todos: bool,
}

#[async_trait]
impl ExtensionHandler<ToolCallPoint> for ToolPolicy {
    /// Changes the plan argument or refuses `todo_write`.
    async fn handle(
        &self,
        event: &ToolCallEvent,
        _context: &ExtensionContext,
    ) -> Result<ToolCallResult, ExtensionError> {
        if self.rewrite_plan && event.call.name == "exit_plan_mode" {
            return Ok(ToolCallResult::Replace {
                arguments: serde_json::json!({ "plan": "REWRITTEN PLAN" }),
            });
        }
        if self.block_todos && event.call.name == "todo_write" {
            return Ok(ToolCallResult::Block(
                ToolBlock::builder()
                    .reason("todos are blocked".to_string())
                    .terminate(false)
                    .build(),
            ));
        }
        Ok(ToolCallResult::Continue)
    }
}

/// Builds a production Kernel and ACP server around the scripted model.
fn server(
    root: &Path,
    policy: ToolPolicy,
) -> (Arc<Kernel>, Arc<AcpServerFactory>) {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let model = Arc::new(PlanModel {
        profile: ModelProfile::builder()
            .provider_id("fixture".to_string())
            .model_id("plan".to_string())
            .display_name("Plan fixture".to_string())
            .context_tokens(1_000_000)
            .max_output_tokens(1_024)
            .build(),
    });
    let extensions = StaticExtensionFactory::new(
        StaticExtensionRegistration::default(),
        vec![Arc::new(move || {
            let policy = policy.clone();
            Ok(Arc::new(HookModule(
                move |registrar: &mut ExtensionRegistrar| {
                    registrar.on::<ToolCallPoint, _>(policy.clone())
                },
            )) as Arc<dyn ExtensionModule>)
        })],
    );
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(PlanModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(extensions))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("plan kernel"),
    );
    let acp = Arc::new(AcpServerFactory::new(
        Arc::clone(&kernel),
        NonZeroUsize::new(128).expect("positive batch size"),
    ));
    (kernel, acp)
}

/// A plain policy that changes nothing.
fn no_policy() -> ToolPolicy {
    ToolPolicy {
        rewrite_plan: false,
        block_todos: false,
    }
}

/// What a test client receives from the agent.
struct ClientSide {
    updates: Updates,
    asked: mpsc::UnboundedReceiver<Asked>,
}

/// Runs `body` on an initialized client that hands permission requests to the test.
async fn with_client<B>(factory: &Arc<AcpServerFactory>, body: B)
where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        &mut ClientSide,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let component = factory.component(AcpTransportKind::Stdio);
    let (updates_tx, updates) = mpsc::unbounded_channel();
    let (asked_tx, asked) = mpsc::unbounded_channel();
    Client
        .v2()
        .on_receive_notification(
            async move |notification: wire::UpdateSessionNotification,
                        _connection| {
                let _ignored = updates_tx.send(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: wire::RequestPermissionRequest,
                        responder: Responder<
                wire::RequestPermissionResponse,
            >,
                        _connection| {
                // Answering here would stall dispatch, so the test body answers.
                let _ignored = asked_tx.send((request, responder));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(component, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new("plan-mode-test", "0.1.0"),
                ))
                .block_task()
                .await?;
            let mut side = ClientSide { updates, asked };
            body(&connection, &mut side).await
        })
        .await
        .expect("ACP connection");
}

/// Serializes one update for structural assertions.
fn json(update: &wire::UpdateSessionNotification) -> serde_json::Value {
    serde_json::to_value(&update.update).expect("serialize update")
}

/// Waits for the first update that satisfies `check`.
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

/// Builds a one-block text prompt request.
fn prompt(session_id: &wire::SessionId, text: &str) -> wire::PromptRequest {
    wire::PromptRequest::new(
        session_id.clone(),
        vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
    )
}

/// Asks to change the Session's mode.
fn set_mode(
    session_id: &wire::SessionId,
    value: &str,
) -> wire::SetSessionConfigOptionRequest {
    wire::SetSessionConfigOptionRequest::new(
        session_id.clone(),
        "mode",
        wire::SessionConfigOptionValue::id(value),
    )
}

/// Reports whether an update is a plan update for `plan_id`.
fn is_plan(value: &serde_json::Value, plan_id: &str) -> bool {
    value["sessionUpdate"] == "plan_update"
        && value
            .get("plan")
            .and_then(|plan| plan.get("planId"))
            .and_then(serde_json::Value::as_str)
            == Some(plan_id)
}

/// Reports whether an update touches the tool call `id`.
fn is_tool_update(value: &serde_json::Value, id: &str) -> bool {
    value["toolCallId"] == id
}

/// Waits until the Session's model reply finishes.
async fn wait_idle(updates: &mut Updates) {
    next_matching(updates, |value| {
        value["sessionUpdate"] == "state_update" && value["state"] == "idle"
    })
    .await;
}

/// Turns a wire Session id into the Kernel's.
fn kernel_session(session_id: &wire::SessionId) -> protocol::SessionId {
    protocol::SessionId::try_from(session_id.to_string()).expect("session id")
}

/// Approves the Session's pending plan directly through the Kernel.
async fn approve_through_kernel(kernel: &Kernel, session_id: &wire::SessionId) {
    let session = kernel_session(session_id);
    let request = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(request) =
                kernel.plan_approval(&session).expect("pending")
            {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pending approval");
    kernel
        .respond_plan_approval(protocol::PlanApprovalResponse {
            session_id: session,
            request_id: request.request_id,
            decision: protocol::PlanApprovalDecision::Approve,
        })
        .await
        .expect("approve");
}

/// The mode select follows model and thinking, switches the Session, and rejects bad values.
#[tokio::test]
async fn mode_config_option_switches_the_session() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        let options =
            serde_json::to_value(&created.config_options).expect("options");
        assert_eq!(options[2]["configId"], "mode");
        assert_eq!(options[2]["category"], "mode");
        assert_eq!(options[2]["currentValue"], "default");
        let values = options[2]["options"]
            .as_array()
            .expect("choices")
            .iter()
            .map(|choice| (choice["value"].clone(), choice["name"].clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                (serde_json::json!("default"), serde_json::json!("Default")),
                (serde_json::json!("plan"), serde_json::json!("Plan")),
            ]
        );

        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        next_matching(&mut side.updates, |value| {
            value["sessionUpdate"] == "config_option_update"
                && value["configOptions"].as_array().is_some_and(|options| {
                    options.iter().any(|option| {
                        option["configId"] == "mode"
                            && option["currentValue"] == "plan"
                    })
                })
        })
        .await;

        let refused = connection
            .send_request(set_mode(&created.session_id, "invalid"))
            .block_task()
            .await
            .expect_err("unknown mode");
        assert_eq!(refused.code, ErrorCode::new(-32602), "{refused:?}");
        Ok(())
    })
    .await;
}

/// `exit_plan_mode` is reported as a mode switch.
#[tokio::test]
async fn exit_plan_mode_is_a_switch_mode_call() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        let _response =
            connection.send_request(prompt(&created.session_id, "plan it"));
        let call = next_matching(&mut side.updates, |value| {
            is_tool_update(value, "call-exit") && value["kind"].is_string()
        })
        .await;
        assert_eq!(call["kind"], "switch_mode");
        approve_through_kernel(&kernel, &created.session_id).await;
        wait_idle(&mut side.updates).await;
        Ok(())
    })
    .await;
}

/// An approved plan is published as Markdown right after its tool result.
#[tokio::test]
async fn approved_plan_is_published_as_markdown() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        let _response =
            connection.send_request(prompt(&created.session_id, "plan it"));
        approve_through_kernel(&kernel, &created.session_id).await;
        let plan = next_matching(&mut side.updates, |value| {
            is_plan(value, "plan-mode")
        })
        .await;
        assert_eq!(plan["plan"]["type"], "markdown");
        assert_eq!(plan["plan"]["content"], PLAN);
        wait_idle(&mut side.updates).await;
        Ok(())
    })
    .await;
}

/// Keeping the plan publishes nothing.
#[tokio::test]
async fn kept_plan_is_not_published() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        let _response =
            connection.send_request(prompt(&created.session_id, "plan it"));
        let session = kernel_session(&created.session_id);
        let request = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(request) =
                    kernel.plan_approval(&session).expect("pending")
                {
                    return request;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pending approval");
        kernel
            .respond_plan_approval(protocol::PlanApprovalResponse {
                session_id: session,
                request_id: request.request_id,
                decision: protocol::PlanApprovalDecision::KeepPlanning,
            })
            .await
            .expect("keep planning");
        wait_idle(&mut side.updates).await;
        let later = drain(&mut side.updates).await;
        assert!(
            !later
                .iter()
                .any(|value| value["sessionUpdate"] == "plan_update")
        );
        Ok(())
    })
    .await;
}

/// A plan an extension rewrote is the plan published, live and on replay.
#[tokio::test]
async fn rewritten_plan_is_published_live_and_on_replay() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(
        root.path(),
        ToolPolicy {
            rewrite_plan: true,
            block_todos: false,
        },
    );
    let workspace = root.path().to_path_buf();
    let session_id = std::sync::Mutex::new(None);
    with_client(&acp, async |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace.clone()))
            .block_task()
            .await?;
        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        let _response =
            connection.send_request(prompt(&created.session_id, "plan it"));
        approve_through_kernel(&kernel, &created.session_id).await;
        let plan = next_matching(&mut side.updates, |value| {
            is_plan(value, "plan-mode")
        })
        .await;
        assert_eq!(plan["plan"]["content"], "REWRITTEN PLAN");
        wait_idle(&mut side.updates).await;
        *session_id.lock().expect("session") = Some(created.session_id);
        Ok(())
    })
    .await;
    let session_id = session_id
        .lock()
        .expect("session")
        .clone()
        .expect("created");
    let replayed = replay(&acp, &session_id, &workspace).await;
    let plans = replayed
        .iter()
        .filter(|value| is_plan(value, "plan-mode"))
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0]["plan"]["content"], "REWRITTEN PLAN");
}

/// Replays a Session from its start on a fresh connection and returns the updates.
async fn replay(
    acp: &Arc<AcpServerFactory>,
    session_id: &wire::SessionId,
    workspace: &Path,
) -> Vec<serde_json::Value> {
    let mut received = Vec::new();
    with_client(acp, async |connection, side| {
        connection
            .send_request(
                wire::ResumeSessionRequest::new(
                    session_id.clone(),
                    workspace.to_path_buf(),
                )
                .replay_from(wire::ReplayFrom::from(
                    wire::ReplayFromStart::new(),
                )),
            )
            .block_task()
            .await?;
        received = drain(&mut side.updates).await;
        Ok(())
    })
    .await;
    received
}

/// `todo_write` becomes the `todos` plan, with no tool card, live and on replay.
#[tokio::test]
async fn todo_write_updates_the_plan_without_a_tool_card() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    let session_id = std::sync::Mutex::new(None);
    let live = std::sync::Mutex::new(Vec::new());
    with_client(&acp, async |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace.clone()))
            .block_task()
            .await?;
        connection
            .send_request(prompt(&created.session_id, "todo"))
            .block_task()
            .await?;
        let mut values = Vec::new();
        loop {
            let value = next_matching(&mut side.updates, |_| true).await;
            let idle = value["sessionUpdate"] == "state_update"
                && value["state"] == "idle";
            values.push(value);
            if idle {
                break;
            }
        }
        *live.lock().expect("live") = values;
        *session_id.lock().expect("session") = Some(created.session_id);
        Ok(())
    })
    .await;
    let live = live.into_inner().expect("live");
    let expected = serde_json::json!({
        "sessionUpdate": "plan_update",
        "plan": {
            "type": "items",
            "planId": "todos",
            "entries": [
                { "content": "Read the code", "status": "completed", "priority": "medium" },
                { "content": "Writing the change", "status": "in_progress", "priority": "medium" },
                { "content": "Run the tests", "status": "pending", "priority": "medium" },
            ],
        },
    });
    let plans = live
        .iter()
        .filter(|value| is_plan(value, "todos"))
        .map(|value| {
            let mut value = value.clone();
            value.as_object_mut().expect("object").remove("_meta");
            value
        })
        .collect::<Vec<_>>();
    assert_eq!(plans, [expected]);
    assert!(!live.iter().any(|value| is_tool_update(value, "call-todo")));

    let session_id =
        session_id.into_inner().expect("session").expect("created");
    let replayed = replay(&acp, &session_id, &workspace).await;
    assert!(
        !replayed
            .iter()
            .any(|value| is_tool_update(value, "call-todo"))
    );
    assert_eq!(
        replayed
            .iter()
            .filter(|value| is_plan(value, "todos"))
            .count(),
        1
    );
}

/// A blocked `todo_write` publishes nothing.
#[tokio::test]
async fn blocked_todo_write_is_not_published() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(
        root.path(),
        ToolPolicy {
            rewrite_plan: false,
            block_todos: true,
        },
    );
    let workspace = root.path().to_path_buf();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        connection
            .send_request(prompt(&created.session_id, "todo"))
            .block_task()
            .await?;
        wait_idle(&mut side.updates).await;
        let later = drain(&mut side.updates).await;
        assert!(
            !later
                .iter()
                .any(|value| value["sessionUpdate"] == "plan_update")
        );
        Ok(())
    })
    .await;
}

/// A subagent's todo list stays in the subagent's own update stream.
#[tokio::test]
async fn subagent_todos_stay_in_the_child() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    let listing = workspace.clone();
    with_client(&acp, async move |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        connection
            .send_request(prompt(&created.session_id, "delegate"))
            .block_task()
            .await?;
        let mut values = Vec::new();
        loop {
            let update = tokio::time::timeout(WAIT, side.updates.recv())
                .await
                .expect("update")
                .expect("stream");
            let value = json(&update);
            let idle = update.session_id == created.session_id
                && value["sessionUpdate"] == "state_update"
                && value["state"] == "idle";
            values.push((update.session_id.clone(), value));
            if idle {
                break;
            }
        }
        // The child really ran and wrote its own todo list.
        let child = tokio::time::timeout(WAIT, async {
            loop {
                let child = kernel
                    .list_sessions(Some(&listing))
                    .expect("list")
                    .into_iter()
                    .find(|summary| summary.ownership.is_some())
                    .map(|summary| summary.session_id);
                if let Some(child) = child
                    && kernel
                        .session_transcript(&child)
                        .expect("child transcript")
                        .iter()
                        .any(|message| matches!(&message.content,
                            MessageContent::ToolResult { tool_call_id, is_error: false, .. }
                                if tool_call_id.as_str() == "call-child-todo"))
                {
                    break child;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("child wrote its todos");
        while let Ok(Some(update)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), side.updates.recv()).await
        {
            values.push((update.session_id.clone(), json(&update)));
        }
        assert_ne!(child.to_string(), created.session_id.to_string());
        assert!(
            !values
                .iter()
                .any(|(session, value)| session == &created.session_id
                    && value["sessionUpdate"] == "plan_update")
        );
        Ok(())
    })
    .await;
}

/// Two todo lists and an approved plan replay in order, and nothing is ever removed.
#[tokio::test]
async fn replay_publishes_plans_in_order() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    let session_id = std::sync::Mutex::new(None);
    with_client(&acp, async |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace.clone()))
            .block_task()
            .await?;
        for text in ["todo", "todo again"] {
            connection
                .send_request(prompt(&created.session_id, text))
                .block_task()
                .await?;
        }
        connection
            .send_request(set_mode(&created.session_id, "plan"))
            .block_task()
            .await?;
        let _response =
            connection.send_request(prompt(&created.session_id, "plan it"));
        approve_through_kernel(&kernel, &created.session_id).await;
        next_matching(&mut side.updates, |value| is_plan(value, "plan-mode"))
            .await;
        wait_idle(&mut side.updates).await;
        *session_id.lock().expect("session") = Some(created.session_id);
        Ok(())
    })
    .await;
    let session_id =
        session_id.into_inner().expect("session").expect("created");
    let replayed = replay(&acp, &session_id, &workspace).await;
    let order = replayed
        .iter()
        .filter(|value| value["sessionUpdate"] == "plan_update")
        .map(|value| {
            (
                value["plan"]["planId"].clone(),
                value["plan"]["entries"][0]["content"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            (
                serde_json::json!("todos"),
                serde_json::json!("Read the code")
            ),
            (
                serde_json::json!("todos"),
                serde_json::json!("Read the tests")
            ),
            (serde_json::json!("plan-mode"), serde_json::Value::Null),
        ]
    );
    assert!(
        !replayed
            .iter()
            .any(|value| value["sessionUpdate"] == "plan_removed")
    );
}

/// Waits for the next permission request the agent sent.
async fn next_asked(side: &mut ClientSide) -> Asked {
    tokio::time::timeout(WAIT, side.asked.recv())
        .await
        .expect("permission request")
        .expect("request stream")
}

/// Builds a client answer that selects one option.
fn selected(option: &str) -> wire::RequestPermissionResponse {
    wire::RequestPermissionResponse::new(
        wire::RequestPermissionOutcome::Selected(
            wire::SelectedPermissionOutcome::new(option),
        ),
    )
}

/// Starts plan mode and submits a plan, returning the Session.
async fn start_plan(
    connection: &V2ConnectionTo<Agent>,
    workspace: &Path,
) -> Result<wire::SessionId, agent_client_protocol::Error> {
    let created = connection
        .send_request(wire::NewSessionRequest::new(workspace.to_path_buf()))
        .block_task()
        .await?;
    connection
        .send_request(set_mode(&created.session_id, "plan"))
        .block_task()
        .await?;
    let _response =
        connection.send_request(prompt(&created.session_id, "plan it"));
    Ok(created.session_id)
}

/// Returns the Session's current mode.
fn mode_of(
    kernel: &Kernel,
    session_id: &wire::SessionId,
) -> protocol::SessionMode {
    kernel
        .session_settings(&kernel_session(session_id))
        .expect("settings")
        .mode
}

/// The approval is a standard permission request about the `switch_mode` call.
#[tokio::test]
async fn plan_approval_uses_request_permission() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async |connection, side| {
        let session_id = start_plan(connection, &workspace).await?;
        let (request, responder) = next_asked(side).await;
        let request = serde_json::to_value(&request).expect("request");
        assert_eq!(request["title"], "Ready to execute the plan?");
        assert_eq!(request["subject"]["type"], "tool_call");
        assert_eq!(request["subject"]["toolCall"]["toolCallId"], "call-exit");
        assert_eq!(request["subject"]["toolCall"]["kind"], "switch_mode");
        assert!(
            request["subject"]["toolCall"]["content"]
                .to_string()
                .contains("Read the code")
        );
        let options = request["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| (option["optionId"].clone(), option["kind"].clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            options,
            [
                (
                    serde_json::json!("approve"),
                    serde_json::json!("allow_once")
                ),
                (
                    serde_json::json!("keep_planning"),
                    serde_json::json!("reject_once")
                ),
            ]
        );
        responder.respond(selected("approve"))?;
        wait_idle(&mut side.updates).await;
        assert_eq!(
            mode_of(&kernel, &session_id),
            protocol::SessionMode::Default
        );
        Ok(())
    })
    .await;
}

/// Dismissing the prompt or answering with an unknown option keeps planning.
#[tokio::test]
async fn cancelled_or_unknown_option_keeps_planning() {
    for answer in [
        wire::RequestPermissionResponse::new(
            wire::RequestPermissionOutcome::Cancelled(
                wire::CancelledPermissionOutcome::new(),
            ),
        ),
        selected("something_else"),
    ] {
        let root = tempfile::tempdir().expect("root");
        let (kernel, acp) = server(root.path(), no_policy());
        let workspace = root.path().to_path_buf();
        with_client(&acp, async |connection, side| {
            let session_id = start_plan(connection, &workspace).await?;
            let (_request, responder) = next_asked(side).await;
            responder.respond(answer)?;
            wait_idle(&mut side.updates).await;
            assert_eq!(
                mode_of(&kernel, &session_id),
                protocol::SessionMode::Plan
            );
            Ok(())
        })
        .await;
    }
}

/// With two clients on one Session, the first answer wins and the other copy is withdrawn.
#[tokio::test]
async fn first_client_wins_and_others_are_withdrawn() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    with_client(&acp, async |connection, side| {
        let session_id = start_plan(connection, &workspace).await?;
        let (_first, first_responder) = next_asked(side).await;
        with_client(&acp, async |other, other_side| {
            other
                .send_request(wire::ResumeSessionRequest::new(
                    session_id.clone(),
                    workspace.clone(),
                ))
                .block_task()
                .await?;
            let (_second, second_responder) = next_asked(other_side).await;
            first_responder.respond(selected("keep_planning"))?;
            tokio::time::timeout(
                WAIT,
                second_responder.cancellation().cancelled(),
            )
            .await
            .expect("other copy withdrawn");
            Ok(())
        })
        .await;
        wait_idle(&mut side.updates).await;
        assert_eq!(mode_of(&kernel, &session_id), protocol::SessionMode::Plan);
        Ok(())
    })
    .await;
}

/// A plan still waiting is asked again after a resume, which reports requires_action.
#[tokio::test]
async fn pending_approval_is_reestablished_after_resume() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(root.path(), no_policy());
    let workspace = root.path().to_path_buf();
    let session_id = std::sync::Mutex::new(None);
    with_client(&acp, async |connection, side| {
        let created = start_plan(connection, &workspace).await?;
        // Received but never answered before this connection closes.
        let _unanswered = next_asked(side).await;
        *session_id.lock().expect("session") = Some(created);
        Ok(())
    })
    .await;
    let session_id =
        session_id.into_inner().expect("session").expect("created");
    with_client(&acp, async |connection, side| {
        connection
            .send_request(
                wire::ResumeSessionRequest::new(
                    session_id.clone(),
                    workspace.clone(),
                )
                .replay_from(wire::ReplayFrom::from(
                    wire::ReplayFromStart::new(),
                )),
            )
            .block_task()
            .await?;
        let (_request, responder) = next_asked(side).await;
        let resumed = drain(&mut side.updates).await;
        assert!(
            resumed
                .iter()
                .any(|value| value["sessionUpdate"] == "state_update"
                    && value["state"] == "requires_action"),
            "{resumed:?}"
        );
        responder.respond(selected("approve"))?;
        wait_idle(&mut side.updates).await;
        Ok(())
    })
    .await;
    assert_eq!(
        mode_of(&kernel, &session_id),
        protocol::SessionMode::Default
    );
}

/// A `todo_write` that fails or is blocked shows no tool card, live or on replay.
#[tokio::test]
async fn failed_todo_write_shows_no_tool_card() {
    for (policy, text, call) in [
        (
            ToolPolicy {
                rewrite_plan: false,
                block_todos: true,
            },
            "todo",
            "call-todo",
        ),
        (no_policy(), "todo bad", "call-todo-bad"),
    ] {
        let root = tempfile::tempdir().expect("root");
        let (_kernel, acp) = server(root.path(), policy);
        let workspace = root.path().to_path_buf();
        let session_id = std::sync::Mutex::new(None);
        let live = std::sync::Mutex::new(Vec::new());
        with_client(&acp, async |connection, side| {
            let created = connection
                .send_request(wire::NewSessionRequest::new(workspace.clone()))
                .block_task()
                .await?;
            connection
                .send_request(prompt(&created.session_id, text))
                .block_task()
                .await?;
            wait_idle(&mut side.updates).await;
            let mut values = drain(&mut side.updates).await;
            values.extend(live.lock().expect("live").drain(..));
            *live.lock().expect("live") = values;
            *session_id.lock().expect("session") = Some(created.session_id);
            Ok(())
        })
        .await;
        let session_id =
            session_id.into_inner().expect("session").expect("created");
        let replayed = replay(&acp, &session_id, &workspace).await;
        let live = live.into_inner().expect("live");
        assert!(
            !live.iter().any(|value| is_tool_update(value, call)),
            "{text}"
        );
        assert!(
            !replayed.iter().any(|value| is_tool_update(value, call)),
            "{text}"
        );
    }
}

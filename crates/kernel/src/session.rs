//! Session lifecycle: channel-backed handles, background task, and event stream.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use config::AppConfig;
use futures::Stream;
use protocol::{AgentPath, Event, KernelError, Op, ReviewDecision, SessionId, StopReason};
use provider::factory::ArcLlm;
use skills::SkillRegistry;
use tokio::sync::{mpsc, oneshot, watch};

use crate::agent::control::AgentControl;
use crate::agent::mailbox::{Mailbox, MailboxReceiver, mailbox_pair};
use crate::context::ContextManager;
use crate::turn::{TurnContext, execute_turn};
use store::{
    PersistedPayload, SessionRecorder, TurnAbortedRecord, TurnCompleteRecord, TurnKindRecord,
};
use tools::ToolRegistry;

/// Frontend handle for a live session.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct Thread {
    /// Session identifier owned by this handle.
    pub session_id: SessionId,
    /// Working directory associated with this live session.
    pub cwd: PathBuf,
    /// Send operations to the background task.
    pub(crate) tx_op: mpsc::UnboundedSender<Op>,
    /// Shared sender for per-turn events.
    pub(crate) tx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<Event>>>,
    /// Shared pending approval channels. The handle stores the primary copy
    /// so callers outside the background task (e.g. ACP agent) can resolve
    /// approval requests without blocking on the session loop.
    pub(crate) pending_approvals:
        Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>>,
    /// Signal cancellation.
    pub(crate) cancel_tx: watch::Sender<bool>,
    /// AgentControl for multi-agent operations.
    #[allow(dead_code)]
    pub(crate) agent_control: Option<Arc<AgentControl>>,
    /// Mailbox for receiving inter-agent messages.
    #[allow(dead_code)]
    pub(crate) mailbox: Mailbox,
    /// Optional file-backed recorder for canonical session history.
    #[builder(default, setter(strip_option))]
    pub(crate) recorder: Option<Arc<dyn SessionRecorder>>,
}

impl Thread {
    /// Create a new event receiver for this prompt and wire it up.
    pub(crate) async fn take_rx(&self) -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut guard = self.tx_event.lock().await;
        *guard = tx;
        rx
    }
}

/// Runtime state owned by the background task of a single session.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct Session {
    /// Session identifier handled by this runtime task.
    pub session_id: SessionId,
    /// Working directory used for tool execution.
    pub cwd: PathBuf,
    /// Operation receiver consumed by the runtime task.
    pub rx_op: mpsc::UnboundedReceiver<Op>,
    /// Current event sender used by prompt streams.
    pub tx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<Event>>>,
    #[allow(dead_code)]
    /// Cancellation signal receiver for the active stream.
    pub cancel_rx: watch::Receiver<bool>,
    /// Conversation context owned by this session.
    pub context: Box<dyn ContextManager>,
    /// LLM used for turn execution.
    pub llm: ArcLlm,
    /// Tool registry available to this session.
    pub tools: Arc<ToolRegistry>,
    /// Shared map of pending approval channels. execute_turn inserts a
    /// oneshot::Sender keyed by call_id; run_loop sends the user's decision.
    pub pending_approvals:
        Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>>,
    /// Agent path for this session.
    pub agent_path: AgentPath,
    /// Approval policy — controls tool confirmation behaviour.
    pub approval: Arc<crate::approval::ApprovalPolicy>,
    /// Mailbox receiver for inter-agent messages.
    #[allow(dead_code)]
    pub mailbox_rx: MailboxReceiver,
    /// AgentControl shared across session tree.
    #[builder(default)]
    #[allow(dead_code)]
    pub agent_control: Option<Arc<AgentControl>>,
    /// Application configuration.
    #[builder(default)]
    pub app_config: Arc<AppConfig>,
    /// Skill registry for this session's working directory.
    #[builder(default)]
    pub skill_registry: Arc<SkillRegistry>,
    /// MCP connection manager for this session.
    /// Held to keep server connections alive — tool dispatch goes through ToolRegistry.
    #[allow(dead_code)]
    pub mcp_manager: Arc<mcp::McpConnectionManager>,
    /// Optional file-backed recorder for canonical session history.
    #[builder(default, setter(strip_option))]
    pub recorder: Option<Arc<dyn SessionRecorder>>,
}

/// Spawn the background task for a session and return the frontend handle.
///
/// Creates all channel pairs (ops, events, approval, cancel, mailbox) and
/// wires them into the [`Session`] and [`Thread`] halves. If `agent_control`
/// is provided, the session participates in multi-agent routing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_thread(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
    agent_path: AgentPath,
    agent_control: Option<Arc<AgentControl>>,
    approval: Arc<crate::approval::ApprovalPolicy>,
    app_config: Arc<AppConfig>,
    recorder: Option<Arc<dyn SessionRecorder>>,
) -> Thread {
    let (tx_op, rx_op) = mpsc::unbounded_channel();
    let (initial_tx, _initial_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (mailbox, mailbox_rx) = mailbox_pair();

    let skill_registry = SkillRegistry::discover(&cwd, &app_config.skills);
    tools.register_skill_tools(Arc::clone(&skill_registry));

    let mcp_manager = {
        let configs: Vec<mcp::McpServerConfig> = app_config
            .mcp_servers
            .iter()
            .filter(|c| c.enabled)
            .cloned()
            .filter_map(|config| match config.try_into() {
                Ok(config) => Some(config),
                Err(error) => {
                    // Config loading validates MCP entries, so this only guards test-built configs.
                    tracing::warn!(%error, "skipping invalid MCP server config");
                    None
                }
            })
            .collect();

        let manager = Arc::new(mcp::McpConnectionManager::new(
            configs,
            mcp::default_auth_dir(),
        ));
        let rx = manager.spawn_background();

        // Register MCP tools once all servers finish connecting.
        let mgr = Arc::clone(&manager);
        let tools_ref = Arc::clone(&tools);
        tokio::spawn(async move {
            let _ = rx.await;
            tools_ref.register_mcp_tools(mgr);
        });

        manager
    };

    let tx_event = Arc::new(tokio::sync::Mutex::new(initial_tx));
    let pending_approvals: Arc<
        tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    let thread_cwd = cwd.clone();
    let mut runtime = Session::builder()
        .session_id(session_id.clone())
        .cwd(cwd)
        .rx_op(rx_op)
        .tx_event(Arc::clone(&tx_event))
        .cancel_rx(cancel_rx)
        .context(context)
        .llm(llm)
        .tools(tools)
        .pending_approvals(Arc::clone(&pending_approvals))
        .agent_path(agent_path)
        .approval(approval)
        .mailbox_rx(mailbox_rx)
        .agent_control(agent_control.as_ref().map(Arc::clone))
        .app_config(app_config)
        .skill_registry(skill_registry)
        .mcp_manager(mcp_manager)
        .build();
    runtime.recorder = recorder.clone();

    tokio::spawn(run_loop(runtime));

    let mut thread = Thread::builder()
        .session_id(session_id)
        .cwd(thread_cwd)
        .tx_op(tx_op)
        .tx_event(tx_event)
        .pending_approvals(pending_approvals)
        .cancel_tx(cancel_tx)
        .agent_control(agent_control)
        .mailbox(mailbox)
        .build();
    thread.recorder = recorder;
    thread
}

/// Background task: receive ops, execute turns, emit events.
async fn run_loop(mut rt: Session) {
    loop {
        let op = rt.rx_op.recv().await;
        match op {
            // Inter-agent messages are processed identically to user prompts.
            // The content is injected as the turn input, and the turn loop
            // handles tool calls, approvals, and cancellation the same way.
            Some(Op::InterAgentMessage { content, .. }) => {
                let turn_id = uuid::Uuid::new_v4().to_string();
                let ctx = TurnContext::builder()
                    .session_id(rt.session_id.clone())
                    .turn_id(turn_id.clone())
                    .turn_kind(TurnKindRecord::InterAgentMessage)
                    .llm(Arc::clone(&rt.llm))
                    .tools(Arc::clone(&rt.tools))
                    .cwd(rt.cwd.clone())
                    .provider_id(active_provider_id(&rt.app_config))
                    .pending_approvals(Arc::clone(&rt.pending_approvals))
                    .agent_path(rt.agent_path.clone())
                    .approval(Arc::clone(&rt.approval))
                    .app_config(Arc::clone(&rt.app_config))
                    .skill_registry(Arc::clone(&rt.skill_registry))
                    .build();
                let ctx = with_recorder(ctx, rt.recorder.clone());

                let tx = { rt.tx_event.lock().await.clone() };
                let turn = execute_turn(&ctx, content, &mut rt.context, &tx);
                tokio::pin!(turn);

                loop {
                    tokio::select! {
                        result = &mut turn => {
                            if let Err(e) = result {
                                persist_turn_aborted(&rt.recorder, &turn_id, e.to_string()).await;
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::Error,
                                ));
                                tracing::error!(
                                    session_id = %rt.session_id,
                                    error = %e,
                                    "Turn execution failed"
                                );
                            } else {
                                persist_turn_complete(&rt.recorder, &turn_id, StopReason::EndTurn).await;
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::EndTurn,
                                ));
                            }
                            break;
                        }
                        op = rt.rx_op.recv() => match op {
                            Some(Op::ExecApprovalResponse { call_id, decision })
                            | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                                if let Some(tx) =
                                    rt.pending_approvals.lock().await.remove(&call_id)
                                {
                                    let _ = tx.send(decision);
                                }
                            }
                            Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => {
                                return;
                            }
                            Some(other) => {
                                tracing::debug!(?other, "Ignoring operation while turn is running");
                            }
                        }
                    }
                }
            }
            Some(Op::Prompt { text, system, .. }) => {
                let turn_id = uuid::Uuid::new_v4().to_string();
                let ctx = TurnContext::builder()
                    .session_id(rt.session_id.clone())
                    .turn_id(turn_id.clone())
                    .turn_kind(TurnKindRecord::Prompt)
                    .llm(Arc::clone(&rt.llm))
                    .tools(Arc::clone(&rt.tools))
                    .cwd(rt.cwd.clone())
                    .provider_id(active_provider_id(&rt.app_config))
                    .pending_approvals(Arc::clone(&rt.pending_approvals))
                    .agent_path(rt.agent_path.clone())
                    .approval(Arc::clone(&rt.approval))
                    .user_system_prompt(system)
                    .app_config(Arc::clone(&rt.app_config))
                    .skill_registry(Arc::clone(&rt.skill_registry))
                    .build();
                let ctx = with_recorder(ctx, rt.recorder.clone());

                let tx = { rt.tx_event.lock().await.clone() };

                let turn = execute_turn(&ctx, text, &mut rt.context, &tx);
                tokio::pin!(turn);

                loop {
                    tokio::select! {
                        result = &mut turn => {
                            if let Err(e) = result {
                                persist_turn_aborted(&rt.recorder, &turn_id, e.to_string()).await;
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::Error,
                                ));
                                tracing::error!(
                                    session_id = %rt.session_id,
                                    error = %e,
                                    "Turn execution failed"
                                );
                            } else {
                                persist_turn_complete(&rt.recorder, &turn_id, StopReason::EndTurn).await;
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::EndTurn,
                                ));
                            }
                            break;
                        }
                        op = rt.rx_op.recv() => match op {
                            Some(Op::ExecApprovalResponse { call_id, decision })
                            | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                                if let Some(tx) =
                                    rt.pending_approvals.lock().await.remove(&call_id)
                                {
                                    let _ = tx.send(decision);
                                }
                            }
                            Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => {
                                return;
                            }
                            Some(other) => {
                                tracing::debug!(?other, "Ignoring operation while turn is running");
                            }
                        }
                    }
                }
            }
            Some(Op::ExecApprovalResponse { call_id, decision })
            | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                if let Some(tx) = rt.pending_approvals.lock().await.remove(&call_id) {
                    let _ = tx.send(decision);
                }
            }
            Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => break,
            _ => {}
        }
    }
}

/// Attach an optional recorder to a turn context after typed-builder construction.
fn with_recorder(mut ctx: TurnContext, recorder: Option<Arc<dyn SessionRecorder>>) -> TurnContext {
    ctx.recorder = recorder;
    ctx
}

/// Return the provider id portion of the configured active model.
fn active_provider_id(app_config: &AppConfig) -> String {
    app_config
        .active_model
        .split_once('/')
        .map(|(provider_id, _)| provider_id.to_string())
        .unwrap_or_default()
}

/// Persist a successful turn completion marker, logging but not failing the live turn.
async fn persist_turn_complete(
    recorder: &Option<Arc<dyn SessionRecorder>>,
    turn_id: &str,
    stop_reason: StopReason,
) {
    let Some(recorder) = recorder else {
        return;
    };
    let record = TurnCompleteRecord::builder()
        .turn_id(turn_id.to_string())
        .stop_reason(stop_reason)
        .build();
    if let Err(error) = recorder
        .append(&[PersistedPayload::TurnComplete(record)])
        .await
    {
        tracing::warn!(%error, "failed to persist turn completion");
    }
}

/// Persist an interrupted turn marker, logging but not failing shutdown/error handling.
async fn persist_turn_aborted(
    recorder: &Option<Arc<dyn SessionRecorder>>,
    turn_id: &str,
    reason: String,
) {
    let Some(recorder) = recorder else {
        return;
    };
    let record = TurnAbortedRecord::builder()
        .turn_id(turn_id.to_string())
        .reason(reason)
        .build();
    if let Err(error) = recorder
        .append(&[PersistedPayload::TurnAborted(record)])
        .await
    {
        tracing::warn!(%error, "failed to persist turn abort");
    }
}

/// Build an [`EventStream`] from the session's event receiver and cancel watch.
///
/// The stream terminates when `TurnComplete` arrives or cancellation is signaled.
pub(crate) fn event_stream(
    mut rx_event: mpsc::UnboundedReceiver<Event>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>> {
    Box::pin(async_stream::stream! {
        loop {
            tokio::select! {
                event = rx_event.recv() => {
                    match event {
                        Some(e @ Event::TurnComplete { .. }) => {
                            yield Ok(e);
                            break;
                        }
                        Some(e) => yield Ok(e),
                        None => break,
                    }
                }
                _ = cancel_rx.changed() => {
                    yield Err(KernelError::Cancelled);
                    break;
                }
            }
        }
    })
}

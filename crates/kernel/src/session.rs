//! Session lifecycle: channel-backed handles, background task, and event stream.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use config::AppConfig;
use futures::Stream;
use protocol::message::Message;
use protocol::{
    AgentPath, AgentStatus, Event, InterAgentMessage, KernelError, Op, ReviewDecision, SessionId,
    StopReason, TurnId, Usage,
};
use provider::factory::{ArcLlm, LlmFactory};
use skills::SkillRegistry;
use tokio::sync::{mpsc, oneshot, watch};

use crate::agent::control::AgentControl;
use crate::context::ContextManager;
use crate::input_queue::InputQueue;
use crate::turn::{TurnContext, execute_turn};
use store::{
    MessageRecord, PersistedPayload, SessionRecorder, TurnAbortedRecord, TurnCompleteRecord,
    TurnKindRecord,
};
use tools::ToolRegistry;

/// Frontend handle for a live session.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct Thread {
    /// Session identifier owned by this handle.
    pub session_id: SessionId,
    /// Working directory associated with this live session.
    pub cwd: PathBuf,
    /// Agent path associated with this live session.
    pub agent_path: AgentPath,
    /// Current model id in `provider_id/model_id` form for frontend state.
    pub(crate) current_model: Arc<tokio::sync::RwLock<String>>,
    /// Accumulated provider-reported usage for this live session.
    pub(crate) current_usage: Arc<tokio::sync::RwLock<Usage>>,
    /// Send operations to the background task.
    pub(crate) tx_op: mpsc::UnboundedSender<Op>,
    /// Shared sender for per-turn events.
    pub(crate) tx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<Event>>>,
    /// Initial receiver kept alive until the first frontend subscription.
    pub(crate) initial_rx_event: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<Event>>>>,
    /// Shared pending approval channels. The handle stores the primary copy
    /// so callers outside the background task (e.g. ACP agent) can resolve
    /// approval requests without blocking on the session loop.
    pub(crate) pending_approvals:
        Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>>,
    /// Signal cancellation.
    pub(crate) cancel_tx: watch::Sender<bool>,
    /// Tool registry available to this live session.
    pub(crate) tools: Arc<ToolRegistry>,
    /// MCP connection manager for this live session.
    pub(crate) mcp_manager: Arc<mcp::McpConnectionManager>,
    /// Recorder for canonical session history.
    pub(crate) recorder: Arc<dyn SessionRecorder>,
    /// Session-scoped queue used to deliver inter-agent mailbox messages.
    pub(crate) input_queue: Arc<tokio::sync::Mutex<InputQueue>>,
}

impl Thread {
    /// Create a new event receiver for this prompt and wire it up.
    pub(crate) async fn take_rx(&self) -> mpsc::UnboundedReceiver<Event> {
        // The first subscriber must receive events emitted before the frontend
        // attached, which happens for sub-agent turns started internally.
        if let Some(rx) = self.initial_rx_event.lock().await.take() {
            return rx;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let mut guard = self.tx_event.lock().await;
        *guard = tx;
        rx
    }

    /// Return the current runtime model id for this live session.
    pub(crate) async fn current_model(&self) -> String {
        self.current_model.read().await.clone()
    }

    /// Return the accumulated runtime usage for this live session.
    pub(crate) async fn current_usage(&self) -> Usage {
        *self.current_usage.read().await
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
    /// Conversation context owned by this session.
    pub context: Box<dyn ContextManager>,
    /// LLM used for turn execution.
    pub llm: ArcLlm,
    /// Agent-specific system prompt selected by the thread role.
    #[builder(default, setter(strip_option))]
    pub agent_prompt: Option<String>,
    /// Current model id in `provider_id/model_id` form for session-state responses.
    pub current_model: Arc<tokio::sync::RwLock<String>>,
    /// Accumulated provider-reported usage for this live session.
    pub usage: Arc<tokio::sync::RwLock<Usage>>,
    /// Factory used by internal model-switch operations.
    pub llm_factory: Arc<LlmFactory>,
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
    /// AgentControl shared across session tree.
    pub agent_control: Arc<AgentControl>,
    /// Application configuration.
    #[builder(default)]
    pub app_config: Arc<AppConfig>,
    /// Skill registry for this session's working directory.
    #[builder(default)]
    pub skill_registry: Arc<SkillRegistry>,
    /// Recorder for canonical session history.
    pub recorder: Arc<dyn SessionRecorder>,
    /// Session-scoped queue for model-visible inter-agent mailbox delivery.
    #[builder(default = Arc::new(tokio::sync::Mutex::new(InputQueue::default())))]
    pub input_queue: Arc<tokio::sync::Mutex<InputQueue>>,
}

/// Spawn the background task for a session and return the frontend handle.
///
/// Creates all channel pairs (ops, events, approval, cancel) and
/// wires them into the [`Session`] and [`Thread`] halves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_thread(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    agent_prompt: Option<String>,
    llm_factory: Arc<LlmFactory>,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
    agent_path: AgentPath,
    agent_control: Arc<AgentControl>,
    approval: Arc<crate::approval::ApprovalPolicy>,
    app_config: Arc<AppConfig>,
    recorder: Arc<dyn SessionRecorder>,
    initial_usage: Usage,
) -> Thread {
    let (tx_op, rx_op) = mpsc::unbounded_channel();
    let (initial_tx, initial_rx) = mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = watch::channel(false);
    let current_model = Arc::new(tokio::sync::RwLock::new(format!(
        "{}/{}",
        llm.provider_id(),
        llm.model_id()
    )));
    let current_usage = Arc::new(tokio::sync::RwLock::new(initial_usage));

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
    let initial_rx_event = Arc::new(tokio::sync::Mutex::new(Some(initial_rx)));
    let pending_approvals: Arc<
        tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let input_queue = Arc::new(tokio::sync::Mutex::new(InputQueue::default()));

    let thread_cwd = cwd.clone();
    let mut runtime = Session::builder()
        .session_id(session_id.clone())
        .cwd(cwd)
        .rx_op(rx_op)
        .tx_event(Arc::clone(&tx_event))
        .context(context)
        .llm(llm)
        .current_model(Arc::clone(&current_model))
        .usage(Arc::clone(&current_usage))
        .llm_factory(llm_factory)
        .tools(Arc::clone(&tools))
        .pending_approvals(Arc::clone(&pending_approvals))
        .agent_path(agent_path.clone())
        .approval(approval)
        .agent_control(Arc::clone(&agent_control))
        .app_config(app_config)
        .skill_registry(skill_registry)
        .recorder(Arc::clone(&recorder))
        .input_queue(Arc::clone(&input_queue))
        .build();
    runtime.agent_prompt = agent_prompt;

    tokio::spawn(run_loop(runtime));

    Thread::builder()
        .session_id(session_id)
        .cwd(thread_cwd)
        .agent_path(agent_path)
        .current_model(current_model)
        .current_usage(current_usage)
        .tx_op(tx_op)
        .tx_event(tx_event)
        .initial_rx_event(initial_rx_event)
        .pending_approvals(pending_approvals)
        .cancel_tx(cancel_tx)
        .tools(tools)
        .mcp_manager(mcp_manager)
        .recorder(recorder)
        .input_queue(input_queue)
        .build()
}

/// Background task: receive ops, execute turns, emit events.
async fn run_loop(mut rt: Session) {
    loop {
        let op = rt.rx_op.recv().await;
        match op {
            // Queue mailbox-only messages until a turn boundary can deliver them
            // as model-visible context without starting a new turn.
            Some(Op::InterAgentMessage { message }) if !message.trigger_turn => {
                rt.input_queue
                    .lock()
                    .await
                    .enqueue_mailbox_communication(message);
            }
            Some(Op::InterAgentMessage { message }) => {
                let mut next_message = Some(message);
                while let Some(message) = next_message {
                    let keep_running = run_inter_agent_turn(&mut rt, message).await;
                    if !keep_running {
                        return;
                    }
                    next_message = rt.input_queue.lock().await.take_next_triggering_message();
                }
                drain_pending_inter_agent_messages(
                    &mut *rt.context,
                    Arc::clone(&rt.recorder),
                    Arc::clone(&rt.input_queue),
                )
                .await;
            }
            Some(Op::Prompt { text, system, .. }) => {
                let turn_id = TurnId(uuid::Uuid::new_v4().to_string());
                let ctx = TurnContext::from_session(&rt, turn_id, TurnKindRecord::Prompt, system);
                let tx = { rt.tx_event.lock().await.clone() };

                drain_pending_inter_agent_messages(
                    &mut *rt.context,
                    Arc::clone(&rt.recorder),
                    Arc::clone(&rt.input_queue),
                )
                .await;

                match run_turn_select_loop(&mut rt, &ctx, text, &tx).await {
                    TurnStepOutcome::Shutdown => return,
                    TurnStepOutcome::Finished(status) => {
                        let status = with_final_message(status, &*rt.context);
                        notify_terminal_turn(&rt.agent_control, &rt.session_id, status).await;
                    }
                }

                let mut next_message = rt.input_queue.lock().await.take_next_triggering_message();
                while let Some(message) = next_message {
                    let keep_running = run_inter_agent_turn(&mut rt, message).await;
                    if !keep_running {
                        return;
                    }
                    next_message = rt.input_queue.lock().await.take_next_triggering_message();
                }
                drain_pending_inter_agent_messages(
                    &mut *rt.context,
                    Arc::clone(&rt.recorder),
                    Arc::clone(&rt.input_queue),
                )
                .await;
            }
            Some(Op::ExecApprovalResponse { call_id, decision })
            | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                if let Some(tx) = rt.pending_approvals.lock().await.remove(&call_id) {
                    let _ = tx.send(decision);
                }
            }
            Some(Op::SetModel {
                provider_id,
                model_id,
                ..
            }) => match rt.llm_factory.get(&provider_id, &model_id) {
                Some(llm) => {
                    rt.llm = llm;
                    *rt.current_model.write().await = format!("{provider_id}/{model_id}");
                }
                None => {
                    tracing::warn!(
                        provider_id = %provider_id,
                        model_id = %model_id,
                        "failed to apply protocol model switch operation"
                    );
                }
            },
            Some(Op::Cancel { .. } | Op::CloseSession { .. }) | None => break,
            _ => {}
        }
    }
}

/// Outcome of the shared turn execution select loop.
enum TurnStepOutcome {
    /// Turn finished executing (success or error).
    Finished(AgentStatus),
    /// Session should shut down (cancel/close received).
    Shutdown,
}

/// Run the `execute_turn` select loop: waits for either the turn to complete
/// or an operation on the session channel. Handles approval responses and
/// inter-agent message queuing inline. Returns the turn outcome.
async fn run_turn_select_loop(
    rt: &mut Session,
    ctx: &TurnContext,
    text: String,
    tx: &mpsc::UnboundedSender<Event>,
) -> TurnStepOutcome {
    let turn = execute_turn(ctx, text, &mut rt.context, tx);
    tokio::pin!(turn);
    loop {
        tokio::select! {
            result = &mut turn => {
                return match result {
                    Err(e) => {
                        let reason = e.to_string();

                        // Persist the turn aborted record before sending the event.
                        persist_turn_aborted(rt.recorder.as_ref(), &ctx.turn_id, reason.clone())
                            .await;

                        // Send the turn complete event after the turn finishes.
                        let _ = tx.send(Event::turn_complete(
                            rt.session_id.clone(),
                            StopReason::Error,
                        ));

                        tracing::error!(
                            session_id = %rt.session_id,
                            error = %e,
                            "Turn execution failed"
                        );
                        TurnStepOutcome::Finished(AgentStatus::Errored { reason })
                    }
                    Ok(usage) => {
                        // Store the turn total after execute_turn finishes; live usage events
                        // are emitted as provider-level increments while the turn is running.
                        *rt.usage.write().await += usage;

                        // Persist the turn complete record after the turn finishes.
                        persist_turn_complete(
                            rt.recorder.as_ref(),
                            &ctx.turn_id,
                            StopReason::EndTurn,
                        )
                        .await;

                        // Send the turn complete event after the turn finishes.
                        let _ = tx.send(Event::turn_complete(
                            rt.session_id.clone(),
                            StopReason::EndTurn,
                        ));

                        TurnStepOutcome::Finished(AgentStatus::Completed { message: None })
                    }
                };
            }
            op = rt.rx_op.recv() => match op {
                Some(Op::ExecApprovalResponse { call_id, decision })
                | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                    if let Some(sender) =
                        rt.pending_approvals.lock().await.remove(&call_id)
                    {
                        let _ = sender.send(decision);
                    }
                }
                Some(Op::SetModel { .. }) => {
                    tracing::warn!("Ignoring model switch while a turn is running");
                }
                Some(Op::Cancel { .. } | Op::CloseSession { .. }) | None => {
                    return TurnStepOutcome::Shutdown;
                }
                Some(Op::InterAgentMessage { message }) => {
                    rt.input_queue
                        .lock()
                        .await
                        .enqueue_mailbox_communication(message);
                }
                Some(_other) => {
                    tracing::debug!("Ignoring operation while turn is running");
                }
            }
        }
    }
}

/// Execute one inter-agent turn and return whether the session should keep running.
async fn run_inter_agent_turn(rt: &mut Session, message: InterAgentMessage) -> bool {
    let turn_id = TurnId(uuid::Uuid::new_v4().to_string());
    let ctx = TurnContext::from_session(rt, turn_id, TurnKindRecord::InterAgentMessage, None);
    let tx = { rt.tx_event.lock().await.clone() };
    drain_pending_inter_agent_messages(
        &mut *rt.context,
        Arc::clone(&rt.recorder),
        Arc::clone(&rt.input_queue),
    )
    .await;

    match run_turn_select_loop(rt, &ctx, message.render_turn_input(), &tx).await {
        TurnStepOutcome::Shutdown => false,
        TurnStepOutcome::Finished(status) => {
            let status = with_final_message(status, &*rt.context);
            notify_terminal_turn(&rt.agent_control, &rt.session_id, status).await;
            true
        }
    }
}

/// Persist and inject all pending inter-agent messages into context.
async fn drain_pending_inter_agent_messages(
    context: &mut dyn ContextManager,
    recorder: Arc<dyn SessionRecorder>,
    input_queue: Arc<tokio::sync::Mutex<InputQueue>>,
) {
    let messages = input_queue.lock().await.drain_mailbox_input_items();
    for message in messages {
        persist_inter_agent_message(context, recorder.as_ref(), message).await;
    }
}

/// Persist and inject one inter-agent message into context without starting a turn.
async fn persist_inter_agent_message(
    context: &mut dyn ContextManager,
    recorder: &dyn SessionRecorder,
    message: InterAgentMessage,
) {
    let rendered = message.render_model_context();
    let user_message = Message::user(rendered);
    context.push(user_message.clone());
    persist_message(
        recorder,
        &TurnId(uuid::Uuid::new_v4().to_string()),
        user_message,
    )
    .await;
}

/// Persist a message accepted into the session context.
async fn persist_message(recorder: &dyn SessionRecorder, turn_id: &TurnId, message: Message) {
    let record = MessageRecord::builder()
        .turn_id(String::from(turn_id))
        .message(message)
        .build();
    if let Err(error) = recorder.append(&[PersistedPayload::Message(record)]).await {
        tracing::warn!(%error, "failed to persist session message");
    }
}

/// Notify agent control when this session reaches a terminal turn state.
async fn notify_terminal_turn(
    agent_control: &AgentControl,
    session_id: &SessionId,
    status: AgentStatus,
) {
    if let Err(error) = agent_control
        .notify_child_terminal_turn(session_id, status)
        .await
    {
        tracing::warn!(%error, "failed to notify parent of terminal child turn");
    }
}

/// Attach the latest assistant text to completed statuses.
fn with_final_message(status: AgentStatus, context: &dyn ContextManager) -> AgentStatus {
    match status {
        AgentStatus::Completed { .. } => AgentStatus::Completed {
            message: context.last_assistant_text(),
        },
        status => status,
    }
}

/// Persist a successful turn completion marker, logging but not failing the live turn.
async fn persist_turn_complete(
    recorder: &dyn SessionRecorder,
    turn_id: &TurnId,
    stop_reason: StopReason,
) {
    let record = TurnCompleteRecord::builder()
        .turn_id(String::from(turn_id))
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
async fn persist_turn_aborted(recorder: &dyn SessionRecorder, turn_id: &TurnId, reason: String) {
    let record = TurnAbortedRecord::builder()
        .turn_id(String::from(turn_id))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_inter_agent_message_uses_structured_context_markers() {
        let rendered = test_message("hello", false).render_model_context();

        assert!(rendered.contains("<inter_agent_communication>"));
        assert!(rendered.contains("\"from\":\"/root\""));
        assert!(rendered.contains("\"to\":\"/root/child\""));
        assert!(rendered.contains("\"content\":\"hello\""));
        assert!(rendered.contains("</inter_agent_communication>"));
    }

    #[test]
    fn inter_agent_turn_input_preserves_message_envelope() {
        let rendered = test_message("do work", true).render_turn_input();

        assert!(rendered.contains("<inter_agent_communication>"));
        assert!(rendered.contains("\"trigger_turn\":true"));
        assert!(rendered.contains("\"content\":\"do work\""));
    }

    #[test]
    fn idle_non_trigger_message_is_queued_before_delivery() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("queued", false));

        let pending = queue.subscribe_mailbox();
        let messages = queue.drain_mailbox_input_items();

        assert!(pending.has_changed().expect("mailbox receiver open"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "queued");
    }

    /// Build an inter-agent message for session queue tests.
    fn test_message(content: &str, trigger_turn: bool) -> InterAgentMessage {
        InterAgentMessage::builder()
            .from(AgentPath::root())
            .to(AgentPath::root().join("child"))
            .content(content.to_string())
            .trigger_turn(trigger_turn)
            .build()
    }
}

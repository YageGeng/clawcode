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
    StopReason, TurnId,
};
use provider::factory::ArcLlm;
use skills::SkillRegistry;
use tokio::sync::{mpsc, oneshot, watch};

use crate::agent::control::AgentControl;
use crate::agent::mailbox::{Mailbox, MailboxReceiver, mailbox_pair};
use crate::context::ContextManager;
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
    /// Mailbox for receiving inter-agent messages.
    #[allow(dead_code)]
    pub(crate) mailbox: Mailbox,
    /// Tool registry available to this live session.
    pub(crate) tools: Arc<ToolRegistry>,
    /// MCP connection manager for this live session.
    pub(crate) mcp_manager: Arc<mcp::McpConnectionManager>,
    /// Recorder for canonical session history.
    pub(crate) recorder: Arc<dyn SessionRecorder>,
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
    pub agent_control: Arc<AgentControl>,
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
    /// Recorder for canonical session history.
    pub recorder: Arc<dyn SessionRecorder>,
    /// Inter-agent messages queued for model-visible delivery at the next turn boundary.
    #[builder(default)]
    pub pending_inter_agent_messages: Vec<InterAgentMessage>,
}

/// Spawn the background task for a session and return the frontend handle.
///
/// Creates all channel pairs (ops, events, approval, cancel, mailbox) and
/// wires them into the [`Session`] and [`Thread`] halves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_thread(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
    agent_path: AgentPath,
    agent_control: Arc<AgentControl>,
    approval: Arc<crate::approval::ApprovalPolicy>,
    app_config: Arc<AppConfig>,
    recorder: Arc<dyn SessionRecorder>,
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
    let runtime = Session::builder()
        .session_id(session_id.clone())
        .cwd(cwd)
        .rx_op(rx_op)
        .tx_event(Arc::clone(&tx_event))
        .cancel_rx(cancel_rx)
        .context(context)
        .llm(llm)
        .tools(Arc::clone(&tools))
        .pending_approvals(Arc::clone(&pending_approvals))
        .agent_path(agent_path)
        .approval(approval)
        .mailbox_rx(mailbox_rx)
        .agent_control(Arc::clone(&agent_control))
        .app_config(app_config)
        .skill_registry(skill_registry)
        .mcp_manager(Arc::clone(&mcp_manager))
        .recorder(Arc::clone(&recorder))
        .build();

    tokio::spawn(run_loop(runtime));

    Thread::builder()
        .session_id(session_id)
        .cwd(thread_cwd)
        .tx_op(tx_op)
        .tx_event(tx_event)
        .pending_approvals(pending_approvals)
        .cancel_tx(cancel_tx)
        .mailbox(mailbox)
        .tools(tools)
        .mcp_manager(mcp_manager)
        .recorder(recorder)
        .build()
}

/// Background task: receive ops, execute turns, emit events.
async fn run_loop(mut rt: Session) {
    loop {
        let op = rt.rx_op.recv().await;
        match op {
            // Inter-agent messages are processed identically to user prompts.
            // The content is injected as the turn input, and the turn loop
            // handles tool calls, approvals, and cancellation the same way.
            Some(Op::InterAgentMessage { message }) if !message.trigger_turn => {
                persist_inter_agent_message(&mut *rt.context, rt.recorder.as_ref(), message).await;
            }
            Some(Op::InterAgentMessage { message }) => {
                let mut next_message = Some(message);
                while let Some(message) = next_message {
                    let keep_running = run_inter_agent_turn(&mut rt, message).await;
                    if !keep_running {
                        return;
                    }
                    next_message =
                        take_next_triggering_message(&mut rt.pending_inter_agent_messages);
                }
            }
            Some(Op::Prompt { text, system, .. }) => {
                let turn_id = TurnId(uuid::Uuid::new_v4().to_string());
                let ctx = TurnContext::from_session(&rt, turn_id, TurnKindRecord::Prompt, system);
                let tx = { rt.tx_event.lock().await.clone() };

                drain_pending_inter_agent_messages(
                    &mut *rt.context,
                    Arc::clone(&rt.recorder),
                    &mut rt.pending_inter_agent_messages,
                )
                .await;

                match run_turn_select_loop(&mut rt, &ctx, text, &tx).await {
                    TurnStepOutcome::Shutdown => return,
                    TurnStepOutcome::Finished(status) => {
                        let status = with_final_message(status, &*rt.context);
                        notify_terminal_turn(&rt.agent_control, &rt.session_id, status).await;
                    }
                }

                let mut next_message =
                    take_next_triggering_message(&mut rt.pending_inter_agent_messages);
                while let Some(message) = next_message {
                    let keep_running = run_inter_agent_turn(&mut rt, message).await;
                    if !keep_running {
                        return;
                    }
                    next_message =
                        take_next_triggering_message(&mut rt.pending_inter_agent_messages);
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
                        persist_turn_aborted(rt.recorder.as_ref(), &ctx.turn_id, reason.clone())
                            .await;
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
                    Ok(()) => {
                        persist_turn_complete(
                            rt.recorder.as_ref(),
                            &ctx.turn_id,
                            StopReason::EndTurn,
                        )
                        .await;
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
                Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => {
                    return TurnStepOutcome::Shutdown;
                }
                Some(Op::InterAgentMessage { message }) => {
                    rt.pending_inter_agent_messages.push(message);
                }
                Some(other) => {
                    tracing::debug!(?other, "Ignoring operation while turn is running");
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
        &mut rt.pending_inter_agent_messages,
    )
    .await;

    match run_turn_select_loop(rt, &ctx, message.content, &tx).await {
        TurnStepOutcome::Shutdown => false,
        TurnStepOutcome::Finished(status) => {
            let status = with_final_message(status, &*rt.context);
            notify_terminal_turn(&rt.agent_control, &rt.session_id, status).await;
            true
        }
    }
}

/// Remove the next queued message that requested a follow-up turn.
fn take_next_triggering_message(pending: &mut Vec<InterAgentMessage>) -> Option<InterAgentMessage> {
    let index = pending.iter().position(|message| message.trigger_turn)?;
    Some(pending.remove(index))
}

/// Render an inter-agent message as model-visible user context.
fn render_inter_agent_message(message: &InterAgentMessage) -> String {
    format!(
        "[inter-agent message from {} to {}]\n{}",
        message.from, message.to, message.content
    )
}

/// Persist and inject all pending inter-agent messages into context.
async fn drain_pending_inter_agent_messages(
    context: &mut dyn ContextManager,
    recorder: Arc<dyn SessionRecorder>,
    pending: &mut Vec<InterAgentMessage>,
) {
    let messages = std::mem::take(pending);
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
    let rendered = render_inter_agent_message(&message);
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
    fn take_next_triggering_message_removes_first_trigger_only() {
        let mut pending = vec![
            test_message("first", false),
            test_message("second", true),
            test_message("third", true),
        ];

        let message = take_next_triggering_message(&mut pending).expect("triggering message");

        assert_eq!(message.content, "second");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].content, "first");
        assert_eq!(pending[1].content, "third");
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

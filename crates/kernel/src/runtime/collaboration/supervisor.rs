use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use snafu::ResultExt;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    Result, ThreadHandle,
    context::{SessionTaskContext, TurnContext, TurnContextItem},
    session::{SessionId, ThreadId},
};
use store::{
    AgentRegistrationRecord, MailboxDeliveryRecord, PersistedAgentStatus, PersistedMailboxEventKind,
};
use tools::{
    AgentCommandAck, AgentRuntimeContext, AgentStatus, AgentSummary, ListAgentsResponse,
    MailboxEvent, MailboxEventKind, WaitAgentResponse,
};

use super::mailbox::AgentMailbox;
use super::work_queue::AgentWorkQueue;

/// Session-scoped supervisor that tracks mailbox-backed agents and their thread identity.
pub(crate) struct AgentSupervisor {
    pub(super) store: Arc<SessionTaskContext>,
    pub(super) inner: RwLock<AgentSupervisorState>,
}

/// Mutable supervisor state guarded by an async read/write lock.
pub(super) struct AgentSupervisorState {
    pub(super) next_event_id: u64,
    pub(super) agents: HashMap<String, AgentRecord>,
    pub(super) thread_index: HashMap<(SessionId, ThreadId), String>,
    pub(super) path_index: HashMap<(SessionId, String), String>,
}

/// Durable in-memory record for one agent thread.
pub(super) struct AgentRecord {
    pub(super) session_id: SessionId,
    pub(super) agent_id: String,
    pub(super) parent_agent_id: Option<String>,
    pub(super) thread_id: ThreadId,
    pub(super) path: String,
    pub(super) name: Option<String>,
    pub(super) status: AgentStatus,
    pub(super) pending_tasks: usize,
    pub(super) hidden_root: bool,
    pub(super) accepting_tasks: bool,
    pub(super) worker_started: bool,
    pub(super) mailbox: Arc<AgentMailbox>,
    pub(super) work_queue: Arc<AgentWorkQueue>,
    pub(super) children: Vec<String>,
}

/// Result of registering a child agent in the supervisor graph.
pub(crate) struct RegisteredAgent {
    pub(crate) agent_id: String,
    pub(crate) summary: AgentSummary,
}

/// Typed request used to register one child agent after runtime IDs have
/// already been parsed from the tool-facing spawn request.
pub(super) struct ChildAgentRegistrationRequest<'a> {
    pub(super) session_id: SessionId,
    pub(super) thread_id: ThreadId,
    pub(super) origin: &'a AgentRuntimeContext,
    pub(super) name: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) system_prompt: Option<String>,
    pub(super) current_date: Option<String>,
    pub(super) timezone: Option<String>,
}

/// Structured input used to persist one agent-registration event without a
/// long positional helper signature.
struct AgentRegistrationPersistence<'a> {
    session_id: SessionId,
    agent_id: &'a str,
    parent_agent_id: Option<&'a str>,
    thread_id: &'a ThreadId,
    path: &'a str,
    name: Option<&'a str>,
    hidden_root: bool,
    turn_context: &'a TurnContext,
}

/// Structured input used to persist one mailbox-delivery event without a long
/// positional helper signature.
struct MailboxDeliveryPersistence<'a> {
    session_id: SessionId,
    recipient_agent_id: &'a str,
    source_agent_id: &'a str,
    source_path: &'a str,
    event_id: u64,
    event_kind: MailboxEventKind,
    status: AgentStatus,
    message: &'a str,
}

impl AgentRecord {
    /// Builds the stable structured summary exposed by collaboration tools.
    fn summary(&self) -> AgentSummary {
        AgentSummary {
            agent_id: self.agent_id.clone(),
            parent_agent_id: self.parent_agent_id.clone(),
            thread_id: self.thread_id.to_string(),
            path: self.path.clone(),
            name: self.name.clone(),
            status: self.status,
            pending_tasks: self.pending_tasks,
            unread_mailbox_events: 0,
        }
    }

    /// Returns whether this agent has already reached the terminal closed state.
    fn is_closed(&self) -> bool {
        self.status == AgentStatus::Closed
    }
}

impl AgentSupervisor {
    /// Creates a new session-scoped supervisor for mailbox-backed agent threads.
    pub(crate) fn new(store: Arc<SessionTaskContext>) -> Self {
        Self {
            store,
            inner: RwLock::new(AgentSupervisorState {
                next_event_id: 1,
                agents: HashMap::new(),
                thread_index: HashMap::new(),
                path_index: HashMap::new(),
            }),
        }
    }

    /// Ensures the caller thread has a hidden root-agent record that can own mailbox events.
    pub(super) async fn ensure_origin_agent(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        origin: &AgentRuntimeContext,
    ) -> Result<String> {
        {
            let state = self.inner.read().await;
            if let Some(agent_id) = state.thread_index.get(&(session_id, thread_id.clone())) {
                return Ok(agent_id.clone());
            }
        }

        let agent_id = origin
            .agent_id
            .clone()
            .unwrap_or_else(|| format!("root-{}", thread_id));
        let root_context = origin_turn_context(session_id, thread_id.clone(), &agent_id, origin);
        let mut state = self.inner.write().await;
        if let Some(existing) = state.thread_index.get(&(session_id, thread_id.clone())) {
            return Ok(existing.clone());
        }

        let record = AgentRecord {
            session_id,
            agent_id: agent_id.clone(),
            parent_agent_id: None,
            thread_id: thread_id.clone(),
            path: String::new(),
            name: origin.name.clone(),
            status: AgentStatus::Idle,
            pending_tasks: 0,
            hidden_root: true,
            accepting_tasks: true,
            worker_started: false,
            mailbox: Arc::new(AgentMailbox::new()),
            work_queue: Arc::new(AgentWorkQueue::new()),
            children: Vec::new(),
        };
        state
            .thread_index
            .insert((session_id, thread_id.clone()), agent_id.clone());
        state.agents.insert(agent_id.clone(), record);
        drop(state);

        self.store
            .seed_turn_context(session_id, thread_id.clone(), root_context.clone())
            .await;
        self.persist_agent_registered(AgentRegistrationPersistence {
            session_id,
            agent_id: &agent_id,
            parent_agent_id: None,
            thread_id: &thread_id,
            path: "",
            name: origin.name.as_deref(),
            hidden_root: true,
            turn_context: &root_context,
        })
        .await;

        Ok(agent_id)
    }

    /// Registers a child agent and seeds its turn context before any task runs.
    pub(super) async fn register_child_agent(
        &self,
        input: ChildAgentRegistrationRequest<'_>,
    ) -> Result<RegisteredAgent> {
        let session_id = input.session_id;
        let thread_id = input.thread_id;
        let origin = input.origin;
        let name = input.name;
        let cwd = input.cwd;
        let system_prompt = input.system_prompt;
        let current_date = input.current_date;
        let timezone = input.timezone;
        let parent_agent_id = self
            .ensure_origin_agent(session_id, thread_id.clone(), origin)
            .await?;
        let child_thread_id = ThreadId::new();
        let child_agent_id = Uuid::new_v4().to_string();

        let parent_context = self
            .store
            .load_turn_context(session_id, thread_id.clone())
            .await
            .unwrap_or_else(|| {
                let mut context = TurnContext::new(session_id, thread_id.clone());
                context.agent_id = parent_agent_id.clone();
                context.name = origin.name.clone();
                context.system_prompt = origin.system_prompt.clone();
                context.cwd = origin.cwd.clone();
                context.current_date = origin.current_date.clone();
                context.timezone = origin.timezone.clone();
                context
            });

        let mut child_context = parent_context.fork_child_thread(
            name.clone().unwrap_or_else(|| "agent".to_string()),
            child_thread_id.clone(),
        );
        child_context.agent_id = child_agent_id.clone();
        child_context.system_prompt = system_prompt.or_else(|| origin.system_prompt.clone());
        child_context.cwd = cwd.or_else(|| origin.cwd.clone());
        child_context.current_date = current_date.or_else(|| origin.current_date.clone());
        child_context.timezone = timezone.or_else(|| origin.timezone.clone());

        let child_context_for_persistence = child_context.clone();
        let (path, summary) = {
            let mut state = self.inner.write().await;
            let Some(parent) = state.agents.get_mut(&parent_agent_id) else {
                return Err(runtime_error(
                    "supervisor-register-child",
                    "parent agent disappeared while registering child",
                ));
            };

            let child_index = parent.children.len() + 1;
            let path = if parent.path.is_empty() {
                child_index.to_string()
            } else {
                format!("{}.{}", parent.path, child_index)
            };
            let mailbox = Arc::new(AgentMailbox::new());
            let work_queue = Arc::new(AgentWorkQueue::new());
            let record = AgentRecord {
                session_id,
                agent_id: child_agent_id.clone(),
                parent_agent_id: Some(parent_agent_id.clone()),
                thread_id: child_thread_id.clone(),
                path: path.clone(),
                name: name.clone(),
                status: AgentStatus::Idle,
                pending_tasks: 0,
                hidden_root: false,
                accepting_tasks: true,
                worker_started: false,
                mailbox,
                work_queue: Arc::clone(&work_queue),
                children: Vec::new(),
            };
            parent.children.push(child_agent_id.clone());
            state.thread_index.insert(
                (session_id, child_thread_id.clone()),
                child_agent_id.clone(),
            );
            state
                .path_index
                .insert((session_id, path.clone()), child_agent_id.clone());
            state.agents.insert(child_agent_id.clone(), record);
            let summary = state
                .agents
                .get(&child_agent_id)
                .expect("inserted child agent should exist")
                .summary();
            (path, summary)
        };

        self.store
            .seed_turn_context(session_id, child_thread_id.clone(), child_context)
            .await;
        self.persist_agent_registered(AgentRegistrationPersistence {
            session_id,
            agent_id: &child_agent_id,
            parent_agent_id: Some(&parent_agent_id),
            thread_id: &child_thread_id,
            path: &path,
            name: name.as_deref(),
            hidden_root: false,
            turn_context: &child_context_for_persistence,
        })
        .await;

        Ok(RegisteredAgent {
            agent_id: child_agent_id,
            summary,
        })
    }

    /// Resolves a user-facing agent target into the canonical session-scoped agent id.
    pub(super) async fn resolve_target_agent(
        &self,
        session_id: SessionId,
        target: &str,
    ) -> Result<String> {
        let state = self.inner.read().await;
        resolve_target_agent_id(&state, session_id, target).ok_or_else(|| {
            runtime_error(
                "supervisor-resolve-target-agent",
                &format!("unknown agent target `{target}`"),
            )
        })
    }

    /// Starts one worker lazily when the target agent is alive and has not been started yet.
    pub(super) async fn prepare_worker_start(
        &self,
        agent_id: &str,
    ) -> Result<Option<(ThreadHandle, Arc<AgentWorkQueue>)>> {
        let (session_id, thread_id, work_queue) = {
            let mut state = self.inner.write().await;
            let Some(record) = state.agents.get_mut(agent_id) else {
                return Err(runtime_error(
                    "supervisor-prepare-worker-start",
                    "agent disappeared while starting worker",
                ));
            };
            if record.worker_started || !record.accepting_tasks {
                return Ok(None);
            }
            record.worker_started = true;
            (
                record.session_id,
                record.thread_id.clone(),
                Arc::clone(&record.work_queue),
            )
        };

        Ok(Some((
            self.build_thread_handle(session_id, thread_id).await,
            work_queue,
        )))
    }

    /// Marks a lazily spawned worker as not started when thread creation fails.
    pub(super) async fn reset_worker_started(&self, agent_id: &str) {
        let mut state = self.inner.write().await;
        if let Some(record) = state.agents.get_mut(agent_id)
            && !record.is_closed()
        {
            record.worker_started = false;
        }
    }

    /// Reconstructs the thread handle for one agent from the persisted turn context baseline.
    async fn build_thread_handle(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> ThreadHandle {
        let mut thread_handle = ThreadHandle::new(session_id, thread_id.clone());
        if let Some(turn_context) = self.store.load_turn_context(session_id, thread_id).await {
            if let Some(system_prompt) = turn_context.system_prompt {
                thread_handle = thread_handle.with_system_prompt(system_prompt);
            }
            if let Some(cwd) = turn_context.cwd {
                thread_handle = thread_handle.with_cwd(cwd);
            }
        }
        thread_handle
    }

    /// Rebuilds the in-memory collaboration graph, statuses, and unread mailbox events from disk.
    ///
    /// Uses multiple passes over the event slice because session replay is a cold-start
    /// operation where clarity over the three distinct replay phases matters more than
    /// single-pass micro-optimization.
    pub(super) async fn replay_events(&self, events: &[store::SessionEvent]) -> Result<()> {
        use store::SessionEvent;

        let mut replayed_contexts = Vec::new();
        let mut mailbox_replays = Vec::new();
        let mut registered_agent_ids = Vec::new();

        {
            let mut state = self.inner.write().await;
            state.next_event_id = 1;
            state.agents.clear();
            state.thread_index.clear();
            state.path_index.clear();

            for event in events {
                if let SessionEvent::AgentRegistered {
                    session_id,
                    agent_id,
                    parent_agent_id,
                    thread_id,
                    path,
                    name,
                    hidden_root,
                    turn_context,
                    ..
                } = event
                {
                    let session_id = SessionId::from(*session_id);
                    let thread_id = ThreadId::from(*thread_id);
                    let replayed_context = deserialize_replayed_turn_context(
                        turn_context.as_ref(),
                        session_id,
                        thread_id.clone(),
                        agent_id,
                        parent_agent_id.as_deref(),
                        name.as_deref(),
                    );
                    replayed_contexts.push(replayed_context);
                    registered_agent_ids.push(agent_id.clone());
                    state
                        .thread_index
                        .insert((session_id, thread_id.clone()), agent_id.clone());
                    if !hidden_root {
                        state
                            .path_index
                            .insert((session_id, path.clone()), agent_id.clone());
                    }
                    state.agents.insert(
                        agent_id.clone(),
                        AgentRecord {
                            session_id,
                            agent_id: agent_id.clone(),
                            parent_agent_id: parent_agent_id.clone(),
                            thread_id,
                            path: path.clone(),
                            name: name.clone(),
                            status: AgentStatus::Idle,
                            pending_tasks: 0,
                            hidden_root: *hidden_root,
                            accepting_tasks: true,
                            worker_started: false,
                            mailbox: Arc::new(AgentMailbox::new()),
                            work_queue: Arc::new(AgentWorkQueue::new()),
                            children: Vec::new(),
                        },
                    );
                }
            }

            let parent_links = registered_agent_ids
                .iter()
                .filter_map(|agent_id| {
                    state
                        .agents
                        .get(agent_id)
                        .map(|record| (agent_id.clone(), record.parent_agent_id.clone()))
                })
                .collect::<Vec<_>>();
            for (agent_id, parent_agent_id) in parent_links {
                if let Some(parent_agent_id) = parent_agent_id
                    && let Some(parent) = state.agents.get_mut(&parent_agent_id)
                {
                    parent.children.push(agent_id);
                }
            }

            for event in events {
                if let SessionEvent::AgentStatusChanged {
                    agent_id, status, ..
                } = event
                    && let Some(record) = state.agents.get_mut(agent_id)
                {
                    record.status = (*status).into_runtime();
                    if record.is_closed() {
                        record.accepting_tasks = false;
                        record.pending_tasks = 0;
                    }
                }
            }

            for event in events {
                if let SessionEvent::MailboxDelivered {
                    recipient_agent_id,
                    source_agent_id,
                    source_path,
                    event_id,
                    event_kind,
                    status,
                    message,
                    ..
                } = event
                {
                    state.next_event_id = state.next_event_id.max(event_id.saturating_add(1));
                    if let Some(record) = state.agents.get(recipient_agent_id) {
                        mailbox_replays.push((
                            Arc::clone(&record.mailbox),
                            MailboxEvent {
                                event_id: *event_id,
                                agent_id: source_agent_id.clone(),
                                path: source_path.clone(),
                                event_kind: (*event_kind).into_runtime(),
                                message: message.clone(),
                                status: (*status).into_runtime(),
                            },
                        ));
                    }
                }
            }
        }

        for turn_context in replayed_contexts {
            if self
                .store
                .load_turn_context(turn_context.session_id, turn_context.thread_id.clone())
                .await
                .is_none()
            {
                self.store
                    .seed_turn_context(
                        turn_context.session_id,
                        turn_context.thread_id.clone(),
                        turn_context,
                    )
                    .await;
            }
        }

        for (mailbox, event) in mailbox_replays {
            mailbox.push(event).await;
        }

        Ok(())
    }

    /// Queues more work for an existing agent using either id-based or path-based addressing.
    pub(super) async fn enqueue_input(
        &self,
        session_id: SessionId,
        target: &str,
        input: String,
        interrupt: bool,
    ) -> Result<AgentSummary> {
        let (agent_id, queue, summary) = {
            let mut state = self.inner.write().await;
            let Some(agent_id) = resolve_target_agent_id(&state, session_id, target) else {
                return Err(runtime_error(
                    "supervisor-enqueue-input",
                    &format!("unknown agent target `{target}`"),
                ));
            };
            let Some(record) = state.agents.get_mut(&agent_id) else {
                return Err(runtime_error(
                    "supervisor-enqueue-input",
                    "target agent disappeared while queuing input",
                ));
            };
            if !record.accepting_tasks {
                return Err(runtime_error(
                    "supervisor-enqueue-input",
                    &format!("agent `{target}` is closed"),
                ));
            }

            record.pending_tasks += 1;
            if !record.is_closed() {
                record.status = AgentStatus::Running;
            }
            let queue = Arc::clone(&record.work_queue);
            let summary = record.summary();
            (agent_id.clone(), queue, summary)
        };

        queue.enqueue(input, interrupt).await;
        self.persist_agent_status(session_id, &agent_id, AgentStatus::Running, None)
            .await;
        Ok(summary)
    }

    /// Waits for the next mailbox event delivered to the caller agent.
    pub(super) async fn wait_for_event(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        origin: &AgentRuntimeContext,
        targets: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<WaitAgentResponse> {
        let caller_agent_id = self
            .ensure_origin_agent(session_id, thread_id, origin)
            .await?;
        let (mailbox, resolved_targets) = {
            let state = self.inner.read().await;
            let Some(record) = state.agents.get(&caller_agent_id) else {
                return Err(runtime_error(
                    "supervisor-wait-agent",
                    "caller agent disappeared while waiting",
                ));
            };
            let mailbox = Arc::clone(&record.mailbox);
            let mut resolved_targets = HashSet::new();
            for target in targets {
                let Some(agent_id) = resolve_target_agent_id(&state, session_id, target) else {
                    return Err(runtime_error(
                        "supervisor-wait-agent",
                        &format!("unknown agent target `{target}`"),
                    ));
                };
                resolved_targets.insert(agent_id);
            }
            (mailbox, resolved_targets)
        };

        if let Some(event) = mailbox.pop_matching(&resolved_targets).await {
            return Ok(WaitAgentResponse {
                timed_out: false,
                event: Some(event),
            });
        }

        let mut updates = mailbox.subscribe();
        let wait_future = async {
            loop {
                updates
                    .changed()
                    .await
                    .context(crate::error::ChannelSnafu {
                        stage: "supervisor-wait-agent".to_string(),
                    })?;
                if let Some(event) = mailbox.pop_matching(&resolved_targets).await {
                    return Ok(WaitAgentResponse {
                        timed_out: false,
                        event: Some(event),
                    });
                }
            }
        };

        if let Some(timeout_ms) = timeout_ms {
            use std::time::Duration;

            match tokio::time::timeout(Duration::from_millis(timeout_ms), wait_future).await {
                Ok(result) => result,
                Err(_) => Ok(WaitAgentResponse {
                    timed_out: true,
                    event: None,
                }),
            }
        } else {
            wait_future.await
        }
    }

    /// Returns a visible session-scoped snapshot of the current agent graph.
    pub(super) async fn list_agents(&self, session_id: SessionId) -> ListAgentsResponse {
        let agents = {
            let state = self.inner.read().await;
            state
                .agents
                .values()
                .filter(|record| record.session_id == session_id && !record.hidden_root)
                .map(|record| (record.summary(), Arc::clone(&record.mailbox)))
                .collect::<Vec<_>>()
        };

        let mut summaries = Vec::with_capacity(agents.len());
        for (mut summary, mailbox) in agents {
            summary.unread_mailbox_events = mailbox.unread_len().await;
            summaries.push(summary);
        }
        summaries.sort_by_key(|summary| AgentPathSortKey::from(summary.path.as_str()));

        ListAgentsResponse { agents: summaries }
    }

    /// Closes an agent subtree and stops accepting new tasks for every descendant.
    pub(super) async fn close_agent(
        &self,
        session_id: SessionId,
        target: &str,
    ) -> Result<AgentCommandAck> {
        let (root_agent_id, descendants) = {
            let state = self.inner.read().await;
            let Some(root_agent_id) = resolve_target_agent_id(&state, session_id, target) else {
                return Err(runtime_error(
                    "supervisor-close-agent",
                    &format!("unknown agent target `{target}`"),
                ));
            };
            let descendants = collect_descendants(&state, &root_agent_id);
            (root_agent_id, descendants)
        };

        let mut root_summary = None;
        for agent_id in descendants {
            let maybe_queue = {
                let mut state = self.inner.write().await;
                let Some(record) = state.agents.get_mut(&agent_id) else {
                    return Err(runtime_error(
                        "supervisor-close-agent",
                        "agent disappeared while closing subtree",
                    ));
                };
                record.accepting_tasks = false;
                record.pending_tasks = 0;
                record.status = AgentStatus::Closed;
                let summary = record.summary();
                if agent_id == root_agent_id {
                    root_summary = Some(summary.clone());
                }
                Some((
                    record.session_id,
                    record.agent_id.clone(),
                    Arc::clone(&record.work_queue),
                ))
            };

            if let Some((record_session_id, closed_agent_id, queue)) = maybe_queue {
                queue.close().await;
                self.persist_agent_status(
                    record_session_id,
                    &closed_agent_id,
                    AgentStatus::Closed,
                    Some("closed"),
                )
                .await;
                let _ = self
                    .publish_agent_event(
                        &closed_agent_id,
                        MailboxEventKind::Closed,
                        AgentStatus::Closed,
                        "closed".to_string(),
                    )
                    .await;
            }
        }

        Ok(AgentCommandAck {
            agent: root_summary.ok_or_else(|| {
                runtime_error("supervisor-close-agent", "missing root summary after close")
            })?,
            queued: false,
        })
    }

    /// Marks one task dequeue so the pending-task count stays aligned with the worker queue.
    pub(super) async fn on_task_started(&self, agent_id: &str) -> Result<()> {
        let session_id = {
            let mut state = self.inner.write().await;
            let Some(record) = state.agents.get_mut(agent_id) else {
                return Err(runtime_error(
                    "supervisor-task-started",
                    "agent disappeared while starting a task",
                ));
            };
            record.pending_tasks = record.pending_tasks.saturating_sub(1);
            if !record.is_closed() {
                record.status = AgentStatus::Running;
            }
            record.session_id
        };
        if let Some(status) = self.agent_status(agent_id).await
            && status == AgentStatus::Running
        {
            self.persist_agent_status(session_id, agent_id, AgentStatus::Running, None)
                .await;
        }
        Ok(())
    }

    /// Publishes the terminal result of one child task to every ancestor mailbox.
    pub(super) async fn on_task_finished(
        &self,
        agent_id: &str,
        event_kind: MailboxEventKind,
        status: AgentStatus,
        message: String,
    ) -> Result<()> {
        self.publish_agent_event(agent_id, event_kind, status, message)
            .await
            .map(|_| ())
    }

    /// Publishes one agent event to every ancestor mailbox and persists the same transition.
    pub(super) async fn publish_agent_event(
        &self,
        agent_id: &str,
        event_kind: MailboxEventKind,
        status: AgentStatus,
        message: String,
    ) -> Result<AgentSummary> {
        let (session_id, source_path, recipients, event_id, summary) = {
            let mut state = self.inner.write().await;
            let Some(record) = state.agents.get(agent_id) else {
                return Err(runtime_error(
                    "supervisor-publish-agent-event",
                    "agent disappeared while publishing mailbox event",
                ));
            };
            if record.is_closed() && status != AgentStatus::Closed {
                return Ok(record.summary());
            }
            let event_id = state.next_event_id;
            state.next_event_id += 1;
            let record = state
                .agents
                .get_mut(agent_id)
                .expect("existing agent should still exist while publishing");
            record.status = status;
            let session_id = record.session_id;
            let source_path = record.path.clone();
            let parent_agent_id = record.parent_agent_id.clone();
            let summary = record.summary();
            let recipients = ancestor_mailboxes(&state, parent_agent_id);
            (session_id, source_path, recipients, event_id, summary)
        };

        self.persist_agent_status(session_id, agent_id, status, Some(message.as_str()))
            .await;
        let event = MailboxEvent {
            event_id,
            agent_id: agent_id.to_string(),
            path: source_path.clone(),
            event_kind,
            message: message.clone(),
            status,
        };
        for (recipient_agent_id, mailbox) in recipients {
            mailbox.push(event.clone()).await;
            self.persist_mailbox_delivery(MailboxDeliveryPersistence {
                session_id,
                recipient_agent_id: &recipient_agent_id,
                source_agent_id: agent_id,
                source_path: &source_path,
                event_id,
                event_kind,
                status,
                message: &message,
            })
            .await;
        }

        Ok(summary)
    }

    /// Returns the latest status for one in-memory agent when it still exists.
    pub(super) async fn agent_status(&self, agent_id: &str) -> Option<AgentStatus> {
        let state = self.inner.read().await;
        state.agents.get(agent_id).map(|record| record.status)
    }

    /// Persists a newly registered agent node when a session store is configured.
    async fn persist_agent_registered(&self, input: AgentRegistrationPersistence<'_>) {
        if let Some(persistence) = self.store.persistence().cloned() {
            let turn_context = serde_json::to_value(input.turn_context.to_turn_context_item())
                .map(Some)
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        "failed to serialize agent turn context for persistence: {error}"
                    );
                    None
                });
            persistence
                .record_agent_registered(AgentRegistrationRecord {
                    session_id: input.session_id.as_uuid(),
                    agent_id: input.agent_id.to_string(),
                    parent_agent_id: input.parent_agent_id.map(ToOwned::to_owned),
                    thread_id: input.thread_id.as_uuid(),
                    path: input.path.to_string(),
                    name: input.name.map(ToOwned::to_owned),
                    hidden_root: input.hidden_root,
                    turn_context,
                })
                .await;
        }
    }

    /// Persists the latest agent status when a session store is configured.
    async fn persist_agent_status(
        &self,
        session_id: SessionId,
        agent_id: &str,
        status: AgentStatus,
        detail: Option<&str>,
    ) {
        if let Some(persistence) = self.store.persistence().cloned() {
            persistence
                .record_agent_status(
                    session_id.as_uuid(),
                    agent_id,
                    status.into_persisted(),
                    detail,
                )
                .await;
        }
    }

    /// Persists one mailbox delivery when a session store is configured.
    async fn persist_mailbox_delivery(&self, input: MailboxDeliveryPersistence<'_>) {
        if let Some(persistence) = self.store.persistence().cloned() {
            persistence
                .record_mailbox_delivered(MailboxDeliveryRecord {
                    session_id: input.session_id.as_uuid(),
                    recipient_agent_id: input.recipient_agent_id.to_string(),
                    source_agent_id: input.source_agent_id.to_string(),
                    source_path: input.source_path.to_string(),
                    event_id: input.event_id,
                    event_kind: input.event_kind.into_persisted(),
                    status: input.status.into_persisted(),
                    message: input.message.to_string(),
                })
                .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Walks the ancestor chain and returns mailbox handles for every parent agent.
fn ancestor_mailboxes(
    state: &AgentSupervisorState,
    mut parent_agent_id: Option<String>,
) -> Vec<(String, Arc<AgentMailbox>)> {
    let mut recipients = Vec::new();
    while let Some(agent_id) = parent_agent_id {
        let Some(record) = state.agents.get(&agent_id) else {
            break;
        };
        recipients.push((record.agent_id.clone(), Arc::clone(&record.mailbox)));
        parent_agent_id = record.parent_agent_id.clone();
    }
    recipients
}

/// Resolves one target string as either an agent id or a path within the same session.
fn resolve_target_agent_id(
    state: &AgentSupervisorState,
    session_id: SessionId,
    target: &str,
) -> Option<String> {
    state
        .agents
        .get(target)
        .filter(|record| record.session_id == session_id)
        .map(|record| record.agent_id.clone())
        .or_else(|| {
            state
                .path_index
                .get(&(session_id, target.to_string()))
                .cloned()
        })
}

/// Collects one agent id plus every descendant in depth-first order.
fn collect_descendants(state: &AgentSupervisorState, root_agent_id: &str) -> Vec<String> {
    let mut stack = vec![root_agent_id.to_string()];
    let mut descendants = Vec::new();
    while let Some(agent_id) = stack.pop() {
        descendants.push(agent_id.clone());
        if let Some(record) = state.agents.get(&agent_id) {
            for child in record.children.iter().rev() {
                stack.push(child.clone());
            }
        }
    }
    descendants
}

/// Builds a kernel runtime error with the standard structured payload shape.
fn runtime_error(stage: &str, message: &str) -> crate::Error {
    crate::Error::Runtime {
        message: message.to_string(),
        stage: stage.to_string(),
        inflight_snapshot: None,
    }
}

/// Sort key that keeps dotted agent paths in numeric tree order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AgentPathSortKey {
    segments: Vec<u32>,
    raw: String,
}

impl From<&str> for AgentPathSortKey {
    /// Parses one dotted agent path into numeric segments and keeps the raw
    /// string as a stable tie-breaker for malformed or non-canonical paths.
    fn from(path: &str) -> Self {
        Self {
            segments: path
                .split('.')
                .filter(|segment| !segment.is_empty())
                .map(|segment| segment.parse::<u32>().unwrap_or(u32::MAX))
                .collect(),
            raw: path.to_string(),
        }
    }
}

/// Builds the stable root-agent turn context from the caller environment.
fn origin_turn_context(
    session_id: SessionId,
    thread_id: ThreadId,
    agent_id: &str,
    origin: &AgentRuntimeContext,
) -> TurnContext {
    let mut turn_context = TurnContext::new(session_id, thread_id);
    turn_context.agent_id = agent_id.to_string();
    turn_context.name = origin.name.clone();
    turn_context.system_prompt = origin.system_prompt.clone();
    turn_context.cwd = origin.cwd.clone();
    turn_context.current_date = origin.current_date.clone();
    turn_context.timezone = origin.timezone.clone();
    turn_context
}

/// Reconstructs a stored turn context or falls back to the minimum agent identity data.
fn deserialize_replayed_turn_context(
    turn_context: Option<&serde_json::Value>,
    session_id: SessionId,
    thread_id: ThreadId,
    agent_id: &str,
    parent_agent_id: Option<&str>,
    name: Option<&str>,
) -> TurnContext {
    turn_context
        .and_then(|value| serde_json::from_value::<TurnContextItem>(value.clone()).ok())
        .map(TurnContext::from_item)
        .unwrap_or_else(|| {
            let mut fallback = TurnContext::new(session_id, thread_id);
            fallback.agent_id = agent_id.to_string();
            fallback.parent_agent_id = parent_agent_id.map(ToOwned::to_owned);
            fallback.name = name.map(ToOwned::to_owned);
            fallback
        })
}

// ---------------------------------------------------------------------------
// Collaboration enum conversion traits
// ---------------------------------------------------------------------------

/// Converts runtime collaboration enums into their persisted session-store shape.
trait IntoPersisted {
    /// The persisted representation emitted into the JSONL session stream.
    type Persisted;

    /// Converts one runtime enum into its persisted equivalent.
    fn into_persisted(self) -> Self::Persisted;
}

/// Converts persisted collaboration enums back into runtime collaboration enums.
trait IntoRuntime {
    /// The runtime representation consumed by collaboration APIs.
    type Runtime;

    /// Converts one persisted enum into its runtime equivalent.
    fn into_runtime(self) -> Self::Runtime;
}

impl IntoPersisted for AgentStatus {
    type Persisted = PersistedAgentStatus;

    /// Converts one runtime agent status into the persisted status enum.
    fn into_persisted(self) -> Self::Persisted {
        match self {
            AgentStatus::Idle => PersistedAgentStatus::Idle,
            AgentStatus::Running => PersistedAgentStatus::Running,
            AgentStatus::Completed => PersistedAgentStatus::Completed,
            AgentStatus::Failed => PersistedAgentStatus::Failed,
            AgentStatus::Closed => PersistedAgentStatus::Closed,
        }
    }
}

impl IntoRuntime for PersistedAgentStatus {
    type Runtime = AgentStatus;

    /// Converts one persisted agent status into the runtime status enum.
    fn into_runtime(self) -> Self::Runtime {
        match self {
            PersistedAgentStatus::Idle => AgentStatus::Idle,
            PersistedAgentStatus::Running => AgentStatus::Running,
            PersistedAgentStatus::Completed => AgentStatus::Completed,
            PersistedAgentStatus::Failed => AgentStatus::Failed,
            PersistedAgentStatus::Closed => AgentStatus::Closed,
        }
    }
}

impl IntoPersisted for MailboxEventKind {
    type Persisted = PersistedMailboxEventKind;

    /// Converts one runtime mailbox event kind into the persisted event-kind enum.
    fn into_persisted(self) -> Self::Persisted {
        match self {
            MailboxEventKind::Spawned => PersistedMailboxEventKind::Spawned,
            MailboxEventKind::Running => PersistedMailboxEventKind::Running,
            MailboxEventKind::Completed => PersistedMailboxEventKind::Completed,
            MailboxEventKind::Failed => PersistedMailboxEventKind::Failed,
            MailboxEventKind::Closed => PersistedMailboxEventKind::Closed,
        }
    }
}

impl IntoRuntime for PersistedMailboxEventKind {
    type Runtime = MailboxEventKind;

    /// Converts one persisted mailbox event kind into the runtime event-kind enum.
    fn into_runtime(self) -> Self::Runtime {
        match self {
            PersistedMailboxEventKind::Spawned => MailboxEventKind::Spawned,
            PersistedMailboxEventKind::Running => MailboxEventKind::Running,
            PersistedMailboxEventKind::Completed => MailboxEventKind::Completed,
            PersistedMailboxEventKind::Failed => MailboxEventKind::Failed,
            PersistedMailboxEventKind::Closed => MailboxEventKind::Closed,
        }
    }
}

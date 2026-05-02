use std::{sync::Arc, thread};

use async_trait::async_trait;
use snafu::ResultExt;

use crate::{
    Result, ThreadHandle, ThreadRuntime,
    context::SessionTaskContext,
    events::NoopEventSink,
    model::AgentModel,
    runtime::{
        AgentLoopConfig, RunRequest,
        collaboration::supervisor::{AgentSupervisor, ChildAgentRegistrationRequest},
        collaboration::work_queue::AgentWorkQueue,
    },
    session::{SessionId, ThreadId},
};
use tools::{
    AgentCommandAck, AgentStatus, CloseAgentRequest, CollaborationRuntime, ListAgentsRequest,
    ListAgentsResponse, MailboxEventKind, SendAgentInputRequest, SpawnAgentRequest,
    SpawnAgentResponse, WaitAgentRequest, WaitAgentResponse,
};

/// Collaboration runtime handle bound to one runtime dependency set.
pub(crate) struct KernelCollaborationRuntime<M> {
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    tools: Arc<tools::ToolRouter>,
    config: AgentLoopConfig,
    supervisor: Arc<AgentSupervisor>,
}

impl<M> KernelCollaborationRuntime<M> {
    /// Builds a collaboration runtime backed by a shared supervisor and runtime dependencies.
    pub(crate) fn new(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        tools: Arc<tools::ToolRouter>,
        config: AgentLoopConfig,
        supervisor: Arc<AgentSupervisor>,
    ) -> Self {
        Self {
            model,
            store,
            tools,
            config,
            supervisor,
        }
    }

    /// Starts the target worker lazily so replayed or idle agents only consume a thread on demand.
    async fn ensure_worker_thread(&self, agent_id: &str) -> Result<()>
    where
        M: AgentModel + 'static,
    {
        if let Some((thread_handle, work_queue)) =
            self.supervisor.prepare_worker_start(agent_id).await?
            && let Err(error) =
                self.spawn_worker_thread(agent_id.to_string(), thread_handle, work_queue)
        {
            self.supervisor.reset_worker_started(agent_id).await;
            return Err(error);
        }
        Ok(())
    }

    /// Spawns the dedicated worker thread that drains one child agent's queue.
    ///
    /// Uses `std::thread::Builder` with a dedicated single-threaded tokio runtime
    /// per agent rather than `tokio::spawn` so that each child agent runs in an
    /// isolated OS thread. This prevents one agent's CPU-heavy turn from starving
    /// the supervisor or sibling agents.
    fn spawn_worker_thread(
        &self,
        agent_id: String,
        thread_handle: ThreadHandle,
        work_queue: Arc<AgentWorkQueue>,
    ) -> Result<()>
    where
        M: AgentModel + 'static,
    {
        // Child workers surface completion through the mailbox/supervisor layer.
        // Their internal reasoning and tool events must stay out of the parent
        // ACP session stream, so child turns intentionally run with a private
        // no-op sink instead of reusing the caller's event sink.
        let runtime = ThreadRuntime::new_with_supervisor(
            Arc::clone(&self.model),
            Arc::clone(&self.store),
            Arc::clone(&self.tools),
            Arc::new(NoopEventSink),
            Arc::clone(&self.supervisor),
        )
        .with_config(self.config.clone());
        let supervisor = Arc::clone(&self.supervisor);

        thread::Builder::new()
            .name(format!("clawcode-agent-{agent_id}"))
            .spawn(move || {
                let local = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("agent worker runtime should be creatable");
                local.block_on(async move {
                    while let Some(input) = work_queue.next_input().await {
                        if let Err(error) = supervisor.on_task_started(&agent_id).await {
                            tracing::warn!("failed to mark agent task as started: {error}");
                        }
                        let outcome = runtime
                            .run_request(RunRequest::new(
                                *thread_handle.session_id(),
                                thread_handle.thread_id().clone(),
                                input,
                            ))
                            .await;
                        let publish_result = match outcome {
                            Ok(result) => {
                                supervisor
                                    .on_task_finished(
                                        &agent_id,
                                        MailboxEventKind::Completed,
                                        AgentStatus::Completed,
                                        result.text,
                                    )
                                    .await
                            }
                            Err(error) => {
                                supervisor
                                    .on_task_finished(
                                        &agent_id,
                                        MailboxEventKind::Failed,
                                        AgentStatus::Failed,
                                        error.display_message(),
                                    )
                                    .await
                            }
                        };
                        if let Err(error) = publish_result {
                            tracing::warn!("failed to publish agent task result: {error}");
                        }
                    }
                });
            })
            .context(crate::error::IoSnafu {
                stage: "spawn-agent-worker-thread".to_string(),
            })?;
        Ok(())
    }
}

#[async_trait]
impl<M> CollaborationRuntime for KernelCollaborationRuntime<M>
where
    M: AgentModel + 'static,
{
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> tools::Result<SpawnAgentResponse> {
        let started = request.task.is_some();
        let session_id = parse_runtime_id::<SessionId>(&request.session_id, "parse-session-id")?;
        let thread_id = parse_runtime_id::<ThreadId>(&request.thread_id, "parse-thread-id")?;
        let registered = self
            .supervisor
            .register_child_agent(ChildAgentRegistrationRequest {
                session_id,
                thread_id,
                origin: &request.origin,
                max_subagent_depth: self.config.max_subagent_depth,
                name: request.name,
                cwd: request.cwd,
                system_prompt: request.system_prompt,
                current_date: request.current_date,
                timezone: request.timezone,
            })
            .await?;

        let summary = if let Some(task) = request.task {
            self.ensure_worker_thread(&registered.agent_id).await?;
            self.supervisor
                .enqueue_input(session_id, &registered.agent_id, task, false)
                .await?
        } else {
            registered.summary
        };

        Ok(SpawnAgentResponse {
            agent: summary,
            started,
        })
    }

    async fn send_agent_input(
        &self,
        request: SendAgentInputRequest,
    ) -> tools::Result<AgentCommandAck> {
        let session_id = parse_runtime_id::<SessionId>(&request.session_id, "parse-session-id")?;
        let agent_id = self
            .supervisor
            .resolve_target_agent(session_id, &request.target)
            .await?;
        self.ensure_worker_thread(&agent_id).await?;
        let summary = self
            .supervisor
            .enqueue_input(session_id, &agent_id, request.input, request.interrupt)
            .await?;
        Ok(AgentCommandAck {
            agent: summary,
            queued: true,
        })
    }

    async fn wait_agent(&self, request: WaitAgentRequest) -> tools::Result<WaitAgentResponse> {
        let session_id = parse_runtime_id::<SessionId>(&request.session_id, "parse-session-id")?;
        let thread_id = parse_runtime_id::<ThreadId>(&request.thread_id, "parse-thread-id")?;
        Ok(self
            .supervisor
            .wait_for_event(
                session_id,
                thread_id,
                &request.origin,
                &request.targets,
                request.timeout_ms,
            )
            .await?)
    }

    async fn close_agent(&self, request: CloseAgentRequest) -> tools::Result<AgentCommandAck> {
        let session_id = parse_runtime_id::<SessionId>(&request.session_id, "parse-session-id")?;
        Ok(self
            .supervisor
            .close_agent(session_id, &request.target)
            .await?)
    }

    async fn list_agents(&self, request: ListAgentsRequest) -> tools::Result<ListAgentsResponse> {
        let session_id = parse_runtime_id::<SessionId>(&request.session_id, "parse-session-id")?;
        Ok(self.supervisor.list_agents(session_id).await)
    }
}

/// Parses one UUID-backed runtime identifier from its stable external string form.
fn parse_runtime_id<T>(input: &str, stage: &str) -> tools::Result<T>
where
    for<'a> T: TryFrom<&'a str, Error = uuid::Error>,
{
    T::try_from(input).context(tools::error::InvalidIdentifierSnafu {
        input: input.to_string(),
        stage: stage.to_string(),
    })
}

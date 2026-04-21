use std::sync::Arc;

use llm::{completion::Message, usage::Usage};
use uuid::Uuid;

use crate::{
    Result,
    events::{
        AgentEvent, EventSink, TaskContinuationAction, TaskContinuationDecisionKind,
        TaskContinuationDecisionStage, TaskContinuationDecisionTraceEntry, TaskContinuationSource,
    },
    model::AgentModel,
    session::{SessionContinuationRequest, SessionId, SessionStore, ThreadId},
    tools::router::ToolRouter,
};

use super::agent_loop::run_turn as run_loop_turn;
use super::{AgentLoopConfig, AgentLoopRequest};

/// Shared dependencies that stay stable for one agent lifecycle.
pub struct AgentDeps<M, S, E> {
    pub model: Arc<M>,
    pub store: Arc<S>,
    pub tools: Arc<ToolRouter>,
    pub events: Arc<E>,
}

impl<M, S, E> AgentDeps<M, S, E> {
    /// Builds the shared dependency bundle for an agent instance.
    pub fn new(model: Arc<M>, store: Arc<S>, tools: Arc<ToolRouter>, events: Arc<E>) -> Self {
        Self {
            model,
            store,
            tools,
            events,
        }
    }
}

/// Stable identity and conversation scope for an agent instance.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub agent_id: String,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub parent_agent_id: Option<String>,
    pub name: Option<String>,
}

impl AgentContext {
    /// Creates a root agent context bound to a single session thread.
    pub fn new(session_id: SessionId, thread_id: ThreadId) -> Self {
        Self {
            agent_id: Uuid::new_v4().to_string(),
            session_id,
            thread_id,
            parent_agent_id: None,
            name: None,
        }
    }

    /// Attaches a human-readable name to the agent identity.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Records the parent agent identifier for a derived agent context.
    pub fn with_parent_agent_id(mut self, parent_agent_id: impl Into<String>) -> Self {
        self.parent_agent_id = Some(parent_agent_id.into());
        self
    }

    /// Forks a child agent identity that stays on the same session thread.
    pub fn fork(&self, name: impl Into<String>) -> Self {
        Self::new(self.session_id.clone(), self.thread_id.clone())
            .with_parent_agent_id(self.agent_id.clone())
            .with_name(name)
    }
}

/// Default behavior for an agent across multiple turns.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    pub system_prompt: Option<String>,
    pub loop_config: AgentLoopConfig,
}

/// One agent turn request with optional prompt overrides.
#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub input: String,
    pub system_prompt_override: Option<String>,
}

impl AgentRunRequest {
    /// Builds a single-turn request for an existing agent instance.
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            system_prompt_override: None,
        }
    }
}

/// Long-lived agent object that owns stable context and shared dependencies.
pub struct Agent<M, S, E> {
    context: AgentContext,
    config: AgentConfig,
    deps: AgentDeps<M, S, E>,
}

impl<M, S, E> Agent<M, S, E>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
    E: EventSink + 'static,
{
    /// Builds an agent bound to one session thread and dependency set.
    pub fn new(context: AgentContext, deps: AgentDeps<M, S, E>) -> Self {
        Self {
            context,
            config: AgentConfig::default(),
            deps,
        }
    }

    /// Overrides the default runtime loop configuration for this agent.
    pub fn with_config(mut self, config: AgentLoopConfig) -> Self {
        self.config.loop_config = config;
        self
    }

    /// Sets the default system prompt applied to each turn.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(system_prompt.into());
        self
    }

    /// Runs one input turn inside the agent's stable session context.
    pub async fn run(&self, request: AgentRunRequest) -> Result<RunResult> {
        match self.run_outcome(request).await? {
            RunOutcome::Success(result) => Ok(result),
            RunOutcome::Failure(failure) => Err(failure.error),
        }
    }

    /// Runs one input turn and returns a structured success or failure payload.
    pub async fn run_outcome(&self, request: AgentRunRequest) -> Result<RunOutcome> {
        let run_request = RunRequest::new(
            self.context.session_id.clone(),
            self.context.thread_id.clone(),
            request.input,
        );
        let system_prompt = request
            .system_prompt_override
            .or_else(|| self.config.system_prompt.clone());

        run_task(
            self.deps.model.as_ref(),
            self.deps.store.as_ref(),
            self.deps.tools.as_ref(),
            self.deps.events.as_ref(),
            &self.config.loop_config,
            system_prompt,
            run_request,
        )
        .await
    }
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub input: String,
}

impl RunRequest {
    /// Builds a runtime request from explicit identifiers and user input.
    pub fn new(session_id: SessionId, thread_id: ThreadId, input: impl Into<String>) -> Self {
        Self {
            session_id,
            thread_id,
            input: input.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub text: String,
    pub usage: Usage,
    pub iterations: usize,
    pub inflight_snapshot: super::ToolCallRuntimeSnapshot,
    pub continuation_decision_trace: Vec<crate::events::TaskContinuationDecisionTraceEntry>,
}

/// Structured failure payload for callers that want runtime state without unpacking `Error`.
#[derive(Debug)]
pub struct RunFailure {
    pub error: crate::Error,
    pub inflight_snapshot: super::ToolCallRuntimeSnapshot,
    pub continuation_decision_trace: Vec<crate::events::TaskContinuationDecisionTraceEntry>,
}

/// Structured turn outcome that exposes success and failure payloads symmetrically.
#[derive(Debug)]
pub enum RunOutcome {
    Success(RunResult),
    Failure(RunFailure),
}

/// Internal turn request that carries the persisted run request plus task-scoped loop state.
#[derive(Debug, Clone)]
struct TurnExecutionRequest {
    request: RunRequest,
    system_prompt: Option<String>,
    next_tool_handle_sequence: usize,
}

/// Internal task-level decision describing whether the outer runtime loop should continue.
#[derive(Debug)]
enum TaskContinuation {
    Finish,
    Continue(SessionContinuationRequest),
}

/// Converts one task-continuation outcome into the final public trace entry recorded for the turn.
impl From<&TaskContinuation> for TaskContinuationDecisionTraceEntry {
    fn from(continuation: &TaskContinuation) -> Self {
        match continuation {
            TaskContinuation::Finish => Self {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Finished,
                source: Some(TaskContinuationSource::TaskCompleted),
            },
            TaskContinuation::Continue(continuation) => Self {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::from(continuation)),
            },
        }
    }
}

impl TaskContinuation {
    /// Returns the public continuation action emitted by the outer task loop.
    fn action(&self) -> TaskContinuationAction {
        match self {
            Self::Finish => TaskContinuationAction::Finish,
            Self::Continue(_) => TaskContinuationAction::Continue,
        }
    }

    /// Returns the source that caused the outer task loop to continue or finish.
    fn source(&self) -> TaskContinuationSource {
        match self {
            Self::Finish => TaskContinuationSource::TaskCompleted,
            Self::Continue(SessionContinuationRequest::PendingInput { .. }) => {
                TaskContinuationSource::PendingInput
            }
            Self::Continue(SessionContinuationRequest::SystemFollowUp { .. }) => {
                TaskContinuationSource::SystemFollowUp
            }
        }
    }

    /// Converts the queued continuation request into the next turn request.
    fn into_run_request(self, session_id: SessionId, thread_id: ThreadId) -> Option<RunRequest> {
        match self {
            Self::Finish => None,
            Self::Continue(SessionContinuationRequest::PendingInput { input }) => {
                Some(RunRequest::new(session_id, thread_id, input))
            }
            Self::Continue(SessionContinuationRequest::SystemFollowUp { input }) => {
                // System-driven follow-ups reuse the same turn submission path as
                // pending user input; only the continuation source differs.
                Some(RunRequest::new(session_id, thread_id, input))
            }
        }
    }
}

pub struct AgentRunner<M, S, E> {
    model: Arc<M>,
    store: Arc<S>,
    router: Arc<ToolRouter>,
    events: Arc<E>,
    config: AgentLoopConfig,
    system_prompt: Option<String>,
}

impl<M, S, E> AgentRunner<M, S, E>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
    E: EventSink + 'static,
{
    /// Builds a runner from the model, stores, registry, and event sink.
    pub fn new(model: Arc<M>, store: Arc<S>, router: Arc<ToolRouter>, events: Arc<E>) -> Self {
        Self {
            model,
            store,
            router,
            events,
            config: AgentLoopConfig::default(),
            system_prompt: None,
        }
    }

    /// Overrides the default runtime loop configuration.
    pub fn with_config(mut self, config: AgentLoopConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets a system prompt injected into each model request.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    /// Runs one user input against the runtime and persists the resulting turn.
    pub async fn run(&self, request: RunRequest) -> Result<RunResult> {
        match self.run_outcome(request).await? {
            RunOutcome::Success(result) => Ok(result),
            RunOutcome::Failure(failure) => Err(failure.error),
        }
    }

    /// Runs one user input and returns a structured success or failure payload.
    pub async fn run_outcome(&self, request: RunRequest) -> Result<RunOutcome> {
        run_task(
            self.model.as_ref(),
            self.store.as_ref(),
            self.router.as_ref(),
            self.events.as_ref(),
            &self.config,
            self.system_prompt.clone(),
            request,
        )
        .await
    }
}

/// Executes one runtime task and wraps the inner turn result into the public outcome type.
async fn run_task<M, S, E>(
    model: &M,
    store: &S,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    system_prompt: Option<String>,
    request: RunRequest,
) -> Result<RunOutcome>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
    E: EventSink + 'static,
{
    events
        .publish(AgentEvent::RunStarted {
            session_id: request.session_id.to_string(),
            thread_id: request.thread_id.to_string(),
            input: request.input.clone(),
        })
        .await;

    let mut next_turn_request = Some(request);
    let mut total_usage = Usage::new();
    let mut total_iterations = 0usize;
    let mut turn_index = 0usize;
    let mut task_inflight_snapshot = super::ToolCallRuntimeSnapshot::default();
    let mut task_continuation_decision_trace = Vec::new();
    let mut next_tool_handle_sequence = 0usize;
    while let Some(turn_request) = next_turn_request.take() {
        turn_index += 1;
        let turn_result = run_turn(
            model,
            store,
            router,
            events,
            config,
            TurnExecutionRequest {
                request: turn_request.clone(),
                system_prompt: system_prompt.clone(),
                next_tool_handle_sequence,
            },
        )
        .await;
        let loop_result = match turn_result {
            Ok(loop_result) => loop_result,
            Err(error) => {
                return Ok(build_task_failure_outcome(
                    error,
                    &task_inflight_snapshot,
                    task_continuation_decision_trace,
                ));
            }
        };

        total_usage += loop_result.usage;
        total_iterations += loop_result.iterations;
        next_tool_handle_sequence = loop_result.next_tool_handle_sequence;
        task_inflight_snapshot.extend(loop_result.inflight_snapshot.clone());

        let (continuation, mut decision_trace) =
            match decide_task_continuation(store, &turn_request, &loop_result, config).await {
                Ok(decision) => decision,
                Err(error) => {
                    let continuation_decision_trace = merge_continuation_traces(
                        task_continuation_decision_trace,
                        loop_result.continuation_decision_trace.clone(),
                    );
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        continuation_decision_trace,
                    ));
                }
            };
        decision_trace.push(TaskContinuationDecisionTraceEntry::from(&continuation));
        task_continuation_decision_trace.extend(decision_trace.clone());
        events
            .publish(AgentEvent::TaskContinuationDecided {
                turn_index,
                action: continuation.action(),
                source: continuation.source(),
                decision_trace,
            })
            .await;

        let next_request = continuation.into_run_request(
            turn_request.session_id.clone(),
            turn_request.thread_id.clone(),
        );
        match next_request {
            Some(next_request) => {
                if let Err(error) = store
                    .finalize_turn(
                        turn_request.session_id.clone(),
                        turn_request.thread_id.clone(),
                        loop_result.usage,
                    )
                    .await
                {
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        task_continuation_decision_trace,
                    ));
                }
                next_turn_request = Some(next_request);
            }
            None => {
                if let Err(error) = store
                    .finalize_turn(
                        turn_request.session_id.clone(),
                        turn_request.thread_id.clone(),
                        loop_result.usage,
                    )
                    .await
                {
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        task_continuation_decision_trace,
                    ));
                }

                events
                    .publish(AgentEvent::RunFinished {
                        text: loop_result.final_text.clone(),
                        usage: total_usage,
                    })
                    .await;

                return Ok(RunOutcome::Success(RunResult {
                    text: loop_result.final_text,
                    usage: total_usage,
                    iterations: total_iterations,
                    inflight_snapshot: task_inflight_snapshot,
                    continuation_decision_trace: task_continuation_decision_trace,
                }));
            }
        }
    }

    crate::error::RuntimeSnafu {
        message: "task loop exited without a continuation decision".to_string(),
        stage: "runner-task-loop".to_string(),
        inflight_snapshot: None,
    }
    .fail()
}

/// Executes one persisted turn using the supplied dependencies and runtime config.
async fn run_turn<M, S, E>(
    model: &M,
    store: &S,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    turn_request: TurnExecutionRequest,
) -> Result<super::LoopResult>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
    E: EventSink + 'static,
{
    let TurnExecutionRequest {
        request,
        system_prompt,
        next_tool_handle_sequence,
    } = turn_request;
    let mut history = store
        .load_messages(
            request.session_id.clone(),
            request.thread_id.clone(),
            config.recent_message_limit,
        )
        .await?;

    let user_message = Message::user(request.input.clone());
    store
        .begin_turn(
            request.session_id.clone(),
            request.thread_id.clone(),
            request.input.clone(),
            user_message.clone(),
        )
        .await?;
    history.push(user_message.clone());

    let loop_result = run_loop_turn(
        model,
        store,
        router,
        events,
        config,
        AgentLoopRequest {
            session_id: request.session_id.clone(),
            thread_id: request.thread_id.clone(),
            system_prompt,
            working_messages: history,
            next_tool_handle_sequence,
        },
    )
    .await;

    match loop_result {
        Ok(loop_result) => Ok(loop_result),
        Err(error) => Err(preserve_original_error_after_task_cleanup(store, &request, error).await),
    }
}

/// Packages one task-level failure so callers always receive the aggregated runtime snapshot.
fn build_task_failure_outcome(
    error: crate::Error,
    task_inflight_snapshot: &super::ToolCallRuntimeSnapshot,
    continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
) -> RunOutcome {
    let mut inflight_snapshot = task_inflight_snapshot.clone();
    let error_snapshot = match &error {
        crate::Error::Runtime {
            inflight_snapshot: Some(snapshot),
            ..
        }
        | crate::Error::Tool {
            inflight_snapshot: Some(snapshot),
            ..
        }
        | crate::Error::Cleanup {
            inflight_snapshot: Some(snapshot),
            ..
        } => Some(snapshot.clone()),
        _ => None,
    };
    if let Some(error_snapshot) = error_snapshot {
        inflight_snapshot.extend(error_snapshot);
    }

    RunOutcome::Failure(RunFailure {
        error,
        inflight_snapshot,
        continuation_decision_trace,
    })
}

/// Merges task-level trace entries with the current turn's trace entries without losing either side.
fn merge_continuation_traces(
    mut task_trace: Vec<TaskContinuationDecisionTraceEntry>,
    turn_trace: Vec<TaskContinuationDecisionTraceEntry>,
) -> Vec<TaskContinuationDecisionTraceEntry> {
    task_trace.extend(turn_trace);
    task_trace
}

/// Tries to discard the active turn after a post-loop task failure while preserving the primary error.
async fn preserve_original_error_after_task_cleanup<S>(
    store: &S,
    request: &RunRequest,
    original_error: crate::Error,
) -> crate::Error
where
    S: SessionStore + ?Sized,
{
    let inflight_snapshot = extract_inflight_snapshot(&original_error);
    match discard_active_turn_after_task_failure(store, request).await {
        Ok(()) => original_error,
        Err(cleanup_error) => crate::Error::Cleanup {
            source: Box::new(original_error),
            cleanup_error: Box::new(cleanup_error),
            stage: "runner-discard-active-turn".to_string(),
            inflight_snapshot,
        },
    }
}

/// Extracts any attached runtime snapshot so cleanup wrappers can preserve the original context.
fn extract_inflight_snapshot(error: &crate::Error) -> Option<super::ToolCallRuntimeSnapshot> {
    match error {
        crate::Error::Runtime {
            inflight_snapshot, ..
        }
        | crate::Error::Tool {
            inflight_snapshot, ..
        }
        | crate::Error::Cleanup {
            inflight_snapshot, ..
        } => inflight_snapshot.clone(),
        _ => None,
    }
}

/// Discards the still-active turn after a post-loop task failure so later runs can start cleanly.
async fn discard_active_turn_after_task_failure<S>(store: &S, request: &RunRequest) -> Result<()>
where
    S: SessionStore + ?Sized,
{
    store
        .discard_turn(request.session_id.clone(), request.thread_id.clone())
        .await
}

/// Decides whether the outer task loop should submit another turn after the current one.
async fn decide_task_continuation<S>(
    store: &S,
    request: &RunRequest,
    loop_result: &super::LoopResult,
    config: &AgentLoopConfig,
) -> Result<(TaskContinuation, Vec<TaskContinuationDecisionTraceEntry>)>
where
    S: SessionStore + ?Sized,
{
    let mut continuation = loop_result.requested_continuation.clone();
    let mut trace = loop_result.continuation_decision_trace.clone();

    if let Some(hook) = config.continuation_decision_hook.as_ref() {
        let hook_context = super::ContinuationHookContext {
            phase: super::ContinuationHookPhase::TurnCompleted,
            loop_result: loop_result.clone(),
            iteration: loop_result.iterations,
            tool_batch_summary: None,
            requested_continuation: continuation.clone(),
            inflight_snapshot: loop_result.inflight_snapshot.clone(),
        };
        match hook(&hook_context) {
            super::ContinuationHookDecision::Continue => {}
            super::ContinuationHookDecision::Request(requested_continuation) => {
                if continuation.is_none() {
                    continuation = Some(requested_continuation.clone());
                }
                trace.push(TaskContinuationDecisionTraceEntry {
                    stage: TaskContinuationDecisionStage::TurnCompletedHook,
                    decision: TaskContinuationDecisionKind::Request,
                    source: Some(TaskContinuationSource::from(&requested_continuation)),
                });
            }
            super::ContinuationHookDecision::Replace(replacement_continuation) => {
                continuation = Some(replacement_continuation.clone());
                trace.push(TaskContinuationDecisionTraceEntry {
                    stage: TaskContinuationDecisionStage::TurnCompletedHook,
                    decision: TaskContinuationDecisionKind::Replace,
                    source: Some(TaskContinuationSource::from(&replacement_continuation)),
                });
            }
        }
    } else if let Some(hook) = config.continuation_hook.as_ref() {
        let hook_context = super::ContinuationHookContext {
            phase: super::ContinuationHookPhase::TurnCompleted,
            loop_result: loop_result.clone(),
            iteration: loop_result.iterations,
            tool_batch_summary: None,
            requested_continuation: continuation.clone(),
            inflight_snapshot: loop_result.inflight_snapshot.clone(),
        };
        if let Some(requested_continuation) = hook(&hook_context) {
            if continuation.is_none() {
                continuation = Some(requested_continuation.clone());
            }
            trace.push(TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::TurnCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::from(&requested_continuation)),
            });
        }
    }

    if let Some(continuation) = continuation {
        return Ok((TaskContinuation::Continue(continuation), trace));
    }

    // Runtime-generated continuation requests run first so turn-internal logic
    // can explicitly request a follow-up before the task loop falls back to
    // queued session continuations.
    if let Some(resolver) = config.continuation_resolver.as_ref()
        && let Some(continuation) = resolver(loop_result)
    {
        trace.push(TaskContinuationDecisionTraceEntry {
            stage: TaskContinuationDecisionStage::Resolver,
            decision: TaskContinuationDecisionKind::Request,
            source: Some(TaskContinuationSource::from(&continuation)),
        });
        return Ok((TaskContinuation::Continue(continuation), trace));
    }

    // The task loop continues only when a follow-up input has been queued for
    // this thread. This mirrors Codex's outer task loop shape without yet
    // introducing more advanced interruption or resume sources.
    let continuation = store
        .take_continuation(request.session_id.clone(), request.thread_id.clone())
        .await?;
    Ok(match continuation {
        Some(continuation) => {
            trace.push(TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::SessionQueue,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::from(&continuation)),
            });
            (TaskContinuation::Continue(continuation), trace)
        }
        None => (TaskContinuation::Finish, trace),
    })
}

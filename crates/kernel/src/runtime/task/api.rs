use std::sync::Arc;

use llm::usage::Usage;

use crate::{
    Result,
    context::{SessionTaskContext, TurnContext},
    events::EventSink,
    model::AgentModel,
    runtime::{ToolCallRuntimeSnapshot, continuation::AgentLoopConfig},
    session::{SessionId, ThreadId},
    tools::router::ToolRouter,
};

use super::runner::run_task;

/// Shared dependencies that stay stable for one agent lifecycle.
pub struct AgentDeps<M, E> {
    pub model: Arc<M>,
    pub store: Arc<SessionTaskContext>,
    pub tools: Arc<ToolRouter>,
    pub events: Arc<E>,
}

impl<M, E> AgentDeps<M, E> {
    /// Builds the shared dependency bundle for an agent instance.
    pub fn new(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        tools: Arc<ToolRouter>,
        events: Arc<E>,
    ) -> Self {
        Self {
            model,
            store,
            tools,
            events,
        }
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
pub struct Agent<M, E> {
    context: TurnContext,
    config: AgentConfig,
    deps: AgentDeps<M, E>,
}

impl<M, E> Agent<M, E>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    /// Builds an agent bound to one session thread and dependency set.
    pub fn new(context: TurnContext, deps: AgentDeps<M, E>) -> Self {
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
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
    pub continuation_decision_trace: Vec<crate::events::TaskContinuationDecisionTraceEntry>,
}

/// Structured failure payload for callers that want runtime state without unpacking `Error`.
#[derive(Debug)]
pub struct RunFailure {
    pub error: crate::Error,
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
    pub continuation_decision_trace: Vec<crate::events::TaskContinuationDecisionTraceEntry>,
}

/// Structured turn outcome that exposes success and failure payloads symmetrically.
#[derive(Debug)]
pub enum RunOutcome {
    Success(RunResult),
    Failure(RunFailure),
}

pub struct AgentRunner<M, E> {
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    events: Arc<E>,
    config: AgentLoopConfig,
    system_prompt: Option<String>,
}

impl<M, E> AgentRunner<M, E>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    /// Builds a runner from the model, stores, registry, and event sink.
    pub fn new(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        router: Arc<ToolRouter>,
        events: Arc<E>,
    ) -> Self {
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

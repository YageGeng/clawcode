use std::sync::Arc;

use llm::usage::Usage;

use crate::{
    Result,
    context::SessionTaskContext,
    events::EventSink,
    input::{UserInput, user_inputs_display_text},
    model::AgentModel,
    runtime::{ToolCallRuntimeSnapshot, continuation::AgentLoopConfig},
    session::{SessionId, ThreadId},
    tools::router::ToolRouter,
};

use super::runner::run_task;

/// Shared dependencies that stay stable for one thread runtime lifecycle.
pub struct ThreadRuntimeDeps<M, E> {
    pub model: Arc<M>,
    pub store: Arc<SessionTaskContext>,
    pub tools: Arc<ToolRouter>,
    pub events: Arc<E>,
}

impl<M, E> ThreadRuntimeDeps<M, E> {
    /// Builds the shared dependency bundle for a thread runtime instance.
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

/// Thread-level defaults that apply to submissions routed through one handle.
#[derive(Debug, Clone, Default)]
pub struct ThreadConfig {
    pub system_prompt: Option<String>,
}

/// Lightweight handle that identifies one session/thread binding.
#[derive(Debug, Clone)]
pub struct ThreadHandle {
    session_id: SessionId,
    thread_id: ThreadId,
    config: ThreadConfig,
}

impl ThreadHandle {
    /// Builds a handle for one session/thread pair.
    pub fn new(session_id: SessionId, thread_id: ThreadId) -> Self {
        Self {
            session_id,
            thread_id,
            config: ThreadConfig::default(),
        }
    }

    /// Sets the default system prompt applied to submissions on this thread.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(system_prompt.into());
        self
    }

    /// Returns the session identifier for this thread handle.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the thread identifier for this thread handle.
    pub fn thread_id(&self) -> &ThreadId {
        &self.thread_id
    }

    /// Returns the optional default system prompt carried by this handle.
    pub fn system_prompt(&self) -> Option<&String> {
        self.config.system_prompt.as_ref()
    }
}

/// One thread submission with optional prompt overrides.
#[derive(Debug, Clone)]
pub struct ThreadRunRequest {
    /// Structured user inputs submitted for this turn.
    pub inputs: Vec<UserInput>,
    /// Optional system prompt override applied only to this submission.
    pub system_prompt_override: Option<String>,
}

impl ThreadRunRequest {
    /// Builds a single-turn request for an existing thread handle.
    pub fn new(input: impl Into<String>) -> Self {
        Self::from_inputs(vec![UserInput::text(input)])
    }

    /// Builds a single-turn request from structured user inputs.
    pub fn from_inputs(inputs: Vec<UserInput>) -> Self {
        Self {
            inputs,
            system_prompt_override: None,
        }
    }
}

/// Runtime entrypoint that executes work against supplied thread handles.
pub struct ThreadRuntime<M, E> {
    deps: ThreadRuntimeDeps<M, E>,
    config: AgentLoopConfig,
}

impl<M, E> ThreadRuntime<M, E>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    /// Builds a runtime from the model, session context, tool router, and event sink.
    pub fn new(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        tools: Arc<ToolRouter>,
        events: Arc<E>,
    ) -> Self {
        Self {
            deps: ThreadRuntimeDeps::new(model, store, tools, events),
            config: AgentLoopConfig::default(),
        }
    }

    /// Overrides the default runtime loop configuration for this thread runtime.
    pub fn with_config(mut self, config: AgentLoopConfig) -> Self {
        self.config = config;
        self
    }

    /// Runs one input turn against the supplied thread and returns the final result.
    pub async fn run(&self, thread: &ThreadHandle, request: ThreadRunRequest) -> Result<RunResult> {
        match self.run_outcome(thread, request).await? {
            RunOutcome::Success(result) => Ok(result),
            RunOutcome::Failure(failure) => Err(failure.error),
        }
    }

    /// Runs one input turn and returns a structured success or failure payload.
    pub async fn run_outcome(
        &self,
        thread: &ThreadHandle,
        request: ThreadRunRequest,
    ) -> Result<RunOutcome> {
        let run_request = RunRequest::from_inputs(
            thread.session_id().clone(),
            thread.thread_id().clone(),
            request.inputs,
        );
        let system_prompt = request
            .system_prompt_override
            .or_else(|| thread.system_prompt().cloned());

        run_task(
            self.deps.model.as_ref(),
            self.deps.store.as_ref(),
            self.deps.tools.as_ref(),
            self.deps.events.as_ref(),
            &self.config,
            system_prompt,
            run_request,
        )
        .await
    }

    /// Runs a prebuilt runtime request directly when the caller already owns the ids.
    pub async fn run_request(&self, request: RunRequest) -> Result<RunResult> {
        match self.run_outcome_request(request).await? {
            RunOutcome::Success(result) => Ok(result),
            RunOutcome::Failure(failure) => Err(failure.error),
        }
    }

    /// Runs a prebuilt runtime request and returns a structured success or failure payload.
    pub async fn run_outcome_request(&self, request: RunRequest) -> Result<RunOutcome> {
        run_task(
            self.deps.model.as_ref(),
            self.deps.store.as_ref(),
            self.deps.tools.as_ref(),
            self.deps.events.as_ref(),
            &self.config,
            None,
            request,
        )
        .await
    }
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Session identifier receiving this run.
    pub session_id: SessionId,
    /// Thread identifier receiving this run.
    pub thread_id: ThreadId,
    /// Structured user inputs submitted for this run.
    pub inputs: Vec<UserInput>,
    /// Durable display string derived from structured inputs for events and history.
    pub display_input: String,
}

impl RunRequest {
    /// Builds a runtime request from explicit identifiers and user input.
    pub fn new(session_id: SessionId, thread_id: ThreadId, input: impl Into<String>) -> Self {
        Self::from_inputs(session_id, thread_id, vec![UserInput::text(input)])
    }

    /// Builds a runtime request from explicit identifiers and structured inputs.
    pub fn from_inputs(session_id: SessionId, thread_id: ThreadId, inputs: Vec<UserInput>) -> Self {
        let display_input = user_inputs_display_text(&inputs);
        Self {
            session_id,
            thread_id,
            inputs,
            display_input,
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

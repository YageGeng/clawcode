use std::sync::Arc;

use llm::{completion::Message, usage::Usage};
use uuid::Uuid;

use crate::{
    Result,
    events::{AgentEvent, EventSink},
    model::AgentModel,
    session::{SessionId, SessionStore, ThreadId, Turn},
    tools::registry::ToolRegistry,
};

use super::{AgentLoopConfig, AgentLoopRequest, run_agent_loop};

/// Shared dependencies that stay stable for one agent lifecycle.
pub struct AgentDeps<M, S, E> {
    pub model: Arc<M>,
    pub store: Arc<S>,
    pub tools: Arc<ToolRegistry>,
    pub events: Arc<E>,
}

impl<M, S, E> AgentDeps<M, S, E> {
    /// Builds the shared dependency bundle for an agent instance.
    pub fn new(model: Arc<M>, store: Arc<S>, tools: Arc<ToolRegistry>, events: Arc<E>) -> Self {
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
        let run_request = RunRequest::new(
            self.context.session_id.clone(),
            self.context.thread_id.clone(),
            request.input,
        );
        let system_prompt = request
            .system_prompt_override
            .or_else(|| self.config.system_prompt.clone());

        run_turn(
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
}

pub struct AgentRunner<M, S, E> {
    model: Arc<M>,
    store: Arc<S>,
    registry: Arc<ToolRegistry>,
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
    pub fn new(model: Arc<M>, store: Arc<S>, registry: Arc<ToolRegistry>, events: Arc<E>) -> Self {
        Self {
            model,
            store,
            registry,
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
        run_turn(
            self.model.as_ref(),
            self.store.as_ref(),
            self.registry.as_ref(),
            self.events.as_ref(),
            &self.config,
            self.system_prompt.clone(),
            request,
        )
        .await
    }
}

/// Executes one persisted turn using the supplied dependencies and runtime config.
async fn run_turn<M, S, E>(
    model: &M,
    store: &S,
    registry: &ToolRegistry,
    events: &E,
    config: &AgentLoopConfig,
    system_prompt: Option<String>,
    request: RunRequest,
) -> Result<RunResult>
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

    let mut history = store
        .load_messages(
            request.session_id.clone(),
            request.thread_id.clone(),
            config.recent_message_limit,
        )
        .await?;

    let user_message = Message::user(request.input.clone());
    history.push(user_message.clone());

    let loop_result = run_agent_loop(
        model,
        registry,
        events,
        config,
        AgentLoopRequest {
            session_id: request.session_id.clone(),
            thread_id: request.thread_id.clone(),
            system_prompt,
            working_messages: history,
        },
    )
    .await?;

    let mut transcript = vec![user_message];
    transcript.extend(loop_result.new_messages.clone());
    store
        .append_turn(
            request.session_id,
            request.thread_id,
            Turn::new(request.input, transcript, loop_result.usage),
        )
        .await?;

    events
        .publish(AgentEvent::RunFinished {
            text: loop_result.final_text.clone(),
            usage: loop_result.usage,
        })
        .await;

    Ok(RunResult {
        text: loop_result.final_text,
        usage: loop_result.usage,
        iterations: loop_result.iterations,
    })
}

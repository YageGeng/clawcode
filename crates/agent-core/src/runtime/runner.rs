use std::sync::Arc;

use llm::{completion::Message, usage::Usage};

use crate::{
    Result,
    events::{AgentEvent, EventSink},
    model::AgentModel,
    session::{SessionId, SessionStore, ThreadId, Turn},
    tools::registry::ToolRegistry,
};

use super::{AgentLoopConfig, AgentLoopRequest, run_agent_loop};

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
        self.events
            .publish(AgentEvent::RunStarted {
                session_id: request.session_id.to_string(),
                thread_id: request.thread_id.to_string(),
                input: request.input.clone(),
            })
            .await;

        let mut history = self
            .store
            .load_messages(
                request.session_id.clone(),
                request.thread_id.clone(),
                self.config.recent_message_limit,
            )
            .await?;

        let user_message = Message::user(request.input.clone());
        history.push(user_message.clone());

        let loop_result = run_agent_loop(
            self.model.as_ref(),
            self.registry.as_ref(),
            self.events.as_ref(),
            &self.config,
            AgentLoopRequest {
                session_id: request.session_id.clone(),
                thread_id: request.thread_id.clone(),
                system_prompt: self.system_prompt.clone(),
                working_messages: history,
            },
        )
        .await?;

        let mut transcript = vec![user_message];
        transcript.extend(loop_result.new_messages.clone());
        self.store
            .append_turn(
                request.session_id,
                request.thread_id,
                Turn::new(request.input, transcript, loop_result.usage),
            )
            .await?;

        self.events
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
}

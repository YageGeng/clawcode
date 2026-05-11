//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

pub mod context;
pub mod session;
pub mod tool;
pub(crate) mod turn;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use config::ConfigHandle;
use protocol::{
    AgentKernel, AgentPath, Event, KernelError, ModelInfo, Op, SessionCreated, SessionId,
    SessionInfo, SessionListPage, SessionMode,
};
use provider::factory::LlmFactory;

use crate::context::InMemoryContext;
use crate::session::{Thread, event_stream, spawn_thread};
use crate::tool::ToolRegistry;

/// Central kernel struct implementing [`AgentKernel`].
pub struct Kernel {
    /// Shared LLM factory for dispatching provider/model requests.
    llm_factory: Arc<LlmFactory>,
    /// Configuration handle for reading provider/model settings.
    config: ConfigHandle,
    /// Registered tools available to every session.
    tools: Arc<ToolRegistry>,
    /// Active sessions keyed by [`SessionId`].
    sessions: Mutex<HashMap<SessionId, Thread>>,
}

impl Kernel {
    /// Create a new kernel instance with the given LLM factory,
    /// config, and tool registry.
    #[must_use]
    pub fn new(
        llm_factory: Arc<LlmFactory>,
        config: ConfigHandle,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            llm_factory,
            config,
            tools,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the default LLM handle from the `active_model` config value.
    ///
    /// The config value is in `provider_id/model_id` format (e.g. "deepseek/deepseek-v4-flash").
    fn default_llm(&self) -> Option<provider::factory::ArcLlm> {
        let cfg = self.config.current();
        let (provider_id, model_id) = cfg.active_model.split_once('/')?;
        self.llm_factory.get(provider_id, model_id)
    }

    /// Build available session modes.
    fn build_modes(&self) -> Vec<SessionMode> {
        vec![
            SessionMode {
                id: "read-only".to_string(),
                name: "Read Only".to_string(),
                description: Some("Agent cannot modify files".to_string()),
            },
            SessionMode {
                id: "auto".to_string(),
                name: "Auto".to_string(),
                description: Some("Agent asks for approval before making changes".to_string()),
            },
            SessionMode {
                id: "full-access".to_string(),
                name: "Full Access".to_string(),
                description: Some("Agent can modify files without approval".to_string()),
            },
        ]
    }

    /// Build available models from LLM configuration.
    fn build_models(&self) -> Vec<ModelInfo> {
        let cfg = self.config.current();
        cfg.providers
            .iter()
            .flat_map(|p| {
                p.models.iter().map(|m| {
                    ModelInfo::builder()
                        .id(format!("{}/{}", p.id.as_str(), m.id))
                        .display_name(m.display_name.clone().unwrap_or_else(|| m.id.clone()))
                        .description(None)
                        .context_tokens(m.context_tokens)
                        .max_output_tokens(m.max_output_tokens)
                        .build()
                })
            })
            .collect()
    }
}

#[async_trait]
impl AgentKernel for Kernel {
    async fn new_session(&self, cwd: PathBuf) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let llm = self
            .default_llm()
            .ok_or_else(|| KernelError::Internal(anyhow::anyhow!("no LLM configured")))?;

        let handle = spawn_thread(
            session_id.clone(),
            cwd.clone(),
            llm,
            self.tools.clone(),
            Box::new(InMemoryContext::new()),
        );

        let modes = self.build_modes();
        let models = self.build_models();

        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), handle);

        Ok(SessionCreated {
            session_id,
            modes,
            models,
        })
    }

    async fn load_session(&self, session_id: &SessionId) -> Result<SessionCreated, KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(SessionCreated {
            session_id: session_id.clone(),
            modes: self.build_modes(),
            models: self.build_models(),
        })
    }

    async fn list_sessions(
        &self,
        _cwd: Option<&Path>,
        _cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError> {
        let sessions: Vec<SessionInfo> = self
            .sessions
            .lock()
            .await
            .keys()
            .map(|id| {
                SessionInfo::builder()
                    .session_id(id.clone())
                    .cwd(PathBuf::from("."))
                    .build()
            })
            .collect();

        Ok(SessionListPage {
            sessions,
            next_cursor: None,
        })
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>, KernelError>
    {
        let handle = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?
            .clone();

        // Send the prompt to the background task
        let _ = handle.tx_op.send(Op::Prompt {
            session_id: session_id.clone(),
            text,
        });

        // Take the receiver from the handle and build stream
        let rx_event = handle.take_rx().await;
        Ok(event_stream(rx_event, handle.cancel_tx.subscribe()))
    }

    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
        match self.sessions.lock().await.get(session_id) {
            Some(handle) => {
                let _ = handle.cancel_tx.send(true);
                Ok(())
            }
            None => Err(KernelError::SessionNotFound(session_id.clone())),
        }
    }

    async fn set_mode(&self, session_id: &SessionId, _mode: &str) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn set_model(
        &self,
        session_id: &SessionId,
        _provider_id: &str,
        _model_id: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError> {
        let handle = self
            .sessions
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        // Signal close to the background task
        let _ = handle.tx_op.send(Op::CloseSession {
            session_id: session_id.clone(),
        });
        Ok(())
    }

    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        _agent_path: AgentPath,
        _role: &str,
        _prompt: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(parent_session) {
            return Err(KernelError::SessionNotFound(parent_session.clone()));
        }
        // Sub-agent spawning will be implemented in a subsequent plan.
        Ok(())
    }

    fn available_modes(&self) -> Vec<SessionMode> {
        self.build_modes()
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        self.build_models()
    }
}

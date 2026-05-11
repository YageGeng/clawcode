//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, Stream};
use tokio::sync::Mutex;

use config::ConfigHandle;
use protocol::{
    AgentKernel, AgentPath, Event, KernelError, ModelInfo, SessionCreated, SessionId, SessionInfo,
    SessionListPage, SessionMode, StopReason,
};
use provider::factory::LlmFactory;

/// Central kernel struct implementing [`AgentKernel`].
pub struct Kernel {
    /// Shared LLM factory for dispatching provider/model requests.
    #[allow(dead_code)]
    llm_factory: Arc<LlmFactory>,
    /// Configuration handle for reading provider/model settings.
    config: ConfigHandle,
    /// Active sessions keyed by [`SessionId`].
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
}

/// Per-session runtime handle.
struct SessionHandle {
    /// Working directory for the session.
    cwd: PathBuf,
    /// Token used to signal cancellation.
    cancel_token: tokio::sync::watch::Sender<bool>,
}

impl Kernel {
    /// Create a new kernel instance with the given LLM factory and config.
    #[must_use]
    pub fn new(llm_factory: Arc<LlmFactory>, config: ConfigHandle) -> Self {
        Self {
            llm_factory,
            config,
            sessions: Mutex::new(HashMap::new()),
        }
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
        let (cancel_tx, _) = tokio::sync::watch::channel(false);

        self.sessions.lock().await.insert(
            session_id.clone(),
            SessionHandle {
                cwd: cwd.clone(),
                cancel_token: cancel_tx,
            },
        );

        Ok(SessionCreated {
            session_id,
            modes: self.build_modes(),
            models: self.build_models(),
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
            .iter()
            .map(|(id, handle)| {
                SessionInfo::builder()
                    .session_id(id.clone())
                    .cwd(handle.cwd.clone())
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
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }

        // Minimal stub: echoes the input and completes.
        // Full LLM integration will be added in a subsequent plan.
        let sid = session_id.clone();
        let events: Vec<Result<Event, KernelError>> = vec![
            Ok(Event::AgentMessageChunk {
                session_id: sid.clone(),
                text: format!("Echo: {text}"),
            }),
            Ok(Event::TurnComplete {
                session_id: sid,
                stop_reason: StopReason::EndTurn,
            }),
        ];

        Ok(Box::pin(stream::iter(events)))
    }

    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
        match self.sessions.lock().await.get(session_id) {
            Some(handle) => {
                let _ = handle.cancel_token.send(true);
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
        if self.sessions.lock().await.remove(session_id).is_none() {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
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

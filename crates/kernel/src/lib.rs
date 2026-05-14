//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

pub mod agent;
pub mod approval;
pub mod context;
pub(crate) mod prompt;
pub mod session;
// tool module moved to tools crate
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
    AgentKernel, AgentPath, Event, KernelError, ModelInfo, Op, ReviewDecision, SessionCreated,
    SessionId, SessionInfo, SessionListPage, SessionMode,
};
use provider::factory::LlmFactory;

use crate::agent::adapter::AgentControlAdapter;
use crate::agent::control::AgentControl;
use crate::approval::ApprovalPolicy;
use crate::context::InMemoryContext;
use crate::session::{Thread, event_stream, spawn_thread};
use tools::ToolRegistry;

/// Central kernel struct implementing [`AgentKernel`].
///
/// Construct via [`Kernel::builder()`] to satisfy the typed-builder
/// requirement for structs with more than 3 fields.
#[derive(typed_builder::TypedBuilder)]
pub struct Kernel {
    /// Shared LLM factory for dispatching provider/model requests.
    pub llm_factory: Arc<LlmFactory>,
    /// Configuration handle for reading provider/model settings.
    pub config: ConfigHandle,
    /// Registered tools available to every session.
    pub tools: Arc<ToolRegistry>,
    #[builder(default)]
    sessions: Mutex<HashMap<SessionId, Thread>>,
    /// Shared agent control for multi-agent operations across all sessions.
    /// Constructed from the same llm_factory, config, and tools passed to
    /// the builder — must not be `#[builder(default)]`.
    pub agent_control: Arc<AgentControl>,
}

impl Kernel {
    /// Create a new kernel, constructing `AgentControl` internally from
    /// the provided config and tools. Use [`Kernel::builder()`] if you
    /// need to pre-construct the `AgentControl` separately.
    #[must_use]
    pub fn new(
        llm_factory: Arc<LlmFactory>,
        config: ConfigHandle,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        let cfg = config.current();
        let agent_control = AgentControl::new(
            Arc::clone(&llm_factory),
            config.clone(),
            Arc::clone(&tools),
            cfg.multi_agent.clone(),
        );

        Kernel::builder()
            .llm_factory(llm_factory)
            .config(config)
            .tools(tools)
            .agent_control(agent_control)
            .build()
    }

    /// Register agent management tools using this kernel's `AgentControl`.
    ///
    /// Must be called after construction, before any sessions use the tools.
    /// Internally creates an [`AgentControlAdapter`] and registers it with
    /// the tool registry.
    pub fn register_agent_tools(&self) {
        let adapter = Arc::new(AgentControlAdapter::new(Arc::clone(&self.agent_control)));
        self.tools.register_agent_tools(adapter);
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

        // Use the kernel-wide AgentControl shared across all sessions.
        let agent_ctrl = Arc::clone(&self.agent_control);
        // Register the root thread so it can be the parent of sub-agents.
        agent_ctrl.registry.register_root_thread(session_id.clone());

        let app_cfg = self.config.current();
        let approval = Arc::new(ApprovalPolicy::new(app_cfg.approval));
        let handle = spawn_thread(
            session_id.clone(),
            cwd.clone(),
            llm,
            Arc::clone(&self.tools),
            Box::new(InMemoryContext::new()),
            AgentPath::root(),
            Some(Arc::clone(&agent_ctrl)),
            approval,
        );

        // Register root thread mailbox for inter-agent message routing.
        let sid_for_ctrl = session_id.clone();
        let mb = handle.mailbox.clone();
        tokio::spawn(async move {
            agent_ctrl.register_mailbox(sid_for_ctrl, mb).await;
        });

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
        // Extract what we need without cloning the whole Thread
        let (tx_op, rx_event, cancel_rx) = {
            let sessions = self.sessions.lock().await;
            let h = sessions
                .get(session_id)
                .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
            (h.tx_op.clone(), h.take_rx().await, h.cancel_tx.subscribe())
        };

        let _ = tx_op.send(Op::Prompt {
            session_id: session_id.clone(),
            text,
            system: None,
        });

        Ok(event_stream(rx_event, cancel_rx))
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

    async fn resolve_approval(
        &self,
        session_id: &SessionId,
        call_id: &str,
        decision: ReviewDecision,
    ) -> Result<(), KernelError> {
        // Clone the Arc so we can drop the sessions lock before awaiting
        let pending = {
            self.sessions
                .lock()
                .await
                .get(session_id)
                .map(|h| Arc::clone(&h.pending_approvals))
                .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?
        };

        let tx = pending.lock().await.remove(call_id).ok_or_else(|| {
            KernelError::Internal(anyhow::anyhow!(
                "approval request not found for tool call {call_id}"
            ))
        })?;

        let _ = tx.send(decision);

        Ok(())
    }
}

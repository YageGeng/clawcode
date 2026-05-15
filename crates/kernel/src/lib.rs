//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

pub mod agent;
pub mod approval;
pub mod context;
pub(crate) mod prompt;
pub mod session;
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
use crate::prompt::environment::EnvironmentInfo;
use crate::prompt::{Instructions, SystemPrompt};
use crate::session::{Thread, event_stream, spawn_thread};
use store::{
    AgentEdgeStatusRecord, CreateSessionParams, FileSessionStore, SessionRecorder, SessionStore,
};
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
    /// Shared tool registry owned by this kernel.
    pub tools: Arc<ToolRegistry>,
    #[builder(default)]
    sessions: Mutex<HashMap<SessionId, Thread>>,
    /// Session persistence store.
    #[builder(default = Arc::new(FileSessionStore::new_default()))]
    session_store: Arc<dyn SessionStore>,
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
        let store: Arc<dyn SessionStore> = Arc::new(FileSessionStore::new(
            cfg.session_persistence.enabled,
            cfg.session_persistence.data_home.as_deref(),
        ));
        let agent_control = AgentControl::new(
            Arc::clone(&llm_factory),
            config.clone(),
            Arc::clone(&tools),
            cfg.multi_agent.clone(),
            Some(Arc::clone(&store) as Arc<dyn SessionStore>),
        );
        Kernel::builder()
            .llm_factory(llm_factory)
            .config(config)
            .tools(tools)
            .agent_control(agent_control)
            .session_store(store)
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

    /// Return the active provider/model pair from configuration.
    fn active_provider_model(&self) -> Option<(String, String)> {
        let cfg = self.config.current();
        let (provider_id, model_id) = cfg.active_model.split_once('/')?;
        Some((provider_id.to_string(), model_id.to_string()))
    }

    /// Render the base prompt snapshot stored in the session metadata record.
    fn render_base_system_prompt(
        &self,
        cwd: &Path,
        model_id: &str,
        app_cfg: &config::AppConfig,
    ) -> String {
        let skill_registry = skills::SkillRegistry::discover(cwd, &app_cfg.skills);
        let skills_xml = if app_cfg.skills.include_instructions {
            skill_registry.render_catalog()
        } else {
            None
        };
        SystemPrompt::builder()
            .environment(EnvironmentInfo::capture(
                model_id.to_string(),
                cwd.to_path_buf(),
            ))
            .instructions(Instructions::load(cwd))
            .skills_xml(skills_xml)
            .build()
            .render()
    }

    /// Spawn a live thread from already constructed context and persistence state.
    fn spawn_live_thread(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
        context: Box<dyn crate::context::ContextManager>,
        recorder: Option<Arc<dyn SessionRecorder>>,
        app_cfg: Arc<config::AppConfig>,
    ) -> Result<Thread, KernelError> {
        let llm = self
            .default_llm()
            .ok_or_else(|| KernelError::Internal(anyhow::anyhow!("no LLM configured")))?;
        let approval = Arc::new(ApprovalPolicy::new(app_cfg.approval));
        Ok(spawn_thread(
            session_id,
            cwd,
            llm,
            Arc::clone(&self.tools),
            context,
            AgentPath::root(),
            Some(Arc::clone(&self.agent_control)),
            approval,
            app_cfg,
            recorder,
        ))
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

    /// Recursively restore subagents from persisted agent edges.
    async fn restore_subagent_tree(
        &self,
        edges: &[store::AgentEdgeRecord],
        agent_control: &Arc<AgentControl>,
        app_cfg: &Arc<config::AppConfig>,
    ) {
        for edge in edges
            .iter()
            .filter(|e| e.status == AgentEdgeStatusRecord::Open)
        {
            if agent_control
                .registry
                .agent_id_for_path(&edge.child_agent_path)
                .is_some()
            {
                continue;
            }
            let Some((child_replayed, child_recorder)) = self
                .session_store
                .load_session(&edge.child_session_id)
                .unwrap_or(None)
            else {
                tracing::warn!(child_id = %edge.child_session_id, "failed to load subagent session");
                continue;
            };
            let child_recorder: Arc<dyn SessionRecorder> = Arc::from(child_recorder);
            let handle = match self.spawn_live_thread(
                edge.child_session_id.clone(),
                child_replayed.meta.cwd.clone(),
                Box::new(InMemoryContext::from_messages(
                    child_replayed.messages.clone(),
                )),
                Some(Arc::clone(&child_recorder)),
                Arc::clone(app_cfg),
            ) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(child_id = %edge.child_session_id, error = %e, "failed to restore subagent thread");
                    continue;
                }
            };
            if let Err(e) = agent_control.registry.restore_agent(
                edge.child_session_id.clone(),
                edge.child_agent_path.clone(),
                child_replayed.meta.agent_nickname.clone(),
                child_replayed.meta.agent_role.clone(),
                Some(edge.parent_session_id.clone()),
            ) {
                tracing::warn!(child_id = %edge.child_session_id, error = %e, "failed to register restored subagent");
                continue;
            }
            let sid = edge.child_session_id.clone();
            let mb = handle.mailbox.clone();
            let ag = Arc::clone(agent_control);
            let rec = Arc::clone(&child_recorder);
            tokio::spawn(async move {
                ag.register_mailbox(sid.clone(), mb).await;
                ag.register_recorder(sid, rec).await;
            });
            self.sessions
                .lock()
                .await
                .insert(edge.child_session_id.clone(), handle);
            Box::pin(self.restore_subagent_tree(
                &child_replayed.agent_edges,
                agent_control,
                app_cfg,
            ))
            .await;
        }
    }
}

#[async_trait]
impl AgentKernel for Kernel {
    async fn new_session(&self, cwd: PathBuf) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let (provider_id, model_id) = self.active_provider_model().ok_or_else(|| {
            KernelError::Internal(anyhow::anyhow!("active model must be provider/model"))
        })?;

        // Use the kernel-wide AgentControl shared across all sessions.
        let agent_ctrl = Arc::clone(&self.agent_control);

        let app_cfg = self.config.current();
        let base_system_prompt = self.render_base_system_prompt(&cwd, &model_id, &app_cfg);
        let recorder = self
            .session_store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(cwd.clone())
                    .provider_id(provider_id)
                    .model_id(model_id)
                    .base_system_prompt(base_system_prompt)
                    .build(),
            )
            .await
            .map_err(|error| KernelError::Internal(error.into()))?;
        let recorder: Option<Arc<dyn SessionRecorder>> = recorder.map(Arc::from);
        // Register root recorder so subagent spawns can write AgentEdge to parent.
        if let Some(ref rec) = recorder {
            agent_ctrl
                .register_recorder(session_id.clone(), Arc::clone(rec))
                .await;
        }
        let handle = self.spawn_live_thread(
            session_id.clone(),
            cwd.clone(),
            Box::new(InMemoryContext::new()),
            recorder,
            app_cfg,
        )?;

        // Register root only after session creation succeeds, avoiding stale registry entries.
        agent_ctrl.registry.register_root_thread(session_id.clone());

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

        Ok(SessionCreated::builder()
            .session_id(session_id)
            .modes(modes)
            .models(models)
            .build())
    }

    async fn load_session(&self, session_id: &SessionId) -> Result<SessionCreated, KernelError> {
        if self.sessions.lock().await.contains_key(session_id) {
            return Ok(SessionCreated::builder()
                .session_id(session_id.clone())
                .modes(self.build_modes())
                .models(self.build_models())
                .build());
        }
        let Some((replayed, recorder)) = self
            .session_store
            .load_session(session_id)
            .map_err(|error| KernelError::Internal(error.into()))?
        else {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        };
        let app_cfg = self.config.current();
        let history = replayed.messages;
        let agent_edges = replayed.agent_edges;
        let recorder: Arc<dyn SessionRecorder> = Arc::from(recorder);
        let handle = self.spawn_live_thread(
            session_id.clone(),
            replayed.meta.cwd.clone(),
            Box::new(InMemoryContext::from_messages(history.clone())),
            Some(Arc::clone(&recorder)),
            Arc::clone(&app_cfg),
        )?;
        let agent_ctrl = Arc::clone(&self.agent_control);
        agent_ctrl.registry.register_root_thread(session_id.clone());
        agent_ctrl
            .register_recorder(session_id.clone(), recorder)
            .await;
        let sid_for_ctrl = session_id.clone();
        let mb = handle.mailbox.clone();
        let ag = Arc::clone(&agent_ctrl);
        tokio::spawn(async move {
            ag.register_mailbox(sid_for_ctrl, mb).await;
        });
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), handle);
        self.restore_subagent_tree(&agent_edges, &agent_ctrl, &app_cfg)
            .await;
        Ok(SessionCreated::builder()
            .session_id(session_id.clone())
            .modes(self.build_modes())
            .models(self.build_models())
            .history(history)
            .build())
    }

    async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        _cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError> {
        let mut sessions: Vec<SessionInfo> = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|thread| cwd.is_none_or(|cwd| thread.cwd == cwd))
            .map(|thread| {
                SessionInfo::builder()
                    .session_id(thread.session_id.clone())
                    .cwd(thread.cwd.clone())
                    .build()
            })
            .collect();
        let live_ids: std::collections::HashSet<SessionId> = sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect();
        for session in self
            .session_store
            .list_sessions(cwd)
            .map_err(|error| KernelError::Internal(error.into()))?
        {
            if !live_ids.contains(&session.session_id) {
                sessions.push(session);
            }
        }

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
        let handle = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?
        };

        self.session_store
            .close_session(session_id, handle.recorder.as_deref())
            .await
            .map_err(|error| KernelError::Internal(error.into()))?;

        let removed = self.sessions.lock().await.remove(session_id);
        if let Some(handle) = removed {
            // Signal close only after persistence succeeds so close can be retried on failure.
            let _ = handle.tx_op.send(Op::CloseSession {
                session_id: session_id.clone(),
            });
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

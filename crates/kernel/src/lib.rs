//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

pub mod agent;
pub mod approval;
pub mod context;
pub(crate) mod prompt;
pub mod session;
pub(crate) mod thread_manager;
pub(crate) mod turn;

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use config::ConfigHandle;
use futures::Stream;
use protocol::{
    AgentKernel, AgentPath, Event, KernelError, ModelInfo, Op, ReviewDecision, SessionCreated,
    SessionId, SessionInfo, SessionLaunchOptions, SessionListPage, SessionMode,
};
use provider::factory::LlmFactory;

use crate::agent::adapter::AgentControlAdapter;
use crate::agent::control::AgentControl;
use crate::approval::ApprovalPolicy;
use crate::context::InMemoryContext;
use crate::prompt::environment::EnvironmentInfo;
use crate::prompt::{Instructions, SystemPrompt};
use crate::session::{Thread, event_stream};
use crate::thread_manager::{LoadThreadParams, SpawnThreadParams, ThreadManager};
use store::{
    AgentEdgeStatus, AgentGraphStore, CreateSessionParams, FileSessionStore, SessionRecorder,
    SessionStore,
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
    /// Live thread lifecycle manager.
    pub(crate) thread_manager: Arc<ThreadManager>,
    /// Session persistence store.
    #[builder(default = Arc::new(FileSessionStore::new_default()))]
    session_store: Arc<dyn SessionStore>,
    /// Durable parent-child graph store.
    #[builder(default = Arc::new(FileSessionStore::new_default()))]
    agent_graph_store: Arc<dyn AgentGraphStore>,
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
        let file_store = Arc::new(FileSessionStore::new(
            cfg.session_persistence.enabled,
            cfg.session_persistence.data_home.as_deref(),
        ));
        let store: Arc<dyn SessionStore> = Arc::clone(&file_store) as Arc<dyn SessionStore>;
        let graph_store: Arc<dyn AgentGraphStore> =
            Arc::clone(&file_store) as Arc<dyn AgentGraphStore>;
        let thread_manager = Arc::new(ThreadManager::new());
        let agent_control = AgentControl::new(
            Arc::clone(&llm_factory),
            config.clone(),
            Arc::clone(&tools),
            cfg.multi_agent.clone(),
            Arc::clone(&thread_manager),
            Some(Arc::clone(&store) as Arc<dyn SessionStore>),
            Some(Arc::clone(&graph_store)),
        );
        Kernel::builder()
            .llm_factory(llm_factory)
            .config(config)
            .tools(tools)
            .agent_control(agent_control)
            .session_store(store)
            .agent_graph_store(Arc::clone(&graph_store))
            .thread_manager(thread_manager)
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
    async fn spawn_live_thread(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
        context: Box<dyn crate::context::ContextManager>,
        recorder: Option<Arc<dyn SessionRecorder>>,
        app_cfg: Arc<config::AppConfig>,
    ) -> Result<Thread, KernelError> {
        let llm = self.default_llm().ok_or_else(|| {
            let active = &self.config.current().active_model;
            KernelError::Internal(anyhow::anyhow!(
                "no provider found for active_model '{active}'; \
                 add a [[providers]] section to your config file"
            ))
        })?;
        let approval = Arc::new(ApprovalPolicy::new(app_cfg.approval));
        let builder = SpawnThreadParams::builder()
            .session_id(session_id)
            .cwd(cwd)
            .llm(llm)
            .tools(Arc::clone(&self.tools))
            .context(context)
            .agent_path(AgentPath::root())
            .agent_control(Arc::clone(&self.agent_control))
            .approval(approval)
            .app_config(app_cfg);
        let params = if let Some(recorder) = recorder {
            builder.recorder(recorder).build()
        } else {
            builder.build()
        };
        self.thread_manager.spawn_thread(params).await
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
        let active_model = cfg.active_model.clone();
        let mut models = cfg
            .providers
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
            .collect::<Vec<_>>();

        if let Some(active_index) = models.iter().position(|model| model.id == active_model) {
            // ACP marks the first model as current, so keep config.active_model first
            // while preserving the configured order of all other models.
            let active = models.remove(active_index);
            models.insert(0, active);
        }

        models
    }

    /// Register frontend-injected MCP servers for a live thread and refresh exposed tools.
    async fn register_external_mcp_servers_for_thread(
        &self,
        thread: &Thread,
        external_mcp_servers: Vec<mcp::McpServerConfig>,
    ) -> Result<(), KernelError> {
        // MCP startup can involve process spawn, HTTP auth, and handshakes.
        // Run injected servers in the background so session creation stays responsive.
        external_mcp_servers.into_iter().for_each(|config| {
            let manager = Arc::clone(&thread.mcp_manager);
            let tools = Arc::clone(&thread.tools);
            tokio::spawn(async move {
                match manager.register_external_mcp_server(config).await {
                    Ok(()) => tools.register_mcp_tools(Arc::clone(&manager)),
                    Err(error) => tracing::warn!(%error, "external MCP server registration failed"),
                }
            });
        });

        Ok(())
    }

    /// Recursively restore subagents from persisted agent edges.
    async fn restore_subagent_tree(
        &self,
        parent_session_id: &SessionId,
        agent_control: &Arc<AgentControl>,
        app_cfg: &Arc<config::AppConfig>,
    ) {
        let edges = match self
            .agent_graph_store
            .list_agent_children(parent_session_id, Some(AgentEdgeStatus::Open))
        {
            Ok(edges) => edges,
            Err(error) => {
                tracing::warn!(%error, parent_session_id = %parent_session_id, "failed to list subagent edges");
                return;
            }
        };
        for edge in edges {
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
            let llm = match self.default_llm() {
                Some(llm) => llm,
                None => {
                    tracing::warn!(child_id = %edge.child_session_id, "failed to resolve LLM for restored subagent");
                    continue;
                }
            };
            let handle = match self
                .thread_manager
                .load_thread(
                    LoadThreadParams::builder()
                        .session_id(edge.child_session_id.clone())
                        .cwd(child_replayed.meta.cwd.clone())
                        .history(child_replayed.messages.clone())
                        .llm(llm)
                        .tools(Arc::clone(&self.tools))
                        .agent_path(edge.child_agent_path.clone())
                        .agent_control(Arc::clone(agent_control))
                        .approval(Arc::new(ApprovalPolicy::new(app_cfg.approval)))
                        .app_config(Arc::clone(app_cfg))
                        .recorder(Arc::clone(&child_recorder))
                        .build(),
                )
                .await
            {
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
                let _ = self
                    .thread_manager
                    .close_thread(&edge.child_session_id)
                    .await;
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
            Box::pin(self.restore_subagent_tree(&edge.child_session_id, agent_control, app_cfg))
                .await;
        }
    }
}

#[async_trait]
impl AgentKernel for Kernel {
    async fn new_session(
        &self,
        cwd: PathBuf,
        options: SessionLaunchOptions,
    ) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let (provider_id, model_id) = self.active_provider_model().ok_or_else(|| {
            KernelError::Internal(anyhow::anyhow!(
                "active_model is not set or not in 'provider/model' format; \
                 check your config file or CLAW_PROVIDERS_* env vars"
            ))
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
        let handle = self
            .spawn_live_thread(
                session_id.clone(),
                cwd.clone(),
                Box::new(InMemoryContext::new()),
                recorder,
                app_cfg,
            )
            .await?;

        self.register_external_mcp_servers_for_thread(&handle, options.external_mcp_servers)
            .await?;

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

        Ok(SessionCreated::builder()
            .session_id(session_id)
            .modes(modes)
            .models(models)
            .build())
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
        _cwd: PathBuf,
        options: SessionLaunchOptions,
    ) -> Result<SessionCreated, KernelError> {
        if let Some(handle) = self.thread_manager.get_thread(session_id).await {
            self.register_external_mcp_servers_for_thread(&handle, options.external_mcp_servers)
                .await?;
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
        let recorder: Arc<dyn SessionRecorder> = Arc::from(recorder);
        let llm = self.default_llm().ok_or_else(|| {
            let active = &self.config.current().active_model;
            KernelError::Internal(anyhow::anyhow!(
                "no provider found for active_model '{active}'; \
                 add a [[providers]] section to your config file"
            ))
        })?;
        let handle = self
            .thread_manager
            .load_thread(
                LoadThreadParams::builder()
                    .session_id(session_id.clone())
                    .cwd(replayed.meta.cwd.clone())
                    .history(history.clone())
                    .llm(llm)
                    .tools(Arc::clone(&self.tools))
                    .agent_path(AgentPath::root())
                    .agent_control(Arc::clone(&self.agent_control))
                    .approval(Arc::new(ApprovalPolicy::new(app_cfg.approval)))
                    .app_config(Arc::clone(&app_cfg))
                    .recorder(Arc::clone(&recorder))
                    .build(),
            )
            .await?;
        self.register_external_mcp_servers_for_thread(&handle, options.external_mcp_servers)
            .await?;
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
        self.restore_subagent_tree(session_id, &agent_ctrl, &app_cfg)
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
            .thread_manager
            .live_sessions()
            .await
            .into_iter()
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
        let rx_event = self.thread_manager.take_rx(session_id).await?;
        let cancel_rx = self.thread_manager.cancel_rx(session_id).await?;
        self.thread_manager
            .send_op(
                session_id,
                Op::Prompt {
                    session_id: session_id.clone(),
                    text,
                    system: None,
                },
            )
            .await?;

        Ok(event_stream(rx_event, cancel_rx))
    }

    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
        self.thread_manager.cancel_thread(session_id).await
    }

    async fn set_mode(&self, session_id: &SessionId, _mode: &str) -> Result<(), KernelError> {
        if self.thread_manager.get_thread(session_id).await.is_none() {
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
        if self.thread_manager.get_thread(session_id).await.is_none() {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError> {
        let handle = self
            .thread_manager
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;

        self.session_store
            .close_session(session_id, handle.recorder.as_deref())
            .await
            .map_err(|error| KernelError::Internal(error.into()))?;

        // Signal close only after persistence succeeds so close can be retried on failure.
        self.thread_manager.close_thread(session_id).await?;
        Ok(())
    }

    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        _agent_path: AgentPath,
        _role: &str,
        _prompt: &str,
    ) -> Result<(), KernelError> {
        if self
            .thread_manager
            .get_thread(parent_session)
            .await
            .is_none()
        {
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
        // Clone the Arc so we can drop the thread manager handle before awaiting.
        let pending = self
            .thread_manager
            .get_thread(session_id)
            .await
            .map(|h| Arc::clone(&h.pending_approvals))
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;

        let tx = pending.lock().await.remove(call_id).ok_or_else(|| {
            KernelError::Internal(anyhow::anyhow!(
                "approval request not found for tool call {call_id}"
            ))
        })?;

        let _ = tx.send(decision);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use config::{AppConfig, ConfigHandle};
    use protocol::mcp::{McpServerConfig, McpTransportConfig};
    use store::{AgentEdgeStatus, AgentGraphStore};

    /// Builds a kernel with config-driven providers for metadata-only tests.
    fn kernel_with_config(app_config: AppConfig) -> Kernel {
        let config = ConfigHandle::from_config(app_config);
        let llm_factory = Arc::new(LlmFactory::new(config.clone()));
        Kernel::new(llm_factory, config, Arc::new(ToolRegistry::new()))
    }

    /// Builds an app config with one OpenAI-compatible provider for session tests.
    fn app_config_with_provider() -> AppConfig {
        serde_json::from_value(serde_json::json!({
            "active_model": "deepseek/deepseek-chat",
            "providers": [
                {
                    "id": "deepseek",
                    "display_name": "DeepSeek",
                    "provider_type": "openai-completions",
                    "base_url": "https://example.invalid",
                    "api_key": "test-key",
                    "models": [{ "id": "deepseek-chat" }]
                }
            ],
        }))
        .expect("valid app config")
    }

    /// Builds a runtime MCP config whose command cannot start.
    fn missing_external_mcp_server(name: &str) -> McpServerConfig {
        McpServerConfig::builder()
            .name(name.to_string())
            .external(true)
            .transport(McpTransportConfig::Stdio {
                command: "/definitely/missing/clawcode-mcp-test-server".to_string(),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
            })
            .build()
    }

    /// Builds launch options with one missing external MCP server.
    fn missing_external_mcp_options(name: &str) -> SessionLaunchOptions {
        SessionLaunchOptions {
            external_mcp_servers: vec![missing_external_mcp_server(name)],
        }
    }

    /// Verifies model metadata puts config.active_model first for ACP current model selection.
    #[test]
    fn build_models_orders_active_model_first() {
        let app_config: AppConfig = serde_json::from_value(serde_json::json!({
            "active_model": "deepseek/deepseek-chat",
            "providers": [
                {
                    "id": "openai",
                    "display_name": "OpenAI",
                    "provider_type": "openai-completions",
                    "base_url": "https://example.invalid",
                    "api_key": "test-key",
                    "models": [{ "id": "gpt-5.4" }]
                },
                {
                    "id": "deepseek",
                    "display_name": "DeepSeek",
                    "provider_type": "openai-completions",
                    "base_url": "https://example.invalid",
                    "api_key": "test-key",
                    "models": [
                        { "id": "deepseek-v4-flash" },
                        { "id": "deepseek-chat" }
                    ]
                }
            ],
        }))
        .expect("valid app config");
        let kernel = kernel_with_config(app_config);

        let models = kernel.build_models();

        assert_eq!(models[0].id, "deepseek/deepseek-chat");
        assert_eq!(models[1].id, "openai/gpt-5.4");
        assert_eq!(models[2].id, "deepseek/deepseek-v4-flash");
    }

    /// Verifies new-session launch options register external MCP servers asynchronously.
    #[tokio::test]
    async fn new_session_returns_before_external_mcp_server_startup_finishes() {
        let app_config = app_config_with_provider();
        let kernel = kernel_with_config(app_config);
        let cwd = tempfile::tempdir().expect("temp cwd");

        let created = kernel
            .new_session(
                cwd.path().to_path_buf(),
                missing_external_mcp_options("missing"),
            )
            .await
            .expect("session creation should not wait for external MCP startup");

        assert!(
            kernel
                .thread_manager
                .get_thread(&created.session_id)
                .await
                .is_some()
        );
    }

    /// Verifies active load-session options register external MCP servers asynchronously.
    #[tokio::test]
    async fn active_load_session_returns_before_external_mcp_server_startup_finishes() {
        let app_config = app_config_with_provider();
        let kernel = kernel_with_config(app_config);
        let cwd = tempfile::tempdir().expect("temp cwd");
        let created = kernel
            .new_session(cwd.path().to_path_buf(), SessionLaunchOptions::default())
            .await
            .expect("session should start without external MCP");

        kernel
            .load_session(
                &created.session_id,
                cwd.path().to_path_buf(),
                missing_external_mcp_options("missing-active"),
            )
            .await
            .expect("load session should not wait for external MCP startup");

        assert!(
            kernel
                .thread_manager
                .get_thread(&created.session_id)
                .await
                .is_some()
        );
    }

    /// Builds minimal session creation parameters for restore tests.
    fn persisted_session_params(
        session_id: SessionId,
        agent_path: AgentPath,
        cwd: PathBuf,
        parent_session_id: Option<SessionId>,
    ) -> CreateSessionParams {
        let builder = CreateSessionParams::builder()
            .session_id(session_id)
            .agent_path(agent_path)
            .cwd(cwd)
            .provider_id("deepseek".to_string())
            .model_id("deepseek-chat".to_string())
            .base_system_prompt(String::new());
        // Option setters use `strip_option`, so keep the parent branch explicit.
        if let Some(parent_session_id) = parent_session_id {
            builder.parent_session_id(parent_session_id).build()
        } else {
            builder.build()
        }
    }

    #[tokio::test]
    async fn load_session_restores_open_subagents_and_skips_closed_edges() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let store = FileSessionStore::new(true, Some(&data_home));
        let root_id = SessionId("root-session".to_string());
        let open_id = SessionId("open-child".to_string());
        let closed_id = SessionId("closed-child".to_string());
        let root_path = AgentPath::root();
        let open_path = root_path.join("open_child");
        let closed_path = root_path.join("closed_child");

        store
            .create_session(persisted_session_params(
                root_id.clone(),
                root_path.clone(),
                temp.path().to_path_buf(),
                None,
            ))
            .await
            .expect("create root")
            .expect("root recorder");
        store
            .create_session(persisted_session_params(
                open_id.clone(),
                open_path.clone(),
                temp.path().to_path_buf(),
                Some(root_id.clone()),
            ))
            .await
            .expect("create open child")
            .expect("open recorder");
        store
            .create_session(persisted_session_params(
                closed_id.clone(),
                closed_path.clone(),
                temp.path().to_path_buf(),
                Some(root_id.clone()),
            ))
            .await
            .expect("create closed child")
            .expect("closed recorder");
        store
            .upsert_agent_edge(
                root_id.clone(),
                open_id,
                open_path.clone(),
                Some("default".to_string()),
                AgentEdgeStatus::Open,
            )
            .await
            .expect("open edge");
        store
            .upsert_agent_edge(
                root_id.clone(),
                closed_id.clone(),
                closed_path.clone(),
                Some("default".to_string()),
                AgentEdgeStatus::Open,
            )
            .await
            .expect("closed child initial edge");
        store
            .set_agent_edge_status(&root_id, &closed_id, AgentEdgeStatus::Closed)
            .await
            .expect("closed edge");

        let kernel = kernel_with_config(app_config);
        kernel
            .load_session(
                &root_id,
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("load root");

        assert!(
            kernel
                .agent_control
                .registry
                .agent_id_for_path(&open_path)
                .is_some()
        );
        assert!(
            kernel
                .agent_control
                .registry
                .agent_id_for_path(&closed_path)
                .is_none()
        );
    }
}

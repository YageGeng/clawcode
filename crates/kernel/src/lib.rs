//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

pub mod agent;
pub mod approval;
pub mod command;
pub(crate) mod compaction;
pub mod context;
pub(crate) mod input_queue;
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
use protocol::message::Message;
use protocol::{
    AgentKernel, AgentPath, ContextWindowUsage, Event, KernelError, ModelInfo,
    Op, ReviewDecision, SessionCreated, SessionId, SessionInfo,
    SessionLaunchOptions, SessionListPage, SessionMode,
};
use provider::factory::LlmFactory;

use crate::agent::adapter::AgentControlAdapter;
use crate::agent::control::{AgentControl, AgentSpawnRequest};
use crate::approval::ApprovalPolicy;
use crate::command::prompt_args::parse_slash_name;
use crate::command::slash_command::SlashCommand;
use crate::context::InMemoryContext;
use crate::prompt::environment::EnvironmentInfo;
use crate::prompt::{Instructions, SystemPrompt};
use crate::session::{Thread, event_stream};
use crate::thread_manager::{
    LoadThreadParams, SpawnThreadParams, ThreadManager,
};
use store::{
    AgentEdgeStatus, AgentGraphStore, CreateSessionParams, FileSessionStore,
    SessionRecorder, SessionStore,
};
use tools::ToolRegistry;

const SESSION_LIST_PAGE_SIZE: usize = 10;
const SESSION_TITLE_MAX_CHARS: usize = 80;

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
            cfg.session_persistence.data_home.as_deref(),
        ));
        let store: Arc<dyn SessionStore> =
            Arc::clone(&file_store) as Arc<dyn SessionStore>;
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
        let adapter =
            Arc::new(AgentControlAdapter::new(Arc::clone(&self.agent_control)));
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
        let skill_registry =
            skills::SkillRegistry::discover(cwd, &app_cfg.skills);
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
        recorder: Arc<dyn SessionRecorder>,
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
        let params = SpawnThreadParams::builder()
            .session_id(session_id)
            .cwd(cwd)
            .llm(llm)
            .llm_factory(Arc::clone(&self.llm_factory))
            .tools(Arc::clone(&self.tools))
            .context(context)
            .agent_path(AgentPath::root())
            .agent_control(Arc::clone(&self.agent_control))
            .approval(approval)
            .app_config(app_cfg)
            .recorder(recorder)
            .build();
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
                description: Some(
                    "Agent asks for approval before making changes".to_string(),
                ),
            },
            SessionMode {
                id: "full-access".to_string(),
                name: "Full Access".to_string(),
                description: Some(
                    "Agent can modify files without approval".to_string(),
                ),
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
                p.models.iter().filter_map(|m| {
                    // Only advertise models that the runtime factory can actually resolve.
                    self.llm_factory.get(p.id.as_str(), &m.id).map(|_| {
                        ModelInfo::builder()
                            .id(format!("{}/{}", p.id.as_str(), m.id))
                            .display_name(
                                m.display_name
                                    .clone()
                                    .unwrap_or_else(|| m.id.clone()),
                            )
                            .description(None)
                            .context_tokens(m.context_tokens)
                            .max_output_tokens(m.max_output_tokens)
                            .build()
                    })
                })
            })
            .collect::<Vec<_>>();

        if let Some(active_index) =
            models.iter().position(|model| model.id == active_model)
        {
            // Keep the configured active model prominent while preserving the
            // configured order of all other models.
            let active = models.remove(active_index);
            models.insert(0, active);
        }

        models
    }

    /// Estimate context-window usage for restored live history and a model id.
    fn context_window_usage_for_history(
        &self,
        provider_id: &str,
        model_id: &str,
        cwd: &Path,
        history: &[Message],
    ) -> ContextWindowUsage {
        let preamble = self.render_base_system_prompt(
            cwd,
            model_id,
            &self.config.current(),
        );
        let estimated_tokens = Self::message_history_token_count(history)
            .saturating_add(preamble.len() / 4);
        let used_tokens = u64::try_from(estimated_tokens).unwrap_or(u64::MAX);
        let context_tokens = self.model_context_tokens(provider_id, model_id);
        ContextWindowUsage::new(used_tokens, context_tokens)
    }

    /// Estimate context-window usage from a `provider/model` display id.
    fn context_window_usage_for_model_label(
        &self,
        model_label: &str,
        cwd: &Path,
        history: &[Message],
    ) -> ContextWindowUsage {
        if let Some((provider_id, model_id)) = model_label.split_once('/') {
            self.context_window_usage_for_history(
                provider_id,
                model_id,
                cwd,
                history,
            )
        } else {
            let used_tokens =
                u64::try_from(Self::message_history_token_count(history))
                    .unwrap_or(u64::MAX);
            ContextWindowUsage::new(used_tokens, 0)
        }
    }

    /// Estimate token count for borrowed messages without cloning history.
    fn message_history_token_count(history: &[Message]) -> usize {
        // Keep this formula aligned with InMemoryContext::token_count.
        history
            .iter()
            .map(|message| format!("{message:?}").len() / 4)
            .sum()
    }

    /// Return configured context window size for a provider/model pair.
    fn model_context_tokens(&self, provider_id: &str, model_id: &str) -> u64 {
        self.config
            .current()
            .providers
            .iter()
            .find(|provider| provider.id.as_str() == provider_id)
            .and_then(|provider| {
                provider.models.iter().find(|model| model.id == model_id)
            })
            .and_then(|model| model.context_tokens)
            .unwrap_or(0)
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
                    Err(error) => {
                        tracing::warn!(%error, "external MCP server registration failed")
                    }
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
            let child_recorder: Arc<dyn SessionRecorder> =
                Arc::from(child_recorder);
            let llm = match self.default_llm() {
                Some(llm) => llm,
                None => {
                    tracing::warn!(child_id = %edge.child_session_id, "failed to resolve LLM for restored subagent");
                    continue;
                }
            };
            if let Err(e) = self
                .thread_manager
                .load_thread(
                    LoadThreadParams::builder()
                        .session_id(edge.child_session_id.clone())
                        .cwd(child_replayed.meta.cwd.clone())
                        .history(child_replayed.live_messages.clone())
                        .llm(llm)
                        .llm_factory(Arc::clone(&self.llm_factory))
                        .tools(Arc::clone(&self.tools))
                        .agent_path(edge.child_agent_path.clone())
                        .agent_control(Arc::clone(agent_control))
                        .approval(Arc::new(ApprovalPolicy::new(
                            app_cfg.approval,
                        )))
                        .app_config(Arc::clone(app_cfg))
                        .recorder(Arc::clone(&child_recorder))
                        .build(),
                )
                .await
            {
                tracing::warn!(child_id = %edge.child_session_id, error = %e, "failed to restore subagent thread");
                continue;
            }
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
            agent_control
                .register_recorder(
                    edge.child_session_id.clone(),
                    Arc::clone(&child_recorder),
                )
                .await;
            Box::pin(self.restore_subagent_tree(
                &edge.child_session_id,
                agent_control,
                app_cfg,
            ))
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
        let session_id = SessionId::from(uuid::Uuid::new_v4().to_string());
        let (provider_id, model_id) = self.active_provider_model().ok_or_else(|| {
            KernelError::Internal(anyhow::anyhow!(
                "active_model is not set or not in 'provider/model' format; \
                 check your config file or CLAW_PROVIDERS_* env vars"
            ))
        })?;
        let current_model = format!("{provider_id}/{model_id}");

        // Use the kernel-wide AgentControl shared across all sessions.
        let agent_ctrl = Arc::clone(&self.agent_control);

        let app_cfg = self.config.current();
        let base_system_prompt =
            self.render_base_system_prompt(&cwd, &model_id, &app_cfg);
        let recorder = self
            .session_store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(cwd.clone())
                    .provider_id(provider_id.clone())
                    .model_id(model_id.clone())
                    .base_system_prompt(base_system_prompt)
                    .build(),
            )
            .await
            .map_err(|error| KernelError::Internal(error.into()))?;
        let recorder: Arc<dyn SessionRecorder> = Arc::from(recorder);
        // Register root recorder so subagent spawns can write AgentEdge to parent.
        agent_ctrl
            .register_recorder(session_id.clone(), Arc::clone(&recorder))
            .await;
        let handle = self
            .spawn_live_thread(
                session_id.clone(),
                cwd.clone(),
                Box::new(InMemoryContext::new()),
                recorder,
                app_cfg,
            )
            .await?;

        self.register_external_mcp_servers_for_thread(
            &handle,
            options.external_mcp_servers,
        )
        .await?;

        // Register root only after session creation succeeds, avoiding stale registry entries.
        agent_ctrl.registry.register_root_thread(session_id.clone());

        let modes = self.build_modes();
        let models = self.build_models();

        Ok(SessionCreated::builder()
            .session_id(session_id)
            .current_model(current_model)
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
            self.register_external_mcp_servers_for_thread(
                &handle,
                options.external_mcp_servers,
            )
            .await?;
            // Active sessions may still be loaded by the TUI when switching agents. Replay the
            // persisted message log so ACP can hydrate the newly-created TUI session state.
            let replayed = self
                .session_store
                .load_session(session_id)
                .map_err(|error| KernelError::Internal(error.into()))?
                .map(|(replayed, _)| replayed);
            let current_model = handle.current_model().await;
            let context_window_usage = replayed
                .as_ref()
                .map(|replayed| {
                    self.context_window_usage_for_model_label(
                        &current_model,
                        &handle.cwd,
                        &replayed.live_messages,
                    )
                })
                .unwrap_or_else(|| {
                    self.context_window_usage_for_model_label(
                        &current_model,
                        &handle.cwd,
                        &[],
                    )
                });
            let history = replayed
                .map(|replayed| replayed.messages)
                .unwrap_or_default();
            return Ok(SessionCreated::builder()
                .session_id(session_id.clone())
                .current_model(current_model)
                .modes(self.build_modes())
                .models(self.build_models())
                .history(history)
                .context_window_usage(context_window_usage)
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
        let live_history = replayed.live_messages;
        let recorder: Arc<dyn SessionRecorder> = Arc::from(recorder);
        let llm = self.default_llm().ok_or_else(|| {
            let active = &self.config.current().active_model;
            KernelError::Internal(anyhow::anyhow!(
                "no provider found for active_model '{active}'; \
                 add a [[providers]] section to your config file"
            ))
        })?;
        let context_window_usage = self.context_window_usage_for_history(
            llm.provider_id(),
            llm.model_id(),
            &replayed.meta.cwd,
            &live_history,
        );
        let handle = self
            .thread_manager
            .load_thread(
                LoadThreadParams::builder()
                    .session_id(session_id.clone())
                    .cwd(replayed.meta.cwd.clone())
                    .history(live_history)
                    .llm(llm)
                    .llm_factory(Arc::clone(&self.llm_factory))
                    .tools(Arc::clone(&self.tools))
                    .agent_path(AgentPath::root())
                    .agent_control(Arc::clone(&self.agent_control))
                    .approval(Arc::new(ApprovalPolicy::new(app_cfg.approval)))
                    .app_config(Arc::clone(&app_cfg))
                    .recorder(Arc::clone(&recorder))
                    .build(),
            )
            .await?;
        self.register_external_mcp_servers_for_thread(
            &handle,
            options.external_mcp_servers,
        )
        .await?;
        let agent_ctrl = Arc::clone(&self.agent_control);
        agent_ctrl.registry.register_root_thread(session_id.clone());
        agent_ctrl
            .register_recorder(session_id.clone(), recorder)
            .await;
        self.restore_subagent_tree(session_id, &agent_ctrl, &app_cfg)
            .await;

        Ok(SessionCreated::builder()
            .session_id(session_id.clone())
            .current_model(handle.current_model().await)
            .modes(self.build_modes())
            .models(self.build_models())
            .history(history)
            .context_window_usage(context_window_usage)
            .build())
    }

    async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError> {
        let offset = session_list_offset(cursor)?;
        let persisted_sessions = self
            .session_store
            .list_sessions(cwd)
            .map_err(|error| KernelError::Internal(error.into()))?;
        let persisted_titles: std::collections::HashMap<
            SessionId,
            Option<String>,
        > = persisted_sessions
            .iter()
            .map(|session| (session.session_id.clone(), session.title.clone()))
            .collect();
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
                    .title(
                        persisted_titles
                            .get(&thread.session_id)
                            .cloned()
                            .flatten(),
                    )
                    .build()
            })
            .collect();
        let live_ids: std::collections::HashSet<SessionId> = sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect();
        for session in persisted_sessions {
            if !live_ids.contains(&session.session_id) {
                sessions.push(session);
            }
        }

        Ok(paginate_session_list(sessions, offset))
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>,
        >,
        KernelError,
    > {
        if let Some(SlashCommand::Sessions) =
            SlashCommand::parse_from_text(&text)
        {
            return self.prompt_sessions_command(session_id, &text).await;
        }
        if let Some(SlashCommand::Compact) =
            SlashCommand::parse_from_text(&text)
        {
            return self.prompt_compact_command(session_id).await;
        }

        if let Some(title) = Self::session_title_from_prompt_text(&text)
            && let Err(error) =
                self.session_store.record_session_title(session_id, &title)
        {
            tracing::warn!(%error, %session_id, "failed to persist session title");
        }

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

    /// Subscribe to an existing live session's event channel without starting a turn.
    async fn subscribe_session_events(
        &self,
        session_id: &SessionId,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>,
        >,
        KernelError,
    > {
        let rx_event = self.thread_manager.take_rx(session_id).await?;
        let cancel_rx = self.thread_manager.cancel_rx(session_id).await?;
        Ok(event_stream(rx_event, cancel_rx))
    }

    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
        self.thread_manager.cancel_thread(session_id).await
    }

    async fn set_mode(
        &self,
        session_id: &SessionId,
        _mode: &str,
    ) -> Result<(), KernelError> {
        if self.thread_manager.get_thread(session_id).await.is_none() {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn set_model(
        &self,
        session_id: &SessionId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), KernelError> {
        self.thread_manager
            .set_model(session_id, provider_id, model_id)
            .await
    }

    async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let handle = self
            .thread_manager
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;

        self.session_store
            .close_session(session_id, handle.recorder.as_ref())
            .await
            .map_err(|error| KernelError::Internal(error.into()))?;

        // Signal close only after persistence succeeds so close can be retried on failure.
        self.thread_manager.close_thread(session_id).await?;
        Ok(())
    }

    async fn agent_ui_snapshot(
        &self,
        root_session_id: &SessionId,
    ) -> Result<Vec<protocol::AgentUiMetadata>, KernelError> {
        Ok(self.agent_control.agent_ui_snapshot(root_session_id).await)
    }

    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        agent_path: AgentPath,
        role: &str,
        model: Option<&str>,
        prompt: &str,
    ) -> Result<(), KernelError> {
        let parent_thread = self
            .thread_manager
            .get_thread(parent_session)
            .await
            .ok_or_else(|| {
                KernelError::SessionNotFound(parent_session.clone())
            })?;
        let parent_path = self
            .agent_control
            .registry
            .agent_metadata_for_thread(parent_session.clone())
            .and_then(|metadata| metadata.agent_path)
            .unwrap_or_else(AgentPath::root);
        let direct_child_prefix = format!("{}/", parent_path.as_str());
        let child_name = agent_path
            .as_str()
            .strip_prefix(&direct_child_prefix)
            .filter(|name| !name.is_empty() && !name.contains('/'))
            .ok_or_else(|| {
                KernelError::Internal(anyhow::anyhow!(
                    "agent_path must be a direct child of {parent_path}: {agent_path}"
                ))
            })?;

        self.agent_control
            .spawn({
                let mut request = AgentSpawnRequest::builder()
                    .parent_path(parent_path)
                    .task_name(child_name.to_string())
                    .role_name(role.to_string())
                    .prompt(prompt.to_string())
                    .cwd(parent_thread.cwd.clone())
                    .build();
                request.model = model.map(ToString::to_string);
                request
            })
            .await
            .map(|_| ())
            .map_err(|error| KernelError::Internal(anyhow::anyhow!(error)))
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

impl Kernel {
    /// Handle `/sessions [offset]` before a prompt reaches the provider turn loop.
    async fn prompt_sessions_command(
        &self,
        session_id: &SessionId,
        text: &str,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>,
        >,
        KernelError,
    > {
        let cursor = Self::sessions_cursor_from_prompt_text(text)?;
        let thread = self
            .thread_manager
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let page = self
            .list_sessions(Some(&thread.cwd), cursor.as_deref())
            .await?;
        let message = Self::format_sessions_prompt_response(&page);
        let events = vec![
            Ok(Event::message_chunk(session_id.clone(), message)),
            Ok(Event::turn_complete(
                session_id.clone(),
                protocol::StopReason::EndTurn,
            )),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    /// Handle `/compact` by asking the idle session runtime to compact live history.
    async fn prompt_compact_command(
        &self,
        session_id: &SessionId,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>,
        >,
        KernelError,
    > {
        let rx_event = self.thread_manager.take_rx(session_id).await?;
        let cancel_rx = self.thread_manager.cancel_rx(session_id).await?;
        self.thread_manager
            .send_op(
                session_id,
                Op::Compact {
                    session_id: session_id.clone(),
                },
            )
            .await?;

        Ok(event_stream(rx_event, cancel_rx))
    }

    /// Parse the optional offset cursor from `/sessions` prompt text.
    fn sessions_cursor_from_prompt_text(
        text: &str,
    ) -> Result<Option<String>, KernelError> {
        let Some((name, rest, _rest_offset)) = parse_slash_name(text) else {
            return Err(KernelError::Internal(anyhow::anyhow!(
                "Usage: /sessions [offset]"
            )));
        };
        if name != SlashCommand::Sessions.command() {
            return Err(KernelError::Internal(anyhow::anyhow!(
                "Usage: /sessions [offset]"
            )));
        }
        let Some(offset) = rest.split_whitespace().next() else {
            return Ok(None);
        };
        offset
            .parse::<usize>()
            .map(|parsed| Some(parsed.to_string()))
            .map_err(|error| {
                KernelError::Internal(anyhow::anyhow!(
                    "invalid /sessions offset `{offset}`: {error}"
                ))
            })
    }

    /// Format a session-list page as text suitable for frontend transcript rendering.
    fn format_sessions_prompt_response(response: &SessionListPage) -> String {
        if response.sessions.is_empty() {
            return "No sessions found for this working directory.".into();
        }
        let lines: Vec<String> = response
            .sessions
            .iter()
            .map(|session| {
                let title = session
                    .title
                    .as_deref()
                    .map(Self::truncate_session_title)
                    .unwrap_or_else(|| "-".into());
                let time = session.updated_at.as_deref().unwrap_or("-");
                format!(
                    "| {} | {} | {} | {} |",
                    Self::escape_markdown_table_cell(&session.session_id.0),
                    Self::escape_markdown_table_cell(
                        &session.cwd.display().to_string()
                    ),
                    Self::escape_markdown_table_cell(&title),
                    Self::escape_markdown_table_cell(time)
                )
            })
            .collect();
        let mut output = format!(
            "Recent sessions:\n\n| Session | Cwd | Title | Updated |\n| --- | --- | --- | --- |\n{}",
            lines.join("\n")
        );
        if let Some(next_cursor) = &response.next_cursor {
            output.push_str(&format!("\n\nNext: /sessions {next_cursor}"));
        }
        output
    }

    /// Escape content for one Markdown table cell.
    fn escape_markdown_table_cell(value: &str) -> String {
        value
            .replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace(['\r', '\n'], " ")
    }

    /// Build the manifest title from a user prompt by compacting whitespace.
    fn session_title_from_prompt_text(text: &str) -> Option<String> {
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.is_empty() {
            return None;
        }
        Some(compact.chars().take(SESSION_TITLE_MAX_CHARS).collect())
    }

    /// Return a quoted title capped for compact session-list rendering.
    fn truncate_session_title(title: &str) -> String {
        let chars: String = title.chars().take(10).collect();
        if title.chars().count() > 10 {
            format!("\"{chars}...\"")
        } else {
            format!("\"{chars}\"")
        }
    }
}

/// Parses the ACP session/list cursor as a zero-based offset.
fn session_list_offset(cursor: Option<&str>) -> Result<usize, KernelError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor.parse::<usize>().map_err(|error| {
        KernelError::Internal(anyhow::anyhow!(
            "invalid session list cursor `{cursor}`: {error}"
        ))
    })
}

/// Returns one fixed-size session page and the next offset cursor when available.
fn paginate_session_list(
    sessions: Vec<SessionInfo>,
    offset: usize,
) -> SessionListPage {
    let total = sessions.len();
    let page: Vec<SessionInfo> = sessions
        .into_iter()
        .skip(offset)
        .take(SESSION_LIST_PAGE_SIZE)
        .collect();
    let next_offset = offset.saturating_add(SESSION_LIST_PAGE_SIZE);
    let next_cursor = (next_offset < total).then(|| next_offset.to_string());
    SessionListPage {
        sessions: page,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use config::{AppConfig, ConfigHandle};
    use futures::StreamExt;
    use protocol::mcp::{McpServerConfig, McpTransportConfig};
    use protocol::message::Message;
    use store::{
        AgentEdgeStatus, AgentGraphStore, MessageRecord, PersistedPayload,
        TurnContextRecord,
    };

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

    /// Builds an app config with two selectable provider/model pairs.
    fn app_config_with_two_models() -> AppConfig {
        serde_json::from_value(serde_json::json!({
            "active_model": "deepseek/deepseek-chat",
            "providers": [
                {
                    "id": "deepseek",
                    "display_name": "DeepSeek",
                    "provider_type": "openai-completions",
                    "base_url": "http://127.0.0.1:9",
                    "api_key": "test-key",
                    "models": [{ "id": "deepseek-chat" }]
                },
                {
                    "id": "openai",
                    "display_name": "OpenAI",
                    "provider_type": "openai-completions",
                    "base_url": "http://127.0.0.1:9",
                    "api_key": "test-key",
                    "models": [{ "id": "gpt-5.4" }]
                }
            ],
        }))
        .expect("valid app config")
    }

    /// Read the latest persisted turn context from a test data-home directory.
    fn latest_turn_context_record(
        data_home: &std::path::Path,
    ) -> TurnContextRecord {
        let mut session_files = Vec::new();
        collect_jsonl_files(data_home, &mut session_files);
        session_files.sort();

        session_files
            .iter()
            .rev()
            .find_map(|path| {
                let text =
                    std::fs::read_to_string(path).expect("read session file");
                text.lines().rev().find_map(|line| {
                    let record =
                        serde_json::from_str::<store::PersistedRecord>(line)
                            .ok()?;
                    match record.payload {
                        store::PersistedPayload::TurnContext(turn_context) => {
                            Some(turn_context)
                        }
                        _ => None,
                    }
                })
            })
            .expect("persisted turn context")
    }

    /// Recursively collect persisted session JSONL files.
    fn collect_jsonl_files(
        dir: &std::path::Path,
        files: &mut Vec<std::path::PathBuf>,
    ) {
        for entry in std::fs::read_dir(dir).expect("read data home") {
            let path = entry.expect("read data home entry").path();
            if path.is_dir() {
                collect_jsonl_files(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str())
                == Some("jsonl")
            {
                files.push(path);
            }
        }
    }

    /// Builds a runtime MCP config whose command cannot start.
    fn missing_external_mcp_server(name: &str) -> McpServerConfig {
        McpServerConfig::builder()
            .name(name.to_string())
            .external(true)
            .transport(McpTransportConfig::Stdio {
                command: "/definitely/missing/clawcode-mcp-test-server"
                    .to_string(),
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

    /// Verifies unavailable provider/model pairs are not exposed to ACP clients.
    #[test]
    fn build_models_filters_unavailable_provider_models() {
        let app_config: AppConfig = serde_json::from_value(serde_json::json!({
            "active_model": "chatgpt/gpt-5.4",
            "providers": [
                {
                    "id": "openai",
                    "display_name": "OpenAI",
                    "provider_type": "responses",
                    "base_url": "https://example.invalid",
                    "api_key": { "env": "CLAWCODE_TEST_MISSING_OPENAI_API_KEY_DO_NOT_SET" },
                    "models": [{ "id": "gpt-5.4" }]
                },
                {
                    "id": "chatgpt",
                    "display_name": "ChatGPT",
                    "provider_type": "responses",
                    "base_url": "https://chatgpt.com/backend-api/codex",
                    "auth": { "type": "codex" },
                    "models": [{ "id": "gpt-5.4" }]
                }
            ],
        }))
        .expect("valid app config");
        let kernel = kernel_with_config(app_config);

        let models = kernel.build_models();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "chatgpt/gpt-5.4");
    }

    #[tokio::test]
    async fn set_model_changes_model_used_by_next_prompt_turn() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_two_models();
        app_config.session_persistence.data_home = Some(data_home);
        let kernel = kernel_with_config(app_config);
        let created = kernel
            .new_session(
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("session should start");

        kernel
            .set_model(&created.session_id, "openai", "gpt-5.4")
            .await
            .expect("model switch should succeed");
        let mut events = kernel
            .prompt(&created.session_id, "hello".to_string())
            .await
            .expect("prompt should start");

        let _ = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .expect("prompt should produce a terminal event");
        let turn_context = latest_turn_context_record(temp.path());

        assert_eq!(turn_context.provider_id, "openai");
        assert_eq!(turn_context.model_id, "gpt-5.4");
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
            .expect(
                "session creation should not wait for external MCP startup",
            );

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
    async fn active_load_session_returns_before_external_mcp_server_startup_finishes()
     {
        let app_config = app_config_with_provider();
        let kernel = kernel_with_config(app_config);
        let cwd = tempfile::tempdir().expect("temp cwd");
        let created = kernel
            .new_session(
                cwd.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
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

    /// Verifies loading an already-live persisted session still returns replayable history.
    #[tokio::test]
    async fn active_load_session_returns_persisted_history_for_replay() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let session_id = SessionId::from("live-child");
        let store = FileSessionStore::new(Some(&data_home));
        let recorder = store
            .create_session(persisted_session_params(
                session_id.clone(),
                AgentPath::root().join("child"),
                temp.path().to_path_buf(),
                Some(SessionId::from("root")),
            ))
            .await
            .expect("create persisted child session");
        recorder
            .append(&[PersistedPayload::Message(
                MessageRecord::builder()
                    .turn_id("turn-1".to_string())
                    .message(Message::user("child history"))
                    .build(),
            )])
            .await
            .expect("append child history");
        let kernel = kernel_with_config(app_config);

        kernel
            .load_session(
                &session_id,
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("initial load should restore child");
        let active_load = kernel
            .load_session(
                &session_id,
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("active load should succeed");

        assert_eq!(active_load.history, vec![Message::user("child history")]);
    }

    /// Verifies restored sessions report estimated live context usage for the UI.
    #[tokio::test]
    async fn load_session_returns_context_usage_for_replayed_live_history() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        app_config
            .providers
            .first_mut()
            .expect("test provider")
            .models
            .first_mut()
            .expect("test model")
            .context_tokens = Some(1_000_000);
        let session_id = SessionId::from("resume-session");
        let store = FileSessionStore::new(Some(&data_home));
        let recorder = store
            .create_session(persisted_session_params(
                session_id.clone(),
                AgentPath::root(),
                temp.path().to_path_buf(),
                None,
            ))
            .await
            .expect("create persisted session");
        recorder
            .append(&[PersistedPayload::Message(
                MessageRecord::builder()
                    .turn_id("turn-1".to_string())
                    .message(Message::user("restored history"))
                    .build(),
            )])
            .await
            .expect("append restored history");
        let kernel = kernel_with_config(app_config);

        let loaded = kernel
            .load_session(
                &session_id,
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("load session should succeed");
        let usage = loaded
            .context_window_usage
            .expect("load should include context usage");
        let live_history_tokens =
            Kernel::message_history_token_count(&[Message::user(
                "restored history",
            )]);

        assert!(usage.used_tokens > live_history_tokens as u64);
        assert_eq!(usage.context_tokens, 1_000_000);
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
    async fn list_sessions_paginates_with_cursor_offset() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let store = FileSessionStore::new(Some(&data_home));
        for index in 0..12 {
            store
                .create_session(persisted_session_params(
                    SessionId::from(format!("session-{index:02}")),
                    AgentPath::root(),
                    temp.path().to_path_buf(),
                    None,
                ))
                .await
                .expect("create persisted session");
        }
        let kernel = kernel_with_config(app_config);

        let first_page = kernel
            .list_sessions(Some(temp.path()), None)
            .await
            .expect("list first page");
        let second_page = kernel
            .list_sessions(Some(temp.path()), Some("10"))
            .await
            .expect("list second page");

        assert_eq!(first_page.sessions.len(), 10);
        assert_eq!(first_page.next_cursor, Some("10".to_string()));
        assert_eq!(
            second_page
                .sessions
                .iter()
                .map(|session| session.session_id.0.as_ref())
                .collect::<Vec<_>>(),
            vec!["session-10", "session-11"]
        );
        assert_eq!(second_page.next_cursor, None);
    }

    #[tokio::test]
    async fn list_sessions_merges_manifest_title_for_live_session() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let kernel = kernel_with_config(app_config);
        let created = kernel
            .new_session(
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("session should start");

        kernel
            .session_store
            .record_session_title(
                &created.session_id,
                "Implement persisted titles",
            )
            .expect("record title");

        let page = kernel
            .list_sessions(Some(temp.path()), None)
            .await
            .expect("list sessions");

        assert_eq!(page.sessions.len(), 1);
        assert_eq!(
            page.sessions[0].title.as_deref(),
            Some("Implement persisted titles")
        );
    }

    #[test]
    fn session_title_from_prompt_text_compacts_whitespace_and_truncates() {
        let long_prompt = format!(
            "  Implement\n\n  manifest   titles  {}",
            "x".repeat(SESSION_TITLE_MAX_CHARS)
        );

        let title = Kernel::session_title_from_prompt_text(&long_prompt)
            .expect("title");

        assert!(!title.contains('\n'));
        assert!(!title.contains("  "));
        assert_eq!(title.chars().count(), SESSION_TITLE_MAX_CHARS);
    }

    #[tokio::test]
    async fn prompt_sessions_command_lists_sessions_without_provider_turn() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let store = FileSessionStore::new(Some(&data_home));
        for index in 0..12 {
            store
                .create_session(persisted_session_params(
                    SessionId::from(format!("session-{index:02}")),
                    AgentPath::root(),
                    temp.path().to_path_buf(),
                    None,
                ))
                .await
                .expect("create persisted session");
        }
        let kernel = kernel_with_config(app_config);
        let created = kernel
            .new_session(
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("session should start before slash command");

        let mut events = kernel
            .prompt(&created.session_id, "/sessions 10".to_string())
            .await
            .expect("slash command should create an event stream");

        let first = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("slash command should not wait for provider output")
            .expect("slash command should emit a message")
            .expect("slash command message should be valid");
        let Event::AgentMessageChunk { text, .. } = first else {
            panic!("expected /sessions to emit an agent message");
        };
        assert!(text.contains("Recent sessions:"));
        assert!(text.contains("| Session | Cwd | Title | Updated |"));
        assert!(text.contains("| --- | --- | --- | --- |"));
        assert!(text.contains(&format!(
            "| session-09 | {} | - |",
            temp.path().display()
        )));
        assert!(text.contains("session-09"));
        assert!(text.contains("session-11"));
        assert!(!text.contains("session-08"));

        let second =
            tokio::time::timeout(Duration::from_secs(1), events.next())
                .await
                .expect("slash command should finish promptly")
                .expect("slash command should emit completion")
                .expect("slash command completion should be valid");
        assert!(matches!(
            second,
            Event::TurnComplete {
                stop_reason: protocol::StopReason::EndTurn,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn load_session_restores_open_subagents_and_skips_closed_edges() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home.clone());
        let store = FileSessionStore::new(Some(&data_home));
        let root_id = SessionId::from("root-session");
        let open_id = SessionId::from("open-child");
        let closed_id = SessionId::from("closed-child");
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
            .expect("create root");
        store
            .create_session(persisted_session_params(
                open_id.clone(),
                open_path.clone(),
                temp.path().to_path_buf(),
                Some(root_id.clone()),
            ))
            .await
            .expect("create open child");
        store
            .create_session(persisted_session_params(
                closed_id.clone(),
                closed_path.clone(),
                temp.path().to_path_buf(),
                Some(root_id.clone()),
            ))
            .await
            .expect("create closed child");
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
            .set_agent_edge_status(
                &root_id,
                &closed_id,
                AgentEdgeStatus::Closed,
            )
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

    #[tokio::test]
    async fn kernel_spawn_agent_creates_child_thread() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home);
        let kernel = kernel_with_config(app_config);
        let created = kernel
            .new_session(
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("create root session");
        let child_path = AgentPath::root().join("worker");

        kernel
            .spawn_agent(
                &created.session_id,
                child_path.clone(),
                "default",
                None,
                "do the work",
            )
            .await
            .expect("spawn child");

        let child_id = kernel
            .agent_control
            .registry
            .agent_id_for_path(&child_path)
            .expect("child path registered");
        assert!(kernel.thread_manager.get_thread(&child_id).await.is_some());
    }

    #[tokio::test]
    async fn kernel_spawn_agent_rejects_non_direct_child_path() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let mut app_config = app_config_with_provider();
        app_config.session_persistence.data_home = Some(data_home);
        let kernel = kernel_with_config(app_config);
        let created = kernel
            .new_session(
                temp.path().to_path_buf(),
                SessionLaunchOptions::default(),
            )
            .await
            .expect("create root session");

        let error = kernel
            .spawn_agent(
                &created.session_id,
                AgentPath::root().join("team/worker"),
                "default",
                None,
                "do the work",
            )
            .await
            .expect_err("non-direct child paths should be rejected");

        assert!(error.to_string().contains("direct child"), "{error}");
        assert!(
            kernel
                .agent_control
                .registry
                .agent_id_for_path(&AgentPath::root().join("worker"))
                .is_none()
        );
    }
}

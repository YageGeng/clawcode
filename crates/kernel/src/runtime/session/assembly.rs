//! Building a live Session runtime from its Store, extensions, and MCP capability.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use ::extension::{
    DynamicCommandRegistry, ExtensionRegistrar, ExtensionRuntime,
    HandlerRegistry,
};
use ::tools::ToolRegistry;
use protocol::{
    ExtensionFlagDefinition, LaneId, SessionId, SessionMode, ThinkingLevel,
};
use store::{EntryKind, SessionStore};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use super::model_state::SessionModelState;
use super::settings;
use super::transcript::SessionTranscript;
use crate::runtime::extension::{
    KernelExtensionDiagnostics, SessionExtensions,
};
use crate::runtime::mcp::SessionMcpRuntime;
use crate::runtime::plan::SessionPlanState;
use crate::runtime::run::lifecycle::{SessionExecution, SessionLifecycle};
use crate::runtime::run::queue::PendingQueue;
use crate::runtime::tools::SessionToolState;
use crate::runtime::{Kernel, KernelError, Session, mcp, tools};
use crate::{Model, ModelCatalog};

/// Session-local selections reconstructed from Pi v4 branch entries.
struct RestoredSessionSettings {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    mode: SessionMode,
}

impl RestoredSessionSettings {
    /// Replays settings while retaining preferences for dynamic capabilities that may reconnect.
    fn load(
        store: &dyn SessionStore,
        lane: &LaneId,
        models: &ModelCatalog,
    ) -> Result<Self, KernelError> {
        let mut settings = Self {
            model: models.active(),
            // Sessions without a persisted choice use the product default level.
            thinking_level: ThinkingLevel::default(),
            mode: SessionMode::default(),
        };
        let Some(leaf) = store.lane(lane) else {
            return Ok(settings);
        };
        let branch = store.branch(leaf)?;
        settings.mode = SessionPlanState::branch_mode(&branch);
        for entry in branch {
            if !matches!(
                entry.kind,
                EntryKind::ModelChange | EntryKind::ThinkingLevelChange
            ) {
                continue;
            }
            match settings::SettingChange::try_from(&entry)? {
                settings::SettingChange::Model { provider, model_id } => {
                    // A removed provider must not make the session unopenable.
                    match models.resolve(&provider, &model_id) {
                        Some(model) => settings.model = model,
                        None => tracing::warn!(
                            "persisted model {}/{} is no longer configured; keeping {}/{}",
                            provider,
                            model_id,
                            settings.model.profile().provider_id,
                            settings.model.profile().model_id
                        ),
                    }
                }
                settings::SettingChange::ThinkingLevel(level) => {
                    settings.thinking_level = level
                }
            }
        }
        Ok(settings)
    }
}

/// Inputs for restoring and wiring one Session once its extensions are frozen.
#[derive(typed_builder::TypedBuilder)]
struct SessionAssembly<'a> {
    kernel: &'a Kernel,
    session_id: SessionId,
    store: Box<dyn SessionStore>,
    cwd: PathBuf,
    #[builder(default)]
    ownership: Option<protocol::SessionOwnership>,
    extensions: Arc<HandlerRegistry>,
}

impl SessionAssembly<'_> {
    /// Restores durable state and wires one runtime; the caller closes MCP on failure.
    async fn assemble(
        self,
        mcp: mcp::AcquiredSessionMcp,
    ) -> Result<Arc<Session>, KernelError> {
        let kernel = self.kernel;
        let lane = LaneId::try_from("main")
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        // Each Session builds its own catalog so model selection stays Session-local.
        let session_models = Arc::new(
            kernel
                .model_factory
                .create_catalog(&kernel.static_extensions)?,
        );
        let settings = RestoredSessionSettings::load(
            self.store.as_ref(),
            &lane,
            &session_models,
        )?;
        // Owned Sessions are subagents, whose plan-mode notes use their own wording.
        let delegated = self.ownership.is_some();
        // Resume starts above both shared Store mutations and the extra
        // Usage events synthesized from persisted Assistant messages.
        let initial_event_sequence =
            Kernel::replay_sequence_floor(self.store.as_ref(), &lane)?;
        let queue = PendingQueue::restore(self.store.as_ref())?;
        let commands = DynamicCommandRegistry::new(self.extensions.commands())
            .map_err(|error| {
                KernelError::ExtensionBlocked(error.to_string())
            })?;
        let flags = self.merged_flags();
        let tools = self.restored_tools(&lane, mcp.tools)?;
        let execution = Arc::new(
            SessionExecution::builder()
                .lifecycle(RwLock::new(SessionLifecycle::Starting))
                .gate(AsyncMutex::new(()))
                .active_run(Mutex::new(None))
                .idle_notify(Notify::new())
                .cancellation(Mutex::new(CancellationToken::new()))
                .queue(Mutex::new(queue))
                .sequence(AtomicU64::new(initial_event_sequence))
                .build(),
        );
        // Diagnostics share the transcript component without retaining the
        // Session and creating a reference cycle through ExtensionRuntime.
        let transcript =
            Arc::new(SessionTranscript::restored(self.store, lane, delegated)?);
        let extension_runtime = Arc::new(ExtensionRuntime::new(
            self.extensions,
            Arc::new(
                KernelExtensionDiagnostics::builder()
                    .event_sinks(Arc::clone(&kernel.event_sinks))
                    .clock(Arc::clone(&kernel.clock))
                    .execution(Arc::clone(&execution))
                    .transcript(Arc::clone(&transcript))
                    .id_generator(Arc::clone(&kernel.id_generator))
                    .build(),
            ),
        ));
        let extensions = SessionExtensions::builder()
            .runtime(extension_runtime)
            .commands(commands)
            .flags(Arc::from(flags))
            .events(broadcast::channel(64).0)
            .build();
        let mcp_host = mcp
            .connection
            .as_ref()
            .map(|(_session, host)| Arc::clone(host));
        let runtime = Arc::new(
            Session::builder()
                .ownership(self.ownership)
                .cwd(self.cwd)
                .transcript(transcript)
                .mcp(SessionMcpRuntime::new(
                    mcp.connection,
                    self.session_id,
                    mcp.lease,
                ))
                .resources(OnceLock::new())
                .extensions(extensions)
                .tools(tools)
                .model(SessionModelState::new(
                    session_models,
                    settings.model,
                    settings.thinking_level,
                ))
                .plan(SessionPlanState::new(settings.mode))
                .exec(kernel.exec.as_ref().map(|settings| {
                    Arc::new(::tools::exec::ExecProcessManager::new(
                        Arc::clone(settings),
                    ))
                }))
                .execution(execution)
                .build(),
        );
        runtime
            .tools
            .register_kernel(Arc::new(tools::ToolSearchTool::new(
                Arc::downgrade(&runtime),
                Arc::clone(&kernel.id_generator),
            )))?;
        crate::runtime::subagent::register(&runtime, kernel)?;
        crate::runtime::plan::register_exit_plan_mode(&runtime, kernel)?;
        crate::runtime::plan::register_todo_write(&runtime)?;
        crate::runtime::exec::register(&runtime, kernel)?;
        if runtime.exec.is_some() {
            kernel.reconcile_lost_exec(&runtime)?;
        }
        if let Some(host) = mcp_host
            && let Err(error) = host.attach(&runtime)
        {
            runtime.extensions.runtime_ref().invalidate();
            return Err(error);
        }
        if let Err(error) = runtime.start_mcp_projection().await {
            runtime.extensions.runtime_ref().invalidate();
            // Shutdown runs once, so the caller's later cleanup reports success
            // and this is the only place a failure here can be observed.
            if let Err(shutdown_error) = runtime.mcp.shutdown().await {
                tracing::warn!(
                    "failed to shut down MCP after its projection failed to start: {}",
                    shutdown_error
                );
            }
            return Err(error);
        }
        Ok(runtime)
    }

    /// Lists static flags first, so a session module cannot redefine their names.
    fn merged_flags(&self) -> Vec<ExtensionFlagDefinition> {
        let mut flags = self.kernel.static_extensions.flags.clone();
        let mut flag_names = flags
            .iter()
            .map(|flag| flag.name.clone())
            .collect::<BTreeSet<_>>();
        for flag in self.extensions.flags() {
            if flag_names.insert(flag.name.clone()) {
                flags.push(flag);
            }
        }
        flags
    }

    /// Composes tool partitions and restores the selection persisted on this branch.
    fn restored_tools(
        &self,
        lane: &LaneId,
        mcp_tools: ToolRegistry,
    ) -> Result<Arc<SessionToolState>, KernelError> {
        let mut extension_tools = ToolRegistry::default();
        let mut extension_tool_names = BTreeSet::new();
        for (_extension_id, tool) in self.extensions.tools() {
            let name = tool.definition().name;
            if tools::RESERVED_TOOL_NAMES.contains(&name.as_str()) {
                // Refusing only the tool keeps the Session creatable.
                tracing::warn!(
                    "rejected extension tool {} because the name is reserved",
                    name
                );
                continue;
            }
            // The first extension registration wins while still overriding built-ins.
            if extension_tool_names.insert(name) {
                extension_tools.upsert(tool);
            }
        }
        let tools = Arc::new(SessionToolState::new(
            (*self.kernel.tools).clone(),
            extension_tools,
            mcp_tools,
            protocol::ToolSelection::default(),
        )?);
        // Restore from the full branch rather than compaction's model-context window.
        let entries = self
            .store
            .lane(lane)
            .map(|leaf| self.store.branch(leaf))
            .transpose()?
            .unwrap_or_default();
        tools.update(|next| {
            next.selection =
                tools::ToolSelectionUpdate::restore(&entries, next)?;
            Ok(())
        })?;
        Ok(tools)
    }
}

impl Kernel {
    /// Builds session-scoped tools and reconstructs model history from the active main branch.
    pub(in crate::runtime) async fn build_session(
        &self,
        store: Box<dyn SessionStore>,
        cwd: PathBuf,
        mcp_source: mcp::SessionMcpSource<'_>,
        ownership: Option<protocol::SessionOwnership>,
    ) -> Result<Arc<Session>, KernelError> {
        let session_id = store.session_id().clone();
        let log_extension_failure = |error: &::extension::ExtensionError| {
            tracing::error!(
                "failed extension registration for session {}: {}",
                session_id,
                error
            );
        };
        let modules = self
            .extension_factory
            .create_modules()
            .inspect_err(log_extension_failure)?;
        let mut registrar = ExtensionRegistrar::new();
        for module in modules {
            registrar
                .register_module(module.as_ref())
                .inspect_err(log_extension_failure)?;
        }
        let extensions = Arc::new(registrar.freeze());
        let mcp = mcp_source.acquire(self, &session_id, &cwd).await?;
        let cleanup_mcp = mcp.owned_session();
        let result = SessionAssembly::builder()
            .kernel(self)
            .session_id(session_id.clone())
            .store(store)
            .cwd(cwd)
            .ownership(ownership)
            .extensions(extensions)
            .build()
            .assemble(mcp)
            .await;
        if let Err(error) = &result {
            tracing::error!(
                "failed Session assembly for session {}: {}",
                session_id,
                error
            );
        }
        if result.is_err()
            && let Some(mcp) = cleanup_mcp
            && let Err(error) = mcp.shutdown().await
        {
            // Preserve the original construction error while making cleanup
            // failure observable for operators.
            tracing::warn!(
                "failed to shut down MCP after Session construction failed: {}",
                error
            );
        }
        result
    }
}

//! Production composition root for the modular agent backend.

mod logging;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use kernel::{
    Kernel, KernelFactory, ModelFactory, NanoidIdGenerator,
    ProviderModelFactory,
};
use mcp::{RmcpConnector, RuntimeMcpServer, SessionMcpFactory};
use prompt::FilesystemPromptFactory;
use protocol::ProductIdentity;
use skill::FilesystemSkillFactory;
use store::{FileSecretStore, JsonlStoreFactory, SystemClock};
use tools::BuiltinToolFactory;

pub use logging::{LoggingError, LoggingFactory};
pub use web::{
    ActiveModelInfo, LocalBindAddress, ProductInfo, UiBootstrapResponse,
    WebServerOptions,
};

/// Errors surfaced while composing retained config/provider modules with the new kernel.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// Immutable TOML configuration failed to load.
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    /// One configured MCP server could not be converted to a runtime transport.
    #[error(transparent)]
    McpConfig(#[from] config::McpConfigError),

    /// Kernel factory construction failed.
    #[error(transparent)]
    Kernel(#[from] kernel::KernelError),

    /// Static extension selection or construction failed.
    #[error(transparent)]
    Extension(#[from] extension::ExtensionError),

    /// Process working-directory lookup failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Restricted OAuth credential storage could not be initialized.
    #[error(transparent)]
    SecretStore(#[from] store::SecretStoreError),

    /// No platform data directory was available and no override was configured.
    #[error("no platform data directory is available")]
    MissingDataDirectory,

    /// No platform configuration directory was available for user skills.
    #[error("no platform configuration directory is available")]
    MissingConfigDirectory,

    /// No user home directory was available for portable Agent Skills.
    #[error("no user home directory is available")]
    MissingHomeDirectory,

    /// ACP HTTP/SSE/WebSocket router construction failed.
    #[error(transparent)]
    AcpTransport(#[from] acp::AcpTransportError),

    /// The unauthenticated local WebUI cannot listen beyond loopback.
    #[error("WebUI bind address must be loopback: {0}")]
    NonLoopbackBind(std::net::SocketAddr),
}

/// Fully composed application services shared by every ACP transport.
pub struct Application {
    /// Session-owning agent kernel.
    pub kernel: Arc<Kernel>,
    /// Immutable browser bootstrap response captured from startup config.
    pub ui_bootstrap: UiBootstrapResponse,
}

/// Composition factory that keeps construction policy outside domain modules.
pub struct ApplicationFactory {
    config: config::ConfigHandle,
}

impl ApplicationFactory {
    /// Loads the immutable TOML configuration using retained config semantics.
    pub fn load() -> Result<Self, ApplicationError> {
        Ok(Self {
            config: config::load()?,
        })
    }

    /// Creates a factory from an already loaded immutable configuration snapshot.
    #[must_use]
    pub fn new(config: config::ConfigHandle) -> Self {
        Self { config }
    }

    /// Builds provider, tool, MCP, skill, store, extension, and kernel factories.
    pub fn build(self) -> Result<Application, ApplicationError> {
        let snapshot = self.config.current();
        // ponytail: approval prompts are not implemented, so every tool runs
        // without asking whatever the policy says; wire ACP permission requests
        // into tool execution before honouring it.
        let approval = snapshot.effective_approval_policy();
        if approval != config::AskForApproval::Never {
            tracing::warn!(
                "approval policy {:?} is not enforced yet: tools run without asking for permission",
                approval
            );
        }
        // Resolve generated extension code before external factories so an
        // unavailable runtime selection fails startup without side effects.
        let extension_factory = extensions::compiled_extensions()?
            .select(&snapshot.extensions.enabled)?;
        let data_root = match &snapshot.session_persistence.data_home {
            Some(data_home) => PathBuf::from(data_home),
            None => dirs::data_local_dir()
                .ok_or(ApplicationError::MissingDataDirectory)?
                .join(ProductIdentity::CONFIG_DIR_NAME),
        };
        let sessions_root = data_root.join("sessions");
        let secret_store: Arc<dyn store::SecretStore> =
            Arc::new(FileSecretStore::new(data_root.join("credentials"))?);
        let config_root = dirs::config_dir()
            .ok_or(ApplicationError::MissingConfigDirectory)?
            .join(ProductIdentity::CONFIG_DIR_NAME);
        let cwd = std::env::current_dir()?;
        // The provider layer completes model metadata from the rig catalog.
        let model_factory =
            Arc::new(ProviderModelFactory::from_config(self.config.clone()));
        let model_profile = model_factory
            .create()
            .map_err(kernel::KernelError::from)?
            .profile()
            .clone();
        let active_model_id =
            format!("{}/{}", model_profile.provider_id, model_profile.model_id);
        let ui_bootstrap = UiBootstrapResponse::builder()
            .when_busy(snapshot.kernel.when_busy)
            .product(ProductInfo {
                name: ProductIdentity::NAME.to_string(),
                slug: ProductIdentity::SLUG.to_string(),
            })
            .acp_path(ProductIdentity::DEFAULT_ACP_PATH.to_string())
            .default_cwd(cwd.to_string_lossy().into_owned())
            .active_model(ActiveModelInfo {
                id: active_model_id,
                display_name: model_profile.display_name,
            })
            .build();
        let mcp_servers = snapshot
            .mcp_servers
            .clone()
            .into_iter()
            .map(RuntimeMcpServer::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let builtin_tools = BuiltinToolFactory::new()
            .filesystem_enabled(snapshot.tools.enable_fs);
        // Home discovery is a Skill dependency and must not block deployments
        // that explicitly disable the Skill subsystem.
        let skill_factory = if snapshot.tools.enable_skill {
            let user_home = dirs::home_dir()
                .ok_or(ApplicationError::MissingHomeDirectory)?;
            Some(Arc::new(
                FilesystemSkillFactory::builder()
                    .global_root(config_root.clone())
                    .user_home(user_home)
                    .configured_paths(snapshot.skills.paths.clone())
                    .rules(snapshot.skills.rules.clone())
                    .build(),
            ) as Arc<dyn skill::SkillFactory>)
        } else {
            None
        };
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let kernel = KernelFactory::builder()
            .model_factory(model_factory)
            .tool_factory(Arc::new(builtin_tools))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                sessions_root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(extension_factory))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                config_root,
                snapshot.prompt.clone(),
            )))
            .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
                mcp_servers,
                Arc::new(RmcpConnector::new(secret_store)),
            ))))
            .skill_factory(skill_factory)
            .include_skill_instructions(snapshot.skills.include_instructions)
            .max_turns(snapshot.kernel.max_turns)
            .when_busy(snapshot.kernel.when_busy.into())
            .subagent_max_agents(snapshot.subagent.max_agents)
            .compaction_policy(snapshot.compaction.clone())
            .branch_summary(snapshot.branch_summary)
            .retry_policy(snapshot.retry.model_policy())
            .provider_retry_delay_limit_ms(
                snapshot.retry.provider.max_retry_delay_ms,
            )
            .exec(snapshot.tools.enable_shell.then(|| {
                Arc::new(tools::exec::ExecSettings {
                    config: snapshot.tools.exec.clone(),
                    snapshot_dir: data_root.join("shell_snapshots"),
                })
            }))
            .build()
            .build()?;
        Ok(Application {
            kernel: Arc::new(kernel),
            ui_bootstrap,
        })
    }
}

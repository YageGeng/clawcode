//! Production composition root for the modular agent backend.

mod logging;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use kernel::{
    Kernel, KernelFactory, NanoidIdGenerator, PiSystemPromptFactory,
    ProviderModelFactory,
};
use mcp::{RmcpConnector, RuntimeMcpServer, SessionMcpFactory};
use protocol::{IdGenerator, ProductIdentity};
use skill::FilesystemSkillFactory;
use store::{JsonlStoreFactory, SystemClock};
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

    /// No platform data directory was available and no override was configured.
    #[error("no platform data directory is available")]
    MissingDataDirectory,

    /// No platform configuration directory was available for user skills.
    #[error("no platform configuration directory is available")]
    MissingConfigDirectory,

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
    /// Shared identifier policy used for independent ACP operation traces.
    pub id_generator: Arc<dyn IdGenerator>,
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
        // Resolve generated extension code before external factories so an
        // unavailable runtime selection fails startup without side effects.
        let extension_factory = extensions::compiled_extensions()?
            .select(&snapshot.extensions.enabled)?;
        let sessions_root = match &snapshot.session_persistence.data_home {
            Some(data_home) => PathBuf::from(data_home).join("sessions"),
            None => dirs::data_local_dir()
                .ok_or(ApplicationError::MissingDataDirectory)?
                .join(ProductIdentity::CONFIG_DIR_NAME)
                .join("sessions"),
        };
        let config_root = dirs::config_dir()
            .ok_or(ApplicationError::MissingConfigDirectory)?
            .join(ProductIdentity::CONFIG_DIR_NAME);
        let cwd = std::env::current_dir()?;
        let model_profile = snapshot.model_profile().map_err(|error| {
            ApplicationError::Config(config::ConfigError::Validation(error))
        })?;
        let active_model_id =
            format!("{}/{}", model_profile.provider_id, model_profile.model_id);
        let ui_bootstrap = UiBootstrapResponse::builder()
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
        let skill_roots = vec![
            config_root.join("skills"),
            cwd.join(".pi").join("skills"),
            cwd.join(".agents").join("skills"),
        ];
        let mcp_servers = snapshot
            .mcp_servers
            .clone()
            .into_iter()
            .map(RuntimeMcpServer::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let builtin_tools = BuiltinToolFactory::new()
            .filesystem_enabled(snapshot.tools.enable_fs)
            .shell_enabled(snapshot.tools.enable_shell);
        let skill_factory = snapshot.tools.enable_skill.then(|| {
            Arc::new(FilesystemSkillFactory::new(skill_roots))
                as Arc<dyn skill::SkillFactory>
        });
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let id_generator: Arc<dyn IdGenerator> = Arc::new(NanoidIdGenerator);
        let kernel = KernelFactory::builder()
            .model_factory(Arc::new(ProviderModelFactory::from_config(
                self.config,
            )))
            .tool_factory(Arc::new(builtin_tools))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                sessions_root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(extension_factory))
            .clock(clock)
            .id_generator(Arc::clone(&id_generator))
            .system_prompt_factory(Arc::new(PiSystemPromptFactory::new(
                config_root,
            )))
            .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
                mcp_servers,
                Arc::new(RmcpConnector),
            ))))
            .skill_factory(skill_factory)
            .compaction_policy(snapshot.compaction)
            .retry_policy(snapshot.retry.agent)
            .build()
            .build()?;
        Ok(Application {
            kernel: Arc::new(kernel),
            id_generator,
            ui_bootstrap,
        })
    }
}

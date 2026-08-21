//! Configuration crate: typed TOML configuration and immutable loading.

pub mod app;
pub mod approval;
pub mod config;
pub mod extensions;
pub mod hook;
pub mod kernel;
pub mod llm;
pub mod loader;
pub mod logging;
pub mod mcp;
pub mod prompt;
pub mod retry;
pub mod skills;
pub mod tools;

pub use app::{AppSectionConfig, RecoryConfig};
pub use approval::{ApprovalMode, AskForApproval, GranularApprovalConfig};
pub use config::{
    AppConfig, CompactionConfig, ConfigValidationError,
    SessionPersistenceConfig,
};
pub use extensions::{DEFAULT_EXTENSION_ID, ExtensionsConfig};
pub use hook::{HookEventsToml, HookHandlerConfig, HooksFile, MatcherGroup};
pub use kernel::{KernelConfig, TurnLimit};
pub use llm::{
    ApiKeyConfig, LlmModel, LlmProvider, ProviderAuthConfig, ProviderId,
    ProviderType,
};
pub use loader::{ConfigError, ConfigHandle, load, load_from};
pub use logging::LoggingConfig;
pub use mcp::{McpConfigError, McpOAuthConfig, McpServerConfig};
pub use prompt::PromptPolicy;
pub use retry::{ProviderRetryConfig, RetryConfig};
pub use skills::{SkillSelectionRule, SkillsConfig};
pub use tools::ToolsConfig;

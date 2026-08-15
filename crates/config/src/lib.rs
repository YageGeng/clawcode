//! Configuration crate: typed TOML configuration and immutable loading.

pub mod approval;
pub mod config;
pub mod extensions;
pub mod hook;
pub mod llm;
pub mod loader;
pub mod mcp;
pub mod retry;
pub mod skills;
pub mod tools;

pub use approval::{ApprovalMode, AskForApproval, GranularApprovalConfig};
pub use config::{
    AppConfig, CompactionConfig, ConfigValidationError,
    SessionPersistenceConfig,
};
pub use extensions::{DEFAULT_EXTENSION_ID, ExtensionsConfig};
pub use hook::{HookEventsToml, HookHandlerConfig, HooksFile, MatcherGroup};
pub use llm::{
    ApiKeyConfig, LlmModel, LlmProvider, ProviderAuthConfig, ProviderId,
    ProviderType,
};
pub use loader::{ConfigError, ConfigHandle, load, load_from};
pub use mcp::{McpConfigError, McpOAuthConfig, McpServerConfig};
pub use retry::{ProviderRetryConfig, RetryConfig};
pub use skills::SkillsConfig;
pub use tools::ToolsConfig;

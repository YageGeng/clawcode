//! Configuration crate: typed config + figment loader + ArcSwap-shared handle.

pub mod agent;
pub mod config;
pub mod llm;
pub mod loader;
pub mod mcp;
pub mod skills;

pub use agent::MultiAgentConfig;
pub use config::{AppConfig, SessionPersistenceConfig};
pub use llm::{ApiKeyConfig, LlmModel, LlmProvider, ProviderAuthConfig, ProviderId, ProviderType};
pub use loader::{ConfigError, ConfigHandle, load, load_from};
pub use protocol::ApprovalMode;
pub use skills::SkillsConfig;

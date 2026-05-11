//! Configuration crate: typed config + figment loader + ArcSwap-shared handle.

pub mod config;
pub mod llm;
pub mod loader;

pub use config::AppConfig;
pub use llm::{ApiKeyConfig, LlmModel, LlmProvider, ProviderId, ProviderType};
pub use loader::{ConfigError, ConfigHandle, load, load_from};

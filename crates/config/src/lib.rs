//! Configuration crate: typed config + figment loader + ArcSwap-shared handle.

pub mod llm;
pub mod loader;

pub use llm::{AppConfig, LlmConfig, LlmModel, LlmProvider};
pub use loader::{ConfigError, ConfigHandle, load, load_from};

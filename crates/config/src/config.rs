//! Top-level application configuration types.

use serde::{Deserialize, Serialize};

use crate::llm::LlmProvider;

/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Configured LLM providers.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
    /// Active model in `provider_id/model_id` format (e.g. "deepseek/deepseek-v4-flash").
    #[serde(default = "default_active_model")]
    pub active_model: String,
}

fn default_active_model() -> String {
    "deepseek/deepseek-v4-flash".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            active_model: default_active_model(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AppConfig defaults to an empty providers list.
    #[test]
    fn app_config_default_is_empty() {
        let cfg = AppConfig::default();
        assert!(cfg.providers.is_empty());
    }
}

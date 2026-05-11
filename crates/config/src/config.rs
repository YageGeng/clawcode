//! Top-level application configuration types.

use serde::{Deserialize, Serialize};

use crate::llm::LlmProvider;

/// Top-level application configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Configured LLM providers.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
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

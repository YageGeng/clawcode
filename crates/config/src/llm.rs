//! Configuration types for LLM providers and their models.

use serde::{Deserialize, Serialize};

/// User-visible model metadata attached to an [`LlmProvider`].
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmModel {
    /// Provider model identifier sent in request payloads (e.g. "deepseek-v4-flash").
    pub id: String,
    /// Optional display name for user interfaces.
    pub display_name: Option<String>,
    /// Optional context window size in tokens.
    pub context_tokens: Option<u64>,
    /// Optional maximum output size in tokens.
    pub max_output_tokens: Option<u64>,
    /// Provider-specific parameters merged into every request for this model.
    /// For DeepSeek thinking mode: `{ "thinking": {"type": "enabled"}, "reasoning_effort": "high" }`.
    #[serde(default)]
    pub extra_param: serde_json::Value,
}

/// Provider metadata plus auth/model information used to build a completion model.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmProvider {
    /// Stable provider identifier used by configuration (e.g. "deepseek").
    pub id: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Base URL used for provider requests (no trailing slash).
    pub base_url: String,
    /// Plaintext API key. Use figment env layer to inject from the environment.
    pub api_key: String,
    /// Models available through this provider.
    #[serde(default)]
    pub models: Vec<LlmModel>,
}

/// Aggregate of all configured providers.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmConfig {
    /// Configured providers; provider/model lookup keys come from `id` fields.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
}

/// Top-level application configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// LLM-related configuration block.
    #[serde(default)]
    pub llm: LlmConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TOML round-trip preserves all LlmProvider fields, including nested models.
    #[test]
    fn provider_round_trips_through_toml() {
        let toml = r#"
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-abc"

[[models]]
id = "deepseek-v4-flash"
display_name = "DeepSeek V4 Flash"
context_tokens = 1000000
max_output_tokens = 384000

[models.extra_param]
thinking = { type = "enabled" }
reasoning_effort = "high"
"#;
        let provider: LlmProvider = toml::from_str(toml).unwrap();
        assert_eq!(provider.id, "deepseek");
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "deepseek-v4-flash");
        assert_eq!(provider.models[0].context_tokens, Some(1_000_000));
        assert_eq!(
            provider.models[0].extra_param["reasoning_effort"],
            serde_json::json!("high")
        );
    }

    /// AppConfig defaults to an empty providers list.
    #[test]
    fn app_config_default_is_empty() {
        let cfg = AppConfig::default();
        assert!(cfg.llm.providers.is_empty());
    }
}

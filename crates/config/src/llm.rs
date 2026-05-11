//! Configuration types for LLM providers and their models.

use serde::{Deserialize, Serialize};

/// Stable provider identifier used by configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(from = "String", into = "String")]
pub enum ProviderId {
    /// OpenAI-hosted models.
    Openai,
    /// ChatGPT access-token backed endpoint.
    Chatgpt,
    /// DeepSeek provider.
    Deepseek,
    /// Moonshot provider.
    Moonshot,
    /// MiniMax provider.
    Minimax,
    /// Xiaomi Mimo provider.
    Xiaomimimo,
    /// Anthropic provider.
    Anthropic,
    /// Catch-all for providers not modeled yet.
    Other(String),
}

impl ProviderId {
    /// Returns the provider identifier as a string slice.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Openai => "openai",
            Self::Chatgpt => "chatgpt",
            Self::Deepseek => "deepseek",
            Self::Moonshot => "moonshot",
            Self::Minimax => "minimax",
            Self::Xiaomimimo => "xiaomimimo",
            Self::Anthropic => "anthropic",
            Self::Other(id) => id.as_str(),
        }
    }
}

impl From<String> for ProviderId {
    /// Parses a provider identifier from its string form.
    fn from(value: String) -> Self {
        match value.as_str() {
            "openai" => Self::Openai,
            "chatgpt" => Self::Chatgpt,
            "deepseek" => Self::Deepseek,
            "moonshot" => Self::Moonshot,
            "minimax" => Self::Minimax,
            "xiaomimimo" => Self::Xiaomimimo,
            "anthropic" => Self::Anthropic,
            _ => Self::Other(value),
        }
    }
}

impl From<&str> for ProviderId {
    /// Parses a provider identifier from a borrowed string slice.
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl From<ProviderId> for String {
    /// Serializes a provider identifier back into its string form.
    fn from(value: ProviderId) -> Self {
        match value {
            ProviderId::Openai => "openai".to_string(),
            ProviderId::Chatgpt => "chatgpt".to_string(),
            ProviderId::Deepseek => "deepseek".to_string(),
            ProviderId::Moonshot => "moonshot".to_string(),
            ProviderId::Minimax => "minimax".to_string(),
            ProviderId::Xiaomimimo => "xiaomimimo".to_string(),
            ProviderId::Anthropic => "anthropic".to_string(),
            ProviderId::Other(id) => id,
        }
    }
}

/// Identifies the API protocol used by a provider.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    /// OpenAI Responses API.
    #[default]
    Responses,
    /// OpenAI Chat Completions API (or compatible).
    OpenaiCompletions,
    /// Anthropic Messages API (or compatible).
    Anthropic,
}

/// API key source used by a configured provider.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ApiKeyConfig {
    /// Plaintext API key stored directly in the config file.
    Plaintext(String),
    /// API key loaded from an environment variable at runtime.
    Env {
        /// Environment variable name containing the API key.
        env: String,
    },
}

impl ApiKeyConfig {
    /// Resolve the configured API key source into a concrete string.
    pub fn resolve(&self) -> Result<String, std::env::VarError> {
        match self {
            Self::Plaintext(value) => Ok(value.clone()),
            Self::Env { env } => std::env::var(env),
        }
    }
}

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
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmProvider {
    /// Stable provider identifier used by configuration (e.g. "deepseek").
    pub id: ProviderId,
    /// Human-readable provider name.
    pub display_name: String,
    /// API protocol used by this provider entry.
    #[serde(default)]
    pub provider_type: ProviderType,
    /// Base URL used for provider requests (no trailing slash).
    pub base_url: String,
    /// API key source. This can be a plaintext string or `{ env = "NAME" }`.
    pub api_key: ApiKeyConfig,
    /// Models available through this provider.
    #[serde(default)]
    pub models: Vec<LlmModel>,
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
provider_type = "openai-completions"
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
        assert_eq!(provider.id, ProviderId::Deepseek);
        assert_eq!(provider.provider_type, ProviderType::OpenaiCompletions);
        assert_eq!(
            provider.api_key,
            ApiKeyConfig::Plaintext("sk-abc".to_string())
        );
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "deepseek-v4-flash");
        assert_eq!(provider.models[0].context_tokens, Some(1_000_000));
        assert_eq!(
            provider.models[0].extra_param["reasoning_effort"],
            serde_json::json!("high")
        );
    }

    /// ProviderType defaults to Responses.
    #[test]
    fn provider_type_default_is_responses() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "openai"
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "sk-test"
"#,
        )
        .unwrap();
        assert_eq!(provider.id, ProviderId::Openai);
        assert_eq!(provider.provider_type, ProviderType::Responses);
    }

    /// Unknown provider ids deserialize into ProviderId::Other.
    #[test]
    fn provider_id_preserves_unknown_values() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "custom-provider"
display_name = "Custom"
base_url = "https://example.com"
api_key = "sk-test"
"#,
        )
        .unwrap();
        assert_eq!(
            provider.id,
            ProviderId::Other("custom-provider".to_string())
        );
        assert_eq!(provider.id.as_str(), "custom-provider");
    }

    /// API keys can be loaded from environment variable references.
    #[test]
    fn api_key_supports_environment_reference() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "openai"
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"

[api_key]
env = "OPENAI_API_KEY"
"#,
        )
        .unwrap();
        assert_eq!(
            provider.api_key,
            ApiKeyConfig::Env {
                env: "OPENAI_API_KEY".to_string(),
            }
        );
    }
}

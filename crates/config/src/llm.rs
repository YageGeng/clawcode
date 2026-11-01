//! Configuration types for LLM providers and their models.

use serde::{Deserialize, Serialize};

use crate::config::ConfigValidationError;
use protocol::{ModelInputModalities, ModelInputModality};

/// Local provider identifier chosen in configuration and used in `provider/model` selections.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Returns the identifier as written in configuration.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ProviderId {
    /// Wraps a configured identifier; `LlmProvider::validate_names` checks its shape.
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ProviderId {
    /// Wraps a borrowed configured identifier.
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Identifies the API protocol used by a provider.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    /// OpenAI Responses API.
    Responses,
    /// OpenAI Chat Completions API (or compatible).
    OpenaiCompletions,
    /// Anthropic Messages API (or compatible).
    Anthropic,
    /// Gemini GenerateContent API (or compatible).
    Gemini,
}

impl ProviderType {
    /// Returns the configuration spelling of this protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::OpenaiCompletions => "openai-completions",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }
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

    /// Validates that an API key source resolves without exposing its value.
    pub fn validate_source(
        &self,
        provider_id: &str,
    ) -> Result<(), ConfigValidationError> {
        let key_source = match self {
            Self::Plaintext(value) if !value.trim().is_empty() => return Ok(()),
            Self::Plaintext(_) => "plaintext".to_string(),
            Self::Env { env } => match std::env::var(env) {
                Ok(value) if !value.trim().is_empty() => return Ok(()),
                Ok(_) | Err(_) => env.clone(),
            },
        };

        Err(ConfigValidationError::AuthResolution {
            provider_id: provider_id.to_string(),
            key_source,
        })
    }
}

/// Provider auth configuration for specialized auth workflows.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProviderAuthConfig {
    /// Use the upstream ChatGPT authenticator and its independent token store.
    Chatgpt {
        /// Explicit absolute directory for upstream chatgpt.json credentials.
        token_store_dir: std::path::PathBuf,
    },
}

/// User-visible model metadata attached to an [`LlmProvider`].
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct LlmModel {
    /// Stable local model identifier used for selection and persisted metadata.
    pub id: String,
    /// Optional provider-facing model identifier sent in API request payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub upstream_id: Option<String>,
    /// Optional display name for user interfaces.
    #[builder(default)]
    pub display_name: Option<String>,
    /// Optional context window size in tokens.
    #[builder(default)]
    pub context_tokens: Option<u64>,
    /// Optional maximum output size in tokens; capability metadata, never sent by itself.
    #[builder(default)]
    pub max_output_tokens: Option<u64>,
    /// Output tokens requested per response; at most `compaction.reserve_tokens` and
    /// `max_output_tokens`. Omitted uses the smaller of those two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub request_max_tokens: Option<u64>,
    /// Ordered input modalities accepted by this model; omitted uses the rig catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub input: Option<Vec<ModelInputModality>>,
    /// Provider-specific parameters merged into every request for this model.
    /// For DeepSeek thinking mode: `{ "thinking": {"type": "enabled"}, "reasoning_effort": "high" }`.
    #[serde(default)]
    #[builder(default)]
    pub extra_param: serde_json::Value,
}

/// Provider metadata plus auth/model information used to build a completion model.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct LlmProvider {
    /// Stable local provider identifier used in `provider/model` selections.
    pub id: ProviderId,
    /// rig preset name that selects dialect details and catalog metadata; defaults to `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub preset: Option<String>,
    /// Human-readable provider name.
    pub display_name: String,
    /// API protocol; omitted uses the preset's rig default, or Responses for unknown presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub provider_type: Option<ProviderType>,
    /// Base URL for provider requests; omitted uses the rig preset's endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub base_url: Option<String>,
    /// API key source. This can be a plaintext string or `{ env = "NAME" }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub api_key: Option<ApiKeyConfig>,
    /// Provider-level auth strategy for non-API-key flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub auth: Option<ProviderAuthConfig>,
    /// Models available through this provider.
    #[serde(default)]
    #[builder(default)]
    pub models: Vec<LlmModel>,
}

impl LlmModel {
    /// Validates the values this model sets; omitted metadata is resolved later from the rig catalog.
    pub fn validate(
        &self,
        provider: &LlmProvider,
    ) -> Result<(), ConfigValidationError> {
        let provider_id = provider.id.as_str();
        if self
            .upstream_id
            .as_ref()
            .is_some_and(|upstream_id| upstream_id.trim().is_empty())
        {
            return Err(ConfigValidationError::InvalidUpstreamModelId {
                provider_id: provider_id.to_string(),
                model_id: self.id.clone(),
            });
        }
        for (field, value) in [
            ("context_tokens", self.context_tokens),
            ("max_output_tokens", self.max_output_tokens),
            ("request_max_tokens", self.request_max_tokens),
        ] {
            if value == Some(0) {
                return Err(ConfigValidationError::ZeroModelLimit {
                    provider_id: provider_id.to_string(),
                    model_id: self.id.clone(),
                    field,
                });
            }
        }
        if let Some(input) = &self.input {
            ModelInputModalities::try_from(input.clone()).map_err(
                |reason| ConfigValidationError::InvalidModelInput {
                    provider_id: provider_id.to_string(),
                    model_id: self.id.clone(),
                    reason,
                },
            )?;
        }
        Ok(())
    }
}

impl Default for LlmModel {
    /// Creates an empty model configuration; omitted metadata comes from the rig catalog.
    fn default() -> Self {
        Self::builder().id(String::new()).build()
    }
}

impl LlmProvider {
    /// Returns the rig preset name, which defaults to the local identifier.
    pub fn preset(&self) -> &str {
        self.preset.as_deref().unwrap_or(self.id.as_str())
    }

    /// Rejects an `id` or `preset` that is empty, padded, or contains `/`.
    ///
    /// `/` separates provider and model in selections, and a padded preset would
    /// silently miss rig's presets.
    pub fn validate_names(&self) -> Result<(), ConfigValidationError> {
        let names = [
            ("id", Some(self.id.as_str())),
            ("preset", self.preset.as_deref()),
        ];
        for (field, value) in names {
            if let Some(value) = value.filter(|value| {
                value.is_empty()
                    || value.trim() != *value
                    || value.contains('/')
            }) {
                return Err(ConfigValidationError::InvalidProviderName {
                    provider_id: self.id.as_str().to_owned(),
                    field,
                    value: value.to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Validates unique model identifiers and every configured model value.
    pub fn validate_models(&self) -> Result<(), ConfigValidationError> {
        let mut model_ids = std::collections::HashSet::new();
        for model in &self.models {
            if !model_ids.insert(model.id.as_str()) {
                return Err(ConfigValidationError::DuplicateModel {
                    provider_id: self.id.as_str().to_string(),
                    model_id: model.id.clone(),
                });
            }
            model.validate(self)?;
        }

        Ok(())
    }

    /// Validates that the provider has a resolvable authentication source.
    pub fn validate_auth_source(&self) -> Result<(), ConfigValidationError> {
        if let Some(ProviderAuthConfig::Chatgpt { token_store_dir }) =
            &self.auth
        {
            let reason = if self.preset() != "chatgpt"
                || !matches!(
                    self.provider_type,
                    None | Some(ProviderType::Responses)
                ) {
                Some(
                    "chatgpt authentication requires the ChatGPT Responses provider",
                )
            } else if self.api_key.is_some() {
                Some(
                    "choose an explicit access token or ChatGPT authentication, not both",
                )
            } else if !token_store_dir.is_absolute()
                || token_store_dir.is_file()
            {
                Some(
                    "token_store_dir must be an absolute directory; Codex auth.json is not imported",
                )
            } else {
                None
            };
            return reason.map_or(Ok(()), |reason| {
                Err(ConfigValidationError::InvalidAuthentication {
                    provider_id: self.id.as_str().to_owned(),
                    reason: reason.to_owned(),
                })
            });
        }
        if let Some(api_key) = &self.api_key {
            return api_key.validate_source(self.id.as_str());
        }

        Err(ConfigValidationError::MissingAuthentication {
            provider_id: self.id.as_str().to_string(),
        })
    }
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
        assert_eq!(provider.id, ProviderId::from("deepseek"));
        assert_eq!(
            provider.provider_type,
            Some(ProviderType::OpenaiCompletions)
        );
        assert_eq!(
            provider.api_key,
            Some(ApiKeyConfig::Plaintext("sk-abc".to_string()))
        );
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "deepseek-v4-flash");
        assert_eq!(provider.models[0].context_tokens, Some(1_000_000));
        assert_eq!(
            provider.models[0].extra_param["reasoning_effort"],
            serde_json::json!("high")
        );
    }

    /// An omitted protocol deserializes as unset.
    #[test]
    fn provider_type_is_optional() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "openai"
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "sk-test"
"#,
        )
        .unwrap();
        assert_eq!(provider.id, ProviderId::from("openai"));
        // An omitted protocol stays unset until the provider layer resolves the preset.
        assert_eq!(provider.provider_type, None);
    }

    /// Any provider id round-trips as the string it was configured with.
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
        assert_eq!(provider.id, ProviderId::from("custom-provider"));
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
            Some(ApiKeyConfig::Env {
                env: "OPENAI_API_KEY".to_string(),
            })
        );
    }

    /// auth = { type = "chatgpt", token_store_dir = "/tmp/clawcode-auth" } parses without an api_key.
    #[test]
    fn provider_supports_codex_auth_without_api_key() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "chatgpt"
display_name = "ChatGPT"
provider_type = "responses"
base_url = "https://chatgpt.com/backend-api/codex"
auth = { type = "chatgpt", token_store_dir = "/tmp/clawcode-auth" }

[[models]]
id = "gpt-5.4"
"#,
        )
        .unwrap();

        assert_eq!(provider.id, ProviderId::from("chatgpt"));
        assert!(provider.api_key.is_none());
        assert_eq!(
            provider.auth,
            Some(ProviderAuthConfig::Chatgpt {
                token_store_dir: "/tmp/clawcode-auth".into()
            })
        );
    }

    /// An explicit upstream token-store directory is retained without Codex compatibility.
    #[test]
    fn provider_parses_chatgpt_token_store_override() {
        let provider: LlmProvider = toml::from_str(
            r#"
id = "chatgpt"
display_name = "ChatGPT"
provider_type = "responses"
base_url = "https://chatgpt.com/backend-api/codex"
auth = { type = "chatgpt", token_store_dir = "/tmp/custom-auth" }

[[models]]
id = "gpt-5.4"
"#,
        )
        .unwrap();

        assert_eq!(
            provider.auth,
            Some(ProviderAuthConfig::Chatgpt {
                token_store_dir: "/tmp/custom-auth".into(),
            })
        );
    }
}

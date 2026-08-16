//! Top-level application configuration types.

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalMode, AskForApproval};
use crate::extensions::ExtensionsConfig;
use crate::llm::{LlmModel, LlmProvider};
use crate::logging::LoggingConfig;
use crate::mcp::McpServerConfig;
use crate::prompt::PromptPolicy;
use crate::retry::RetryConfig;
use crate::skills::SkillsConfig;
use crate::tools::ToolsConfig;
pub use protocol::CompactionPolicy as CompactionConfig;
use protocol::ModelProfile;

/// File-backed session persistence settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionPersistenceConfig {
    /// Optional override for the data directory that stores session transcripts.
    #[serde(default)]
    pub data_home: Option<String>,
}

/// Cross-field validation failures detected after TOML extraction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigValidationError {
    /// The active model did not contain a provider and model identifier.
    #[error("active model '{value}' must use provider/model format")]
    MalformedActiveModel {
        /// Invalid configured active model value.
        value: String,
    },
    /// No provider matched the active provider identifier.
    #[error("active provider '{provider_id}' is not configured")]
    MissingProvider {
        /// Provider identifier referenced by the active model.
        provider_id: String,
    },
    /// More than one provider used the active provider identifier.
    #[error("provider identifier '{provider_id}' is configured more than once")]
    DuplicateProvider {
        /// Duplicated provider identifier.
        provider_id: String,
    },
    /// No model matched the active model identifier.
    #[error("active model '{provider_id}/{model_id}' is not configured")]
    MissingModel {
        /// Provider identifier containing the expected model.
        provider_id: String,
        /// Missing model identifier.
        model_id: String,
    },
    /// More than one model used the same identifier under a provider.
    #[error(
        "model identifier '{provider_id}/{model_id}' is configured more than once"
    )]
    DuplicateModel {
        /// Provider identifier containing the duplicate.
        provider_id: String,
        /// Duplicated model identifier.
        model_id: String,
    },
    /// A model omitted a limit required by the runtime.
    #[error(
        "model '{provider_id}/{model_id}' is missing required limit '{field}'"
    )]
    MissingModelLimit {
        /// Provider identifier containing the model.
        provider_id: String,
        /// Model missing the required limit.
        model_id: String,
        /// Name of the missing limit field.
        field: &'static str,
    },
    /// A required model limit was explicitly configured as zero.
    #[error(
        "model '{provider_id}/{model_id}' limit '{field}' must be greater than zero"
    )]
    ZeroModelLimit {
        /// Provider identifier containing the model.
        provider_id: String,
        /// Model containing the zero limit.
        model_id: String,
        /// Name of the invalid limit field.
        field: &'static str,
    },
    /// Exponential retry delay arithmetic would overflow milliseconds.
    #[error(
        "retry backoff overflows milliseconds for base_delay_ms={base_delay_ms} and max_retries={max_retries}"
    )]
    InvalidRetryDuration {
        /// Configured initial delay.
        base_delay_ms: u64,
        /// Configured retry count.
        max_retries: u32,
    },
    /// Reserved compaction tokens consume the complete model context window.
    #[error(
        "compaction reserve_tokens={reserve_tokens} must be less than context_tokens={context_tokens}"
    )]
    CompactionReserveOverflow {
        /// Configured reserve token count.
        reserve_tokens: u64,
        /// Active model context window.
        context_tokens: u64,
    },
    /// Provider authentication configuration is absent.
    #[error("provider '{provider_id}' has no authentication source")]
    MissingAuthentication {
        /// Provider missing authentication configuration.
        provider_id: String,
    },
    /// A configured API key source could not be resolved safely.
    #[error(
        "provider '{provider_id}' could not resolve API key source '{key_source}'"
    )]
    AuthResolution {
        /// Provider whose API key source failed.
        provider_id: String,
        /// Plain source label or environment variable name, never a secret value.
        key_source: String,
    },
}

/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AppConfig {
    /// Immutable process logging configuration.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Prompt resource discovery and rendering policy.
    #[serde(default)]
    pub prompt: PromptPolicy,
    /// Configured LLM providers.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
    /// Active model in `provider_id/model_id` format (e.g. "deepseek/deepseek-v4-flash").
    #[serde(default = "default_active_model")]
    pub active_model: String,
    /// Tool-approval behaviour.
    #[serde(default)]
    pub approval: ApprovalMode,
    /// enhanced tool approval policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    /// Skill subsystem configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Built-in tool registration configuration.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// Static Rust extensions selected in hook execution order.
    #[serde(default)]
    pub extensions: ExtensionsConfig,
    /// MCP server configurations.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// File-backed session persistence configuration.
    #[serde(default)]
    pub session_persistence: SessionPersistenceConfig,
    /// Manual context compaction configuration.
    #[serde(default)]
    pub compaction: CompactionConfig,
    /// Assistant and provider retry configuration.
    #[serde(default)]
    pub retry: RetryConfig,
}

fn default_active_model() -> String {
    "deepseek/deepseek-v4-flash".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            logging: LoggingConfig::default(),
            prompt: PromptPolicy::default(),
            providers: Vec::new(),
            active_model: default_active_model(),
            approval: ApprovalMode::default(),
            approval_policy: None,
            skills: SkillsConfig::default(),
            tools: ToolsConfig::default(),
            extensions: ExtensionsConfig::default(),
            mcp_servers: Vec::new(),
            session_persistence: SessionPersistenceConfig::default(),
            compaction: CompactionConfig::default(),
            retry: RetryConfig::default(),
        }
    }
}

impl AppConfig {
    /// Return the provider id portion of the configured active model.
    pub fn active_provider_id(&self) -> String {
        self.active_model
            .split_once('/')
            .map(|(provider_id, _)| provider_id.to_string())
            .unwrap_or_default()
    }

    /// Resolves the uniquely configured active provider and model.
    pub fn resolve_active_model(
        &self,
    ) -> Result<(&LlmProvider, &LlmModel), ConfigValidationError> {
        let (provider_id, model_id) = self
            .active_model
            .split_once('/')
            .filter(|(provider_id, model_id)| {
                !provider_id.is_empty() && !model_id.is_empty()
            })
            .ok_or_else(|| ConfigValidationError::MalformedActiveModel {
                value: self.active_model.clone(),
            })?;
        let mut providers = self
            .providers
            .iter()
            .filter(|provider| provider.id.as_str() == provider_id);
        let provider = providers.next().ok_or_else(|| {
            ConfigValidationError::MissingProvider {
                provider_id: provider_id.to_string(),
            }
        })?;
        if providers.next().is_some() {
            return Err(ConfigValidationError::DuplicateProvider {
                provider_id: provider_id.to_string(),
            });
        }

        let mut models =
            provider.models.iter().filter(|model| model.id == model_id);
        let model = models.next().ok_or_else(|| {
            ConfigValidationError::MissingModel {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
            }
        })?;
        if models.next().is_some() {
            return Err(ConfigValidationError::DuplicateModel {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
            });
        }

        Ok((provider, model))
    }

    /// Builds the stable runtime profile for the configured active model.
    pub fn model_profile(&self) -> Result<ModelProfile, ConfigValidationError> {
        let (provider, model) = self.resolve_active_model()?;
        model.to_profile(provider)
    }

    /// Return the enhanced approval policy after applying legacy compatibility.
    #[must_use]
    pub fn effective_approval_policy(&self) -> AskForApproval {
        self.approval_policy
            .unwrap_or_else(|| AskForApproval::from(self.approval))
    }

    /// Validate cross-field invariants that serde cannot express directly.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        for provider in &self.providers {
            provider.validate_models()?;
            provider.validate_auth_source()?;
        }

        let profile = self.model_profile()?;
        if !self.retry.has_valid_backoff() {
            return Err(ConfigValidationError::InvalidRetryDuration {
                base_delay_ms: self.retry.agent.base_delay_ms,
                max_retries: self.retry.agent.max_retries,
            });
        }
        if self.compaction.reserve_tokens >= profile.context_tokens {
            return Err(ConfigValidationError::CompactionReserveOverflow {
                reserve_tokens: self.compaction.reserve_tokens,
                context_tokens: profile.context_tokens,
            });
        }

        Ok(())
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

    /// AppConfig enables all built-in tool groups by default.
    #[test]
    fn app_config_default_tools_are_enabled() {
        let cfg = AppConfig::default();

        assert!(cfg.tools.enable_fs);
        assert!(cfg.tools.enable_shell);
        assert!(cfg.tools.enable_skill);
    }

    /// AppConfig reads built-in tool switches from the nested tools section.
    #[test]
    fn app_config_reads_tools_switches() {
        let cfg: AppConfig = toml::from_str(
            r#"
[tools]
enable_fs = false
enable_shell = false
enable_skill = false
"#,
        )
        .expect("parse app config");

        assert!(!cfg.tools.enable_fs);
        assert!(!cfg.tools.enable_shell);
        assert!(!cfg.tools.enable_skill);
    }

    /// AppConfig defaults compaction to pi's reserve and retained-tail budgets.
    #[test]
    fn app_config_default_compaction_matches_pi() {
        let cfg = AppConfig::default();

        assert!(cfg.compaction.enabled);
        assert_eq!(cfg.compaction.reserve_tokens, 16_384);
        assert_eq!(cfg.compaction.keep_recent_tokens, 20_000);
    }

    /// AppConfig reads pi-style compaction settings from TOML.
    #[test]
    fn app_config_reads_compaction_settings() {
        let cfg: AppConfig = toml::from_str(
            r#"
[compaction]
enabled = false
reserve_tokens = 10000
keep_recent_tokens = 25000
"#,
        )
        .expect("parse app config");

        assert!(!cfg.compaction.enabled);
        assert_eq!(cfg.compaction.reserve_tokens, 10_000);
        assert_eq!(cfg.compaction.keep_recent_tokens, 25_000);
    }

    /// AppConfig extracts the provider id from the active model setting.
    #[test]
    fn app_config_active_provider_id_uses_active_model_prefix() {
        let cfg = AppConfig {
            active_model: "openai/gpt-5".to_string(),
            ..AppConfig::default()
        };

        assert_eq!(cfg.active_provider_id(), "openai");
    }

    /// AppConfig returns an empty provider id for malformed active model values.
    #[test]
    fn app_config_active_provider_id_is_empty_without_separator() {
        let cfg = AppConfig {
            active_model: "gpt-5".to_string(),
            ..AppConfig::default()
        };

        assert_eq!(cfg.active_provider_id(), "");
    }
}

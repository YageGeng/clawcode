//! Top-level application configuration types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::app::AppSectionConfig;
use crate::approval::{ApprovalMode, AskForApproval};
use crate::extensions::ExtensionsConfig;
use crate::kernel::KernelConfig;
use crate::llm::{LlmModel, LlmProvider};
use crate::logging::LoggingConfig;
use crate::mcp::{McpConfigError, McpServerConfig};
use crate::prompt::PromptPolicy;
use crate::retry::RetryConfig;
use crate::skills::SkillsConfig;
use crate::subagent::SubagentConfig;
use crate::tools::ToolsConfig;
use protocol::{CompactionPolicy, ModelInputModalitiesError, ModelProfile};

/// Context compaction settings: global budgets plus pi-style per-model overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CompactionConfig {
    /// Global `enabled`, `reserve_tokens`, and `keep_recent_tokens` values.
    #[serde(flatten)]
    pub policy: CompactionPolicy,
    /// Budgets for one model keyed by its exact `provider/model` identifier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_overrides: BTreeMap<String, CompactionModelOverride>,
}

/// Budgets that replace the global compaction values for one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CompactionModelOverride {
    /// Tokens kept free for the summary request and the next response.
    #[serde(default)]
    pub reserve_tokens: Option<u64>,
    /// Recent tokens retained verbatim after summarization.
    #[serde(default)]
    pub keep_recent_tokens: Option<u64>,
}

impl CompactionConfig {
    /// Returns the policy for one model: its override merged over the global
    /// budgets. `enabled` always comes from the global value.
    #[must_use]
    pub fn resolve(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> CompactionPolicy {
        let mut policy = self.policy;
        if let Some(model_override) = self
            .model_overrides
            .get(&format!("{provider_id}/{model_id}"))
        {
            if let Some(reserve_tokens) = model_override.reserve_tokens {
                policy.reserve_tokens = reserve_tokens;
            }
            if let Some(keep_recent_tokens) = model_override.keep_recent_tokens
            {
                policy.keep_recent_tokens = keep_recent_tokens;
            }
        }
        policy
    }

    /// Returns the policy for the model a profile describes.
    #[must_use]
    pub fn for_profile(&self, profile: &ModelProfile) -> CompactionPolicy {
        self.resolve(&profile.provider_id, &profile.model_id)
    }
}

impl From<CompactionPolicy> for CompactionConfig {
    /// Uses one policy for every model.
    fn from(policy: CompactionPolicy) -> Self {
        Self {
            policy,
            model_overrides: BTreeMap::new(),
        }
    }
}

/// Branch-summary settings matching pi's `branchSummary` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct BranchSummaryConfig {
    /// Tokens kept free for the summary prompt and response when selecting
    /// abandoned-branch messages.
    #[serde(default = "BranchSummaryConfig::default_reserve_tokens")]
    pub reserve_tokens: u64,
}

impl BranchSummaryConfig {
    /// Returns pi's default branch-summary reserve.
    const fn default_reserve_tokens() -> u64 {
        16_384
    }
}

impl Default for BranchSummaryConfig {
    /// Uses pi's 16,384-token reserve.
    fn default() -> Self {
        Self {
            reserve_tokens: Self::default_reserve_tokens(),
        }
    }
}

/// Splits a `provider/model` identifier whose two parts are both non-empty.
fn split_model_key(value: &str) -> Option<(&str, &str)> {
    value.split_once('/').filter(|(provider_id, model_id)| {
        !provider_id.is_empty() && !model_id.is_empty()
    })
}

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
    /// MCP Server entries failed individual or collection validation.
    #[error(transparent)]
    Mcp(#[from] McpConfigError),

    /// The active model did not contain a provider and model identifier.
    #[error("active model '{value}' must use provider/model format")]
    MalformedActiveModel {
        /// Invalid configured active model value.
        value: String,
    },
    /// A compaction override key did not contain a provider and model identifier.
    #[error("compaction override '{key}' must use provider/model format")]
    MalformedCompactionOverride {
        /// Invalid configured override key.
        key: String,
    },
    /// A compaction override named a model absent from the model catalog.
    #[error("compaction override '{key}' names no configured model")]
    UnknownCompactionOverride {
        /// Override key without a matching model.
        key: String,
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
    /// A provider `id` or `preset` is empty, padded, or contains `/`.
    #[error(
        "provider '{provider_id}' {field} {value:?} must be a non-empty name without surrounding whitespace or '/'"
    )]
    InvalidProviderName {
        /// Provider identifier as configured.
        provider_id: String,
        /// The offending field, `id` or `preset`.
        field: &'static str,
        /// The value as configured.
        value: String,
    },
    /// A model's request budget exceeds the room compaction keeps free for a response.
    #[error(
        "model '{provider_id}/{model_id}' request_max_tokens={request_max_tokens} must not exceed compaction reserve_tokens={reserve_tokens}"
    )]
    RequestBudgetExceedsReserve {
        /// Provider identifier containing the model.
        provider_id: String,
        /// Model whose budget is too large.
        model_id: String,
        /// Configured per-request output budget.
        request_max_tokens: u64,
        /// Configured compaction reserve.
        reserve_tokens: u64,
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
    /// A model declared an invalid combination of input modalities.
    #[error(
        "model '{provider_id}/{model_id}' has invalid input modalities: {reason}"
    )]
    InvalidModelInput {
        /// Provider identifier containing the model.
        provider_id: String,
        /// Model containing the invalid input list.
        model_id: String,
        /// Typed modality validation failure.
        reason: ModelInputModalitiesError,
    },
    /// A configured upstream model identifier contained no visible content.
    #[error("model '{provider_id}/{model_id}' upstream_id must not be empty")]
    InvalidUpstreamModelId {
        /// Provider identifier containing the model.
        provider_id: String,
        /// Local model identifier containing the invalid override.
        model_id: String,
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
    /// Authentication paths and protocol selections must be explicit before startup.
    #[error("provider '{provider_id}' has invalid authentication: {reason}")]
    InvalidAuthentication {
        /// Configured provider whose authentication is invalid.
        provider_id: String,
        /// Configuration-only diagnostic, never credential contents.
        reason: String,
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
    /// Application-level runtime configuration.
    #[serde(default)]
    pub app: AppSectionConfig,
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
    /// Kernel runtime configuration.
    #[serde(default)]
    pub kernel: KernelConfig,
    /// Subagent delegation configuration.
    #[serde(default)]
    pub subagent: SubagentConfig,
    /// Manual context compaction configuration.
    #[serde(default)]
    pub compaction: CompactionConfig,
    /// Abandoned-branch summary configuration.
    #[serde(default)]
    pub branch_summary: BranchSummaryConfig,
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
            app: AppSectionConfig::default(),
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
            kernel: KernelConfig::default(),
            subagent: SubagentConfig::default(),
            compaction: CompactionConfig::default(),
            branch_summary: BranchSummaryConfig::default(),
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
        let (provider_id, model_id) = split_model_key(&self.active_model)
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

    /// Return the enhanced approval policy after applying legacy compatibility.
    #[must_use]
    pub fn effective_approval_policy(&self) -> AskForApproval {
        self.approval_policy
            .unwrap_or_else(|| AskForApproval::from(self.approval))
    }

    /// Validate cross-field invariants that serde cannot express directly.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        // Catalog models may come from extensions, so key existence is checked
        // when the Kernel resolves its catalog; only the format is checked here.
        if let Some(key) = self
            .compaction
            .model_overrides
            .keys()
            .find(|key| split_model_key(key).is_none())
        {
            return Err(ConfigValidationError::MalformedCompactionOverride {
                key: key.clone(),
            });
        }
        for provider in &self.providers {
            provider.validate_names()?;
            provider.validate_models()?;
            provider.validate_auth_source()?;
            // Compaction keeps input below the window minus the reserve, so a budget
            // within the reserve keeps input plus output inside the window.
            for model in &provider.models {
                let reserve_tokens = self
                    .compaction
                    .resolve(provider.id.as_str(), &model.id)
                    .reserve_tokens;
                if let Some(budget) = model
                    .request_max_tokens
                    .filter(|budget| *budget > reserve_tokens)
                {
                    return Err(
                        ConfigValidationError::RequestBudgetExceedsReserve {
                            provider_id: provider.id.as_str().to_owned(),
                            model_id: model.id.clone(),
                            request_max_tokens: budget,
                            reserve_tokens,
                        },
                    );
                }
            }
        }

        // Context limits may come from the rig catalog, so the compaction reserve is
        // checked when the provider layer resolves the active model.
        self.resolve_active_model()?;
        if !self.retry.has_valid_backoff() {
            return Err(ConfigValidationError::InvalidRetryDuration {
                base_delay_ms: self.retry.agent.base_delay_ms,
                max_retries: self.retry.agent.max_retries,
            });
        }

        let mut server_ids = std::collections::HashSet::new();
        for server in &self.mcp_servers {
            server.validate()?;
            if !server_ids.insert(server.name.as_str()) {
                return Err(McpConfigError::DuplicateName {
                    server: server.name.clone(),
                }
                .into());
            }
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

    /// Three children may run at once by default; zero is rejected at load.
    #[test]
    fn subagent_max_agents_defaults_to_three_and_rejects_zero() {
        assert_eq!(AppConfig::default().subagent.max_agents.get(), 3);

        let cfg: AppConfig = toml::from_str(
            r#"
[subagent]
max_agents = 5
"#,
        )
        .expect("parse subagent config");
        assert_eq!(cfg.subagent.max_agents.get(), 5);

        toml::from_str::<AppConfig>("[subagent]\nmax_agents = 0\n")
            .expect_err("zero max_agents is rejected");
    }

    /// Providers send no prompt-cache option unless one names a retention.
    #[test]
    fn provider_prompt_cache_defaults_to_none_and_reads_retention() {
        let cfg: AppConfig = toml::from_str(
            r#"
[[providers]]
id = "plain"
display_name = "Plain"

[[providers]]
id = "anthropic"
display_name = "Anthropic"
prompt_cache = "long"
"#,
        )
        .expect("parse provider prompt cache");
        assert_eq!(cfg.providers[0].prompt_cache, None);
        assert_eq!(
            cfg.providers[1].prompt_cache,
            Some(crate::PromptCacheRetention::Long)
        );
        // The former global "unset" value is not a retention.
        toml::from_str::<AppConfig>(
            "[[providers]]\nid = \"x\"\ndisplay_name = \"X\"\nprompt_cache = \"unset\"\n",
        )
        .expect_err("unset is not a prompt-cache retention");
    }

    /// App runtime recovery batching defaults to the current transport limit.
    #[test]
    fn app_config_default_recovery_batch_size_is_128() {
        let cfg = AppConfig::default();

        assert_eq!(cfg.app.recovery.max_batch_size.get(), 128);
    }

    /// App runtime recovery batching reads the nested configured limit.
    #[test]
    fn app_config_reads_recovery_batch_size() {
        let cfg: AppConfig = toml::from_str(
            r#"
[app.recovery]
max_batch_size = 32
"#,
        )
        .expect("parse app recovery config");

        assert_eq!(cfg.app.recovery.max_batch_size.get(), 32);
    }

    /// App runtime recovery batching rejects a zero-sized transport batch.
    #[test]
    fn app_config_rejects_zero_recovery_batch_size() {
        let error = toml::from_str::<AppConfig>(
            r#"
[app.recovery]
max_batch_size = 0
"#,
        )
        .expect_err("reject zero app recovery batch size");

        assert!(error.to_string().contains("nonzero"));
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

        assert!(cfg.compaction.policy.enabled);
        assert_eq!(cfg.compaction.policy.reserve_tokens, 16_384);
        assert_eq!(cfg.compaction.policy.keep_recent_tokens, 20_000);
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

        assert!(!cfg.compaction.policy.enabled);
        assert_eq!(cfg.compaction.policy.reserve_tokens, 10_000);
        assert_eq!(cfg.compaction.policy.keep_recent_tokens, 25_000);
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

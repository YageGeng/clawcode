//! Top-level application configuration types.

use serde::{Deserialize, Serialize};

pub use protocol::ApprovalMode;

use crate::agent::MultiAgentConfig;
use crate::llm::LlmProvider;
use crate::mcp::McpServerConfig;
use crate::skills::SkillsConfig;

/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Configured LLM providers.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
    /// Active model in `provider_id/model_id` format (e.g. "deepseek/deepseek-v4-flash").
    #[serde(default = "default_active_model")]
    pub active_model: String,
    /// Tool-approval behaviour.
    #[serde(default)]
    pub approval: ApprovalMode,
    /// Multi-agent subsystem configuration.
    #[serde(default)]
    pub multi_agent: MultiAgentConfig,
    /// Skill subsystem configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// MCP server configurations.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

fn default_active_model() -> String {
    "deepseek/deepseek-v4-flash".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            active_model: default_active_model(),
            approval: ApprovalMode::default(),
            multi_agent: MultiAgentConfig::default(),
            skills: SkillsConfig::default(),
            mcp_servers: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Validate cross-field invariants that serde cannot express directly.
    pub fn validate(&self) -> Result<(), crate::mcp::McpConfigError> {
        for server in &self.mcp_servers {
            server.validate()?;
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
}

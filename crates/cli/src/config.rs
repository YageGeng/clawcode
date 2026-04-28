use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use arc_swap::ArcSwap;
use figment::{
    Figment, Profile,
    providers::{Env, Format, Toml},
};
use kernel::tools::ToolApprovalProfile;
use llm::providers::LlmConfig;
use serde::Deserialize;

/// Application-wide CLI configuration loaded from TOML files plus `APP_` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Factory model reference in `{provider}/{model}` format used for CLI requests.
    pub current_model: String,
    /// Complete LLM provider factory configuration used to build model clients.
    pub llm: LlmConfig,
    /// Skill discovery configuration forwarded into the kernel runtime.
    pub skills: CliSkillsConfig,
    /// Tool approval behavior used when the CLI creates ACP approval handlers.
    #[serde(default)]
    pub approval: CliApprovalConfig,
}

impl AppConfig {
    /// Returns the configured factory model reference selected for CLI requests.
    pub fn current_model_ref(&self) -> &str {
        &self.current_model
    }
}

/// CLI-owned approval configuration loaded from TOML and `APP_APPROVAL__*` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CliApprovalConfig {
    /// Runtime approval profile applied to tool dispatch.
    #[serde(default)]
    pub profile: CliApprovalProfile,
}

impl Default for CliApprovalConfig {
    /// Builds the default approval behavior for interactive CLI sessions.
    fn default() -> Self {
        Self {
            profile: CliApprovalProfile::default(),
        }
    }
}

impl CliApprovalConfig {
    /// Converts CLI-loaded approval settings into the runtime approval profile.
    pub fn to_tool_approval_profile(&self) -> ToolApprovalProfile {
        self.profile.into()
    }
}

/// Serialized approval profile values accepted by CLI configuration.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliApprovalProfile {
    /// Allows every tool call without asking the client for permission.
    TrustAll,
    /// Asks the client for permission before every tool call.
    AskAlways,
    /// Uses each tool's metadata to decide whether permission is required.
    Default,
}

impl Default for CliApprovalProfile {
    /// Builds the default CLI approval profile.
    fn default() -> Self {
        Self::Default
    }
}

impl From<CliApprovalProfile> for ToolApprovalProfile {
    /// Converts CLI config profile names into the tools crate policy model.
    fn from(value: CliApprovalProfile) -> Self {
        match value {
            CliApprovalProfile::TrustAll => Self::TrustAll,
            CliApprovalProfile::AskAlways => Self::AskAlways,
            CliApprovalProfile::Default => Self::Default,
        }
    }
}

/// CLI-owned skill configuration shape loaded from TOML and `APP_SKILLS__*` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CliSkillsConfig {
    /// Explicit directories scanned recursively for `SKILL.md` files.
    pub roots: Vec<PathBuf>,
    /// Optional current working directory used to discover `.agents/skills`.
    pub cwd: Option<PathBuf>,
    /// Enables or disables skill discovery for CLI requests.
    pub enabled: bool,
}

impl CliSkillsConfig {
    /// Converts CLI-loaded skill settings into the kernel skill discovery configuration.
    pub fn to_skill_config(&self) -> skills::SkillConfig {
        skills::SkillConfig {
            roots: self.roots.clone(),
            cwd: self.cwd.clone(),
            enabled: self.enabled,
        }
    }
}

/// Shared process-wide CLI config cached after the first successful load.
static CONFIG: LazyLock<ArcSwap<AppConfig>> =
    LazyLock::new(|| ArcSwap::from_pointee(load_config().expect("Failed to load app config")));

/// Returns the shared CLI config loaded from TOML files and `APP_` environment variables.
pub fn app_config() -> Arc<AppConfig> {
    CONFIG.load_full()
}

/// Loads one fresh CLI config snapshot from disk and environment variables.
pub fn load_config() -> Result<AppConfig, Box<figment::Error>> {
    let mut figment = Figment::new().merge(Toml::file("base.toml"));

    if let Some(profile) = Profile::from_env("APP_PROFILE") {
        figment = figment.merge(Toml::file(format!("base.{}.toml", profile)));
    }

    figment
        .merge(Env::prefixed("APP_").split("__"))
        .extract::<AppConfig>()
        .map_err(Box::new)
}

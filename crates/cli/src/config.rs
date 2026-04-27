use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use arc_swap::ArcSwap;
use figment::{
    Figment, Profile,
    providers::{Env, Format, Toml},
};
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
}

impl AppConfig {
    /// Returns the configured factory model reference selected for CLI requests.
    pub fn current_model_ref(&self) -> &str {
        &self.current_model
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

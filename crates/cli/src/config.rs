use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use arc_swap::ArcSwap;
use figment::{
    Figment, Profile,
    providers::{Env, Format, Toml},
};
use llm::providers::openai;
use serde::Deserialize;

/// Application-wide CLI configuration loaded from TOML files plus `APP_` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    /// Authentication mode used to choose the provider implementation.
    pub auth_mode: AuthMode,
    /// Transport mode used to choose the provider protocol.
    pub link_mode: LinkMode,
    /// OpenAI provider configuration for API-key based requests.
    pub openai: OpenAiConfig,
    /// ChatGPT/Codex provider configuration for OAuth-backed requests.
    pub chatgpt: ChatGptConfig,
    /// Skill discovery configuration forwarded into the kernel runtime.
    pub skills: CliSkillsConfig,
}

impl Default for AppConfig {
    /// Builds the default CLI configuration before TOML/env overrides are merged in.
    fn default() -> Self {
        Self {
            auth_mode: AuthMode::ApiKey,
            link_mode: LinkMode::Response,
            openai: OpenAiConfig::default(),
            chatgpt: ChatGptConfig::default(),
            skills: CliSkillsConfig::default(),
        }
    }
}

/// Supported authentication modes for the CLI config.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Standard OpenAI API-key authentication.
    ApiKey,
    /// ChatGPT/Codex provider authentication backed by llm provider OAuth handling.
    OAuth,
}

/// Supported transport modes for the CLI config.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkMode {
    /// Use the Responses-style provider.
    Response,
    /// Use the traditional OpenAI Chat Completions API.
    Completion,
}

/// OpenAI provider configuration for API-key based requests.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OpenAiConfig {
    /// Base URL used for OpenAI API-key requests.
    pub base_url: String,
    /// Default model used for OpenAI API-key requests.
    pub model: String,
    /// Optional API key sourced from config files or `APP_OPENAI__API_KEY`.
    pub api_key: Option<String>,
}

impl Default for OpenAiConfig {
    /// Builds the default OpenAI provider configuration used by the CLI.
    fn default() -> Self {
        Self {
            base_url: openai::OPENAI_API_BASE_URL.to_string(),
            model: "gpt-5.4".to_string(),
            api_key: None,
        }
    }
}

/// ChatGPT/Codex provider configuration for OAuth-backed requests.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ChatGptConfig {
    /// Base URL used for ChatGPT/Codex responses requests.
    pub base_url: String,
    /// Default model used for ChatGPT/Codex requests.
    pub model: String,
    /// Optional bearer token sourced from config files or `APP_CHATGPT__ACCESS_TOKEN`.
    pub access_token: Option<String>,
}

/// CLI-owned skill configuration shape loaded from TOML and `APP_SKILLS__*` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CliSkillsConfig {
    /// Explicit directories scanned recursively for `SKILL.md` files.
    pub roots: Vec<PathBuf>,
    /// Optional current working directory used to discover `.agents/skills`.
    pub cwd: Option<PathBuf>,
    /// Enables or disables skill discovery for CLI requests.
    pub enabled: bool,
}

impl Default for CliSkillsConfig {
    /// Builds the default CLI skill configuration before config-file overrides are merged.
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            cwd: None,
            enabled: true,
        }
    }
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

impl Default for ChatGptConfig {
    /// Builds the default ChatGPT provider configuration used by the CLI.
    fn default() -> Self {
        Self {
            base_url: openai::codex::OPENAI_CODEX_API_BASE_URL.to_string(),
            model: openai::codex::OPENAI_CODEX_DEFAULT_MODEL.to_string(),
            access_token: None,
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

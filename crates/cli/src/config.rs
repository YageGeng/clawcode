use std::{
    path::{Path, PathBuf},
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
    /// Runtime policy knobs forwarded into the ACP/kernel loop.
    #[serde(default)]
    pub runtime: CliRuntimeConfig,
    /// Tool approval behavior used when the CLI creates ACP approval handlers.
    #[serde(default)]
    pub approval: CliApprovalConfig,
    /// Session persistence configuration.
    #[serde(default)]
    pub persistence: CliPersistenceConfig,
}

impl AppConfig {
    /// Returns the configured factory model reference selected for CLI requests.
    pub fn current_model_ref(&self) -> &str {
        &self.current_model
    }
}

/// CLI-owned runtime configuration loaded from TOML and `APP_RUNTIME__*` overrides.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct CliRuntimeConfig {
    /// Caps the deepest child-agent generation this CLI allows. `None` means unlimited.
    #[serde(default)]
    pub max_subagent_depth: Option<usize>,
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

/// CLI-owned session persistence configuration.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CliPersistenceConfig {
    /// Whether session persistence is enabled. Defaults to `true`.
    #[serde(default = "default_persistence_enabled")]
    pub enabled: bool,
}

fn default_persistence_enabled() -> bool {
    true
}

impl Default for CliPersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: default_persistence_enabled(),
        }
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
    load_config_from(Path::new("."))
}

/// Loads one fresh CLI config snapshot from a specific base directory.
fn load_config_from(base_dir: &Path) -> Result<AppConfig, Box<figment::Error>> {
    let mut figment = Figment::new().merge(Toml::file(base_dir.join("base.toml")));

    if let Some(profile) = Profile::from_env("APP_PROFILE") {
        figment = figment.merge(Toml::file(base_dir.join(format!("base.{}.toml", profile))));
    }

    Ok(figment
        .merge(Env::prefixed("APP_").split("__"))
        .extract::<AppConfig>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use tempfile::tempdir;

    /// Serializes config tests that temporarily mutate process environment variables.
    static CONFIG_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    /// Verifies the CLI runtime config can be loaded from TOML.
    #[test]
    fn load_config_reads_runtime_max_subagent_depth_from_toml() {
        let _guard = CONFIG_TEST_LOCK
            .lock()
            .expect("config test lock should work");
        let temp = tempdir().expect("tempdir should be created");
        std::fs::write(
            temp.path().join("base.toml"),
            r#"
current_model = "openai/gpt-5.4"

[llm]
providers = []

[skills]
roots = []
enabled = true
cwd = "."

[runtime]
max_subagent_depth = 2

[approval]
profile = "default"

[persistence]
enabled = true
"#,
        )
        .expect("base.toml should be written");

        let config = load_config_from(temp.path()).expect("config should load");
        assert_eq!(config.runtime.max_subagent_depth, Some(2));
    }

    /// Verifies `APP_RUNTIME__MAX_SUBAGENT_DEPTH` overrides the TOML runtime setting.
    #[test]
    fn load_config_reads_runtime_max_subagent_depth_from_env() {
        let _guard = CONFIG_TEST_LOCK
            .lock()
            .expect("config test lock should work");
        let temp = tempdir().expect("tempdir should be created");
        std::fs::write(
            temp.path().join("base.toml"),
            r#"
current_model = "openai/gpt-5.4"

[llm]
providers = []

[skills]
roots = []
enabled = true
cwd = "."

[runtime]
max_subagent_depth = 2

[approval]
profile = "default"

[persistence]
enabled = true
"#,
        )
        .expect("base.toml should be written");

        let previous = std::env::var("APP_RUNTIME__MAX_SUBAGENT_DEPTH").ok();
        // SAFETY: this test is the only CLI test mutating this process env var and restores it
        // before returning, so the temporary override stays scoped to this assertion.
        unsafe {
            std::env::set_var("APP_RUNTIME__MAX_SUBAGENT_DEPTH", "1");
        }
        let config = load_config_from(temp.path()).expect("config should load");
        if let Some(previous) = previous {
            // SAFETY: restore the original process env var value for the same scoped test.
            unsafe {
                std::env::set_var("APP_RUNTIME__MAX_SUBAGENT_DEPTH", previous);
            }
        } else {
            // SAFETY: remove the temporary override introduced by this scoped test.
            unsafe {
                std::env::remove_var("APP_RUNTIME__MAX_SUBAGENT_DEPTH");
            }
        }

        assert_eq!(config.runtime.max_subagent_depth, Some(1));
    }
}

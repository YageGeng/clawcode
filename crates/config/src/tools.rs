//! Built-in tool registration configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Configuration for model-facing built-in tool groups.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolsConfig {
    /// Whether filesystem tools should be registered.
    pub enable_fs: bool,
    /// Whether shell tools (`exec_command`, `write_stdin`) should be registered.
    pub enable_shell: bool,
    /// Whether the skill loading tool should be registered.
    pub enable_skill: bool,
    /// Process management settings for the shell tools.
    pub exec: ExecConfig,
}

impl Default for ToolsConfig {
    /// Return the default tool registration policy.
    fn default() -> Self {
        Self {
            enable_fs: true,
            enable_shell: true,
            enable_skill: true,
            exec: ExecConfig::default(),
        }
    }
}

/// Settings for `exec_command` sessions, mirroring codex unified exec.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
#[serde(default)]
pub struct ExecConfig {
    /// Upper bound for an empty `write_stdin` poll, in milliseconds.
    #[builder(default = 300_000)]
    pub background_terminal_max_timeout_ms: u64,
    /// Whether login-shell state is captured once and replayed per command.
    #[builder(default = true)]
    pub shell_snapshot: bool,
    /// Whether the model is told which command sessions are still open.
    #[builder(default = true)]
    pub command_sessions: bool,
    /// How the child environment is derived from this process.
    #[builder(default)]
    pub env: ExecEnvPolicy,
}

impl Default for ExecConfig {
    /// Uses the codex defaults for every exec setting.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Environment filtering applied before the forced exec overrides.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
#[serde(default)]
pub struct ExecEnvPolicy {
    /// Which inherited variables survive the first step.
    #[builder(default)]
    pub inherit: EnvInherit,
    /// Skips removing names matching `*KEY*`, `*SECRET*`, and `*TOKEN*`.
    ///
    /// Defaults to `true`, as in codex, so tokens used by tools such as `gh`
    /// keep working unless the user opts into the filter.
    #[builder(default = true)]
    pub ignore_default_excludes: bool,
    /// Case-insensitive `*` patterns removed from the environment.
    #[builder(default)]
    pub exclude: Vec<String>,
    /// When non-empty, only names matching one of these patterns are kept.
    #[builder(default)]
    pub include_only: Vec<String>,
    /// Variables inserted or replaced after exclusion.
    #[builder(default)]
    pub set: BTreeMap<String, String>,
}

impl Default for ExecEnvPolicy {
    /// Inherits everything and filters nothing, matching codex.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Starting set of inherited environment variables.
#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq,
)]
#[serde(rename_all = "lowercase")]
pub enum EnvInherit {
    /// Keep every inherited variable.
    #[default]
    All,
    /// Keep only the platform's core variables such as `PATH` and `HOME`.
    Core,
    /// Start from an empty environment.
    None,
}

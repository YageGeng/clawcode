//! Built-in tool registration configuration.

use serde::{Deserialize, Serialize};

/// Configuration for model-facing built-in tool groups.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolsConfig {
    /// Whether filesystem tools should be registered.
    pub enable_fs: bool,
    /// Whether shell tools should be registered.
    pub enable_shell: bool,
    /// Whether the skill loading tool should be registered.
    pub enable_skill: bool,
}

impl Default for ToolsConfig {
    /// Return the default tool registration policy.
    fn default() -> Self {
        Self {
            enable_fs: true,
            enable_shell: true,
            enable_skill: true,
        }
    }
}

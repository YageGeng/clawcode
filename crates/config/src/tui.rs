//! TUI configuration loaded from the application TOML.

use serde::{Deserialize, Serialize};

/// Color theme selected for the local terminal UI.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TuiTheme {
    /// Terminal-adaptive dark theme.
    #[default]
    Dark,
    /// Terminal-adaptive light theme.
    Light,
}

/// Top-level TUI configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct TuiConfig {
    /// Color theme used by the local TUI.
    #[serde(default)]
    pub theme: TuiTheme,
}

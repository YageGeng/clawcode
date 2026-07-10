//! Multi-agent configuration: thread limits, depth, wait timeouts.

use serde::{Deserialize, Serialize};

/// Configuration for the multi-agent subsystem.
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
pub struct MultiAgentConfig {
    /// Whether model-facing sub-agent tools should be registered.
    pub enable: bool,
    /// Maximum number of concurrent sub-agent threads per session tree.
    pub max_threads: usize,
    /// Maximum spawn depth (root = 0, its child = 1, etc.).
    pub max_spawn_depth: i32,
    /// Minimum time in milliseconds that wait_agent should block before
    /// returning with a timeout.
    pub min_wait_timeout_ms: u64,
    /// When true, spawn_agent tool returns only `task_name` instead of
    /// `{ task_name, nickname }`.
    pub hide_spawn_metadata: bool,
}

impl Default for MultiAgentConfig {
    /// Return the default multi-agent configuration.
    fn default() -> Self {
        Self::builder()
            .enable(true)
            .max_threads(8)
            .max_spawn_depth(8)
            .min_wait_timeout_ms(1000)
            .hide_spawn_metadata(false)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MultiAgentConfig enables sub-agent support by default.
    #[test]
    fn multi_agent_config_defaults_to_enabled() {
        let cfg = MultiAgentConfig::default();

        assert!(cfg.enable);
    }

    /// MultiAgentConfig reads the sub-agent support switch from TOML.
    #[test]
    fn multi_agent_config_reads_enable_from_toml() {
        let cfg: MultiAgentConfig = toml::from_str(
            r#"
enable = false
"#,
        )
        .expect("parse multi-agent config");

        assert!(!cfg.enable);
    }
}

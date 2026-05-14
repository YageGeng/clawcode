//! Multi-agent configuration: thread limits, depth, wait timeouts.

use serde::{Deserialize, Serialize};

/// Configuration for the multi-agent subsystem.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MultiAgentConfig {
    /// Maximum number of concurrent sub-agent threads per session tree.
    pub max_concurrent_threads_per_session: usize,
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
    fn default() -> Self {
        Self {
            max_concurrent_threads_per_session: 8,
            max_spawn_depth: 8,
            min_wait_timeout_ms: 1000,
            hide_spawn_metadata: false,
        }
    }
}

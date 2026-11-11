//! Subagent delegation configuration.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

const DEFAULT_MAX_AGENTS: NonZeroUsize =
    NonZeroUsize::new(3).expect("default subagent limit must be positive");

/// Delegation settings grouped under the top-level `subagent` section.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SubagentConfig {
    /// Children of one owner allowed to run at the same time.
    pub max_agents: NonZeroUsize,
}

impl Default for SubagentConfig {
    /// Allows three concurrently running children.
    fn default() -> Self {
        Self {
            max_agents: DEFAULT_MAX_AGENTS,
        }
    }
}

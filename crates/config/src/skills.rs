//! Skill subsystem configuration types.

pub use protocol::SkillSelectionRule;
use serde::{Deserialize, Serialize};

/// Top-level skill configuration stored in [`crate::AppConfig`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillsConfig {
    /// Whether to inject the skill catalog block into the system prompt.
    #[serde(default = "default_true")]
    pub include_instructions: bool,

    /// Per-skill enable/disable rules.  Rules later in the list override
    /// earlier ones when their selectors match the same skill.
    #[serde(default)]
    pub rules: Vec<SkillSelectionRule>,
}

fn default_true() -> bool {
    true
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            include_instructions: true,
            rules: Vec::new(),
        }
    }
}

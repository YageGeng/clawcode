//! Skill subsystem configuration types.

use std::path::PathBuf;

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
    pub rules: Vec<SkillConfigRule>,
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

/// A single enable/disable rule targeting a skill by name or path.
///
/// `path` and `name` are mutually exclusive — entries with both or
/// neither are warned about and skipped during resolution.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillConfigRule {
    /// Match by absolute path to the `SKILL.md` file.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Match by skill name.
    #[serde(default)]
    pub name: Option<String>,
    /// `true` = enable, `false` = disable.
    pub enabled: bool,
}

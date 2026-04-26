use std::path::PathBuf;

/// Runtime configuration controlling where skill discovery looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillConfig {
    /// Explicit roots that are scanned directly for `SKILL.md` files.
    pub roots: Vec<PathBuf>,
    /// Optional current working directory used to discover `.agents/skills`.
    pub cwd: Option<PathBuf>,
    /// Enables or disables all skill loading for the current runtime.
    pub enabled: bool,
}

impl Default for SkillConfig {
    /// Builds the default config with skills enabled but no configured roots.
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            cwd: None,
            enabled: true,
        }
    }
}

/// Metadata parsed from a single `SKILL.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    /// Stable skill name used for explicit `$skill-name` mentions.
    pub name: String,
    /// Short description rendered in the available-skills prompt section.
    pub description: String,
    /// Absolute or caller-provided path to the source `SKILL.md` file.
    pub path: PathBuf,
}

/// Non-fatal load error for one candidate skill file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLoadError {
    /// Candidate path that failed to parse or read.
    pub path: PathBuf,
    /// Human-readable reason that can be surfaced in logs or diagnostics.
    pub message: String,
}

/// Result of scanning all configured skill roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillLoadOutcome {
    /// Successfully parsed skills.
    pub skills: Vec<SkillMetadata>,
    /// Per-file errors that did not stop discovery of other skills.
    pub errors: Vec<SkillLoadError>,
}

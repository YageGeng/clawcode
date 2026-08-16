use std::path::PathBuf;

/// Skill discovery and invocation failures.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// Filesystem access failed.
    #[error("Skill I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// SKILL.md frontmatter was missing or invalid.
    #[error("invalid Skill '{}': {reason}", path.display())]
    InvalidFrontmatter {
        /// Skill file that failed validation.
        path: PathBuf,
        /// Validation diagnostic.
        reason: String,
    },
    /// Explicit invocation referenced an unknown Skill name.
    #[error("Skill not found: {0}")]
    NotFound(String),
}

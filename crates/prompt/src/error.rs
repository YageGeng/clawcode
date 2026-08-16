use std::path::PathBuf;

/// Failure encountered while loading or rendering Prompt resources.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// A required configured resource path could not be loaded.
    #[error("configured Prompt resource '{}' could not be loaded: {reason}", path.display())]
    ConfiguredResource {
        /// Required path that failed validation or loading.
        path: PathBuf,
        /// Filesystem failure without resource body content.
        reason: String,
    },
    /// Markdown frontmatter could not be decoded as the requested metadata.
    #[error("invalid Prompt frontmatter: {0}")]
    Frontmatter(String),
}

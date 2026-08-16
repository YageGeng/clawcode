//! Prompt Template discovery, Markdown parsing, and command expansion.

mod catalog;
mod document;
mod expansion;

use std::path::PathBuf;

use protocol::PromptSourceScope;

pub use catalog::PromptTemplateCatalog;
pub use document::MarkdownDocument;

/// Error policy applied when one Prompt Template root cannot be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplateRootRequirement {
    /// Automatic discovery records a warning and continues.
    Discovered,
    /// Explicit configuration returns a fatal loading error.
    Required,
}

/// One ordered file or directory searched for Prompt Templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateRoot {
    /// File or directory path to inspect.
    pub path: PathBuf,
    /// Source scope attached to every template loaded from the root.
    pub scope: PromptSourceScope,
    /// Failure policy for an unavailable root.
    pub requirement: PromptTemplateRootRequirement,
}

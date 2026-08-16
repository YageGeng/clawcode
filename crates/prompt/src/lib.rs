//! Prompt resource discovery, parsing, expansion, and rendering.

mod error;
mod factory;
mod instruction;
mod session;
mod source;
mod system;
mod template;

pub use error::PromptError;
pub use factory::{FilesystemPromptFactory, PromptFactory};
pub use session::PromptSession;
pub use system::{BuiltSystemPrompt, SystemPromptTurnInput};
pub use template::{
    MarkdownDocument, PromptTemplateCatalog, PromptTemplateRoot,
    PromptTemplateRootRequirement,
};

//! Built-in file-system tools and registration.

pub mod edit;
pub mod patch;
pub mod read;
pub mod write;

use std::sync::Arc;

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register built-in file-system tools.
    pub fn register_fs_tools(&self, is_anthropic: bool) {
        self.register(Arc::new(read::ReadFile::new()));
        self.register(Arc::new(write::WriteFile::new()));

        if is_anthropic {
            self.register(Arc::new(edit::EditFile::new()));
        } else {
            self.register(Arc::new(patch::ApplyPatch::new()));
        }
    }
}

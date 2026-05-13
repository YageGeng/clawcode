//! Built-in file-system tools and registration.

pub mod edit;
pub mod patch;
pub mod read;
pub mod write;

use std::sync::Arc;

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register built-in file-system tools.
    pub fn register_fs_tools(&self) {
        self.register(Arc::new(read::ReadFile::new()));
        self.register(Arc::new(write::WriteFile::new()));
        self.register(Arc::new(edit::EditFile::new()));
        self.register(Arc::new(patch::ApplyPatch::new()));
    }
}

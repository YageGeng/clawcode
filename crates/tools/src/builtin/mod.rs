//! Built-in tool implementations and registration.

pub mod file;
pub mod shell;

use std::sync::Arc;

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register all built-in tools.
    pub fn register_builtins(&mut self) {
        self.register(Arc::new(shell::ShellCommand::new()));
        self.register(Arc::new(file::ReadFile::new()));
        self.register(Arc::new(file::WriteFile::new()));
        self.register(Arc::new(file::ApplyPatch::new()));
    }
}

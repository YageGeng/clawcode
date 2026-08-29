mod edit;
mod exec_command;
mod mutation;
pub(crate) mod output;
mod path;
mod read;
mod terminal_updates;
mod write;
mod write_stdin;

use std::sync::Arc;

use crate::{AgentTool, ToolError, ToolFactory, ToolRegistry};

/// Factory for local pi-compatible coding tools.
pub struct BuiltinToolFactory {
    filesystem_enabled: bool,
    shell_enabled: bool,
}

impl BuiltinToolFactory {
    /// Creates a factory with pi's filesystem and shell tool groups enabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            filesystem_enabled: true,
            shell_enabled: true,
        }
    }

    /// Enables or disables read, write, and edit registration.
    #[must_use]
    pub fn filesystem_enabled(mut self, enabled: bool) -> Self {
        self.filesystem_enabled = enabled;
        self
    }

    /// Enables or disables Codex-style terminal tool registration.
    #[must_use]
    pub fn shell_enabled(mut self, enabled: bool) -> Self {
        self.shell_enabled = enabled;
        self
    }
}

impl Default for BuiltinToolFactory {
    /// Uses the same enabled tool groups as the production constructor.
    fn default() -> Self {
        Self::new()
    }
}

impl ToolFactory for BuiltinToolFactory {
    /// Registers enabled local tools in the shared deterministic registry.
    fn create(&self) -> Result<ToolRegistry, ToolError> {
        let mut registry = ToolRegistry::default();
        if self.filesystem_enabled {
            registry.register(Arc::new(read::ReadTool) as Arc<dyn AgentTool>)?;
            registry
                .register(Arc::new(write::WriteTool) as Arc<dyn AgentTool>)?;
            registry.register(Arc::new(edit::EditTool) as Arc<dyn AgentTool>)?;
        }
        if self.shell_enabled {
            registry
                .register(Arc::new(exec_command::ExecCommandTool)
                    as Arc<dyn AgentTool>)?;
            registry.register(
                Arc::new(write_stdin::WriteStdinTool) as Arc<dyn AgentTool>
            )?;
        }
        Ok(registry)
    }
}

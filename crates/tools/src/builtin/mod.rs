mod bash;
mod edit;
mod grep;
mod mutation;
pub(crate) mod output;
mod path;
mod read;
mod write;

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

    /// Enables or disables read, write, edit, and grep registration.
    #[must_use]
    pub fn filesystem_enabled(mut self, enabled: bool) -> Self {
        self.filesystem_enabled = enabled;
        self
    }

    /// Enables or disables bash registration.
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
            registry.register(Arc::new(grep::GrepTool) as Arc<dyn AgentTool>)?;
        }
        if self.shell_enabled {
            registry.register(Arc::new(bash::BashTool) as Arc<dyn AgentTool>)?;
        }
        Ok(registry)
    }
}

/// Metadata shared by built-in tools that only read the workspace.
///
/// Plan mode lets a tool run only when it is known not to change anything.
fn read_only_metadata() -> &'static protocol::ToolMetadata {
    static READ_ONLY: std::sync::OnceLock<protocol::ToolMetadata> =
        std::sync::OnceLock::new();
    READ_ONLY.get_or_init(|| {
        protocol::ToolMetadata::builder()
            .annotations(
                protocol::ToolAnnotations::builder()
                    .read_only_hint(Some(true))
                    .build(),
            )
            .build()
    })
}

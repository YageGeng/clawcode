//! Built-in tool implementations and registration.

pub mod agents;
pub mod fs;
pub mod shell;
pub mod skill;

use std::sync::Arc;

use crate::{FsBackend, LocalFsBackend, LocalTerminalBackend, TerminalBackend, ToolRegistry};

impl ToolRegistry {
    /// Register basic built-in tools (shell, file I/O) with default local backends.
    pub fn register_builtins(&self) {
        self.register_builtins_with_backends(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
        );
    }

    /// Register basic built-in tools using the provided backends.
    pub fn register_builtins_with_backends(
        &self,
        fs_backend: Arc<dyn FsBackend>,
        terminal_backend: Arc<dyn TerminalBackend>,
    ) {
        self.register(Arc::new(shell::ShellCommand::with_backend(
            terminal_backend,
        )));
        self.register_fs_tools_with_backend(false, fs_backend);
    }

    /// Register basic built-in tools using the provided filesystem backend
    /// and a default local terminal backend.
    pub fn register_builtins_with_fs_backend(&self, fs_backend: Arc<dyn FsBackend>) {
        self.register_builtins_with_backends(fs_backend, Arc::new(LocalTerminalBackend::new()));
    }

    /// Register the skill tool, backed by the given registry.
    pub fn register_skill_tools(&self, registry: Arc<skills::SkillRegistry>) {
        self.register(Arc::new(skill::SkillTool::new(registry)));
    }

    /// Register agent management tools.
    pub fn register_agent_tools(&self, agent_ctrl: Arc<dyn agents::AgentControlRef>) {
        self.register(Arc::new(agents::SpawnAgent::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::SendMessage::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::FollowupTask::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::WaitAgent::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::ListAgents::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::CloseAgent::new(Arc::clone(&agent_ctrl))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that basic built-ins expose apply_patch (edit is gated by is_anthropic).
    #[test]
    fn register_builtins_includes_apply_patch() {
        let registry = ToolRegistry::new();
        registry.register_builtins();

        assert!(registry.get("apply_patch").is_some());
    }
}

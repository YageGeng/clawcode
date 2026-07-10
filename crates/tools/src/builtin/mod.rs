//! Built-in tool implementations and registration.

pub mod agents;
pub mod fs;
pub mod shell;
pub mod skill;

use std::sync::Arc;

use crate::{
    FsBackend, LocalFsBackend, LocalTerminalBackend, TerminalBackend,
    ToolRegistry,
};

use self::fs::FsToolSet;

/// Configuration for registering core built-in tool groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinToolConfig {
    /// Whether filesystem tools should be registered.
    pub enable_fs: bool,
    /// Whether shell tools should be registered.
    pub enable_shell: bool,
}

impl Default for BuiltinToolConfig {
    /// Return the default built-in tool registration policy.
    fn default() -> Self {
        Self {
            enable_fs: true,
            enable_shell: true,
        }
    }
}

impl ToolRegistry {
    /// Register basic built-in tools using the selected file-system tool set.
    pub fn register_builtins_with_fs_tool_set(&self, fs_tool_set: FsToolSet) {
        self.register_builtins_with_backends(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
            fs_tool_set,
        );
    }

    /// Register basic built-in tools using the provided backends and filesystem tool set.
    pub fn register_builtins_with_backends(
        &self,
        fs_backend: Arc<dyn FsBackend>,
        terminal_backend: Arc<dyn TerminalBackend>,
        fs_tool_set: FsToolSet,
    ) {
        self.register_builtins_with_backends_and_config(
            fs_backend,
            terminal_backend,
            fs_tool_set,
            BuiltinToolConfig::default(),
        );
    }

    /// Register basic built-in tools using the provided backends, filesystem tool set, and policy.
    pub fn register_builtins_with_backends_and_config(
        &self,
        fs_backend: Arc<dyn FsBackend>,
        terminal_backend: Arc<dyn TerminalBackend>,
        fs_tool_set: FsToolSet,
        config: BuiltinToolConfig,
    ) {
        if config.enable_shell {
            let shell_runtime =
                Arc::new(shell::ShellRuntime::new(terminal_backend));
            self.register(Arc::new(shell::ShellCommand::with_runtime(
                Arc::clone(&shell_runtime),
            )));
            self.register(Arc::new(shell::ShellCommand::exec_command(
                Arc::clone(&shell_runtime),
            )));
            self.register(Arc::new(shell::WriteStdin::new(shell_runtime)));
        }

        if config.enable_fs {
            self.register_fs_tools_with_backend_and_set(
                false,
                fs_backend,
                fs_tool_set,
            );
        }
    }

    /// Register the skill tool, backed by the given registry.
    pub fn register_skill_tools(&self, registry: Arc<skills::SkillRegistry>) {
        self.register(Arc::new(skill::SkillTool::new(registry)));
    }

    /// Register agent management tools.
    pub fn register_agent_tools(
        &self,
        agent_ctrl: Arc<dyn agents::AgentControlRef>,
    ) {
        self.register(Arc::new(agents::SpawnAgent::new(Arc::clone(
            &agent_ctrl,
        ))));
        self.register(Arc::new(agents::SendMessage::new(Arc::clone(
            &agent_ctrl,
        ))));
        self.register(Arc::new(agents::FollowupTask::new(Arc::clone(
            &agent_ctrl,
        ))));
        self.register(Arc::new(agents::WaitAgent::new(Arc::clone(
            &agent_ctrl,
        ))));
        self.register(Arc::new(agents::ListAgents::new(Arc::clone(
            &agent_ctrl,
        ))));
        self.register(Arc::new(agents::CloseAgent::new(Arc::clone(
            &agent_ctrl,
        ))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that built-in registration can select hashline filesystem tools.
    #[test]
    fn register_builtins_with_fs_tool_set_includes_hashline_edit_file() {
        let registry = ToolRegistry::new();
        registry.register_builtins_with_fs_tool_set(FsToolSet::Hashline);

        assert!(registry.get("edit_file").is_some());
        assert!(registry.get("apply_patch").is_none());
    }

    /// Verifies backend-based built-in registration requires an explicit filesystem tool set.
    #[test]
    fn register_builtins_with_backends_accepts_explicit_fs_tool_set() {
        let registry = ToolRegistry::new();
        registry.register_builtins_with_backends(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
            FsToolSet::Hashline,
        );

        assert!(registry.get("edit_file").is_some());
        assert!(registry.get("apply_patch").is_none());
        assert!(registry.get("exec_command").is_some());
        assert!(registry.get("write_stdin").is_some());
    }

    /// Verifies configurable built-in registration keeps all tool groups enabled by default.
    #[test]
    fn register_builtins_with_config_defaults_to_all_tools() {
        let registry = ToolRegistry::new();
        registry.register_builtins_with_backends_and_config(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
            FsToolSet::Hashline,
            BuiltinToolConfig::default(),
        );

        assert!(registry.get("shell").is_some());
        assert!(registry.get("exec_command").is_some());
        assert!(registry.get("write_stdin").is_some());
        assert!(registry.get("edit_file").is_some());
    }

    /// Verifies configurable built-in registration can omit shell tools.
    #[test]
    fn register_builtins_with_config_can_disable_shell_tools() {
        let registry = ToolRegistry::new();
        registry.register_builtins_with_backends_and_config(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
            FsToolSet::Hashline,
            BuiltinToolConfig {
                enable_shell: false,
                ..BuiltinToolConfig::default()
            },
        );

        assert!(registry.get("shell").is_none());
        assert!(registry.get("exec_command").is_none());
        assert!(registry.get("write_stdin").is_none());
        assert!(registry.get("edit_file").is_some());
    }

    /// Verifies configurable built-in registration can omit filesystem tools.
    #[test]
    fn register_builtins_with_config_can_disable_fs_tools() {
        let registry = ToolRegistry::new();
        registry.register_builtins_with_backends_and_config(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
            FsToolSet::Hashline,
            BuiltinToolConfig {
                enable_fs: false,
                ..BuiltinToolConfig::default()
            },
        );

        assert!(registry.get("shell").is_some());
        assert!(registry.get("exec_command").is_some());
        assert!(registry.get("write_stdin").is_some());
        assert!(registry.get("read_file").is_none());
        assert!(registry.get("write_file").is_none());
        assert!(registry.get("edit_file").is_none());
        assert!(registry.get("grep").is_none());
    }
}

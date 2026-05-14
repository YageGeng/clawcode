//! Built-in tool implementations and registration.

pub mod agents;
pub mod fs;
pub mod shell;
pub mod skill;

use std::sync::Arc;

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register basic built-in tools (shell, file I/O). Takes `&self` so
    /// callers can register through `Arc<ToolRegistry>` after passing it to Kernel.
    pub fn register_builtins(&self) {
        self.register(Arc::new(shell::ShellCommand::new()));
        self.register_fs_tools(false);
    }

    /// Register the skill tool, backed by the given registry.
    /// Separate from `register_builtins` because the registry is created
    /// per-session in the kernel and passed in here.
    pub fn register_skill_tools(&self, registry: Arc<skills::SkillRegistry>) {
        self.register(Arc::new(skill::SkillTool::new(registry)));
    }

    /// Register agent management tools. Separate from `register_builtins` so
    /// callers control the composition order.
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

    /// Verifies that basic built-ins expose both edit and apply_patch tools.
    #[test]
    fn register_builtins_includes_edit_and_apply_patch() {
        let registry = ToolRegistry::new();
        registry.register_builtins();

        assert!(registry.get("edit").is_some());
        assert!(registry.get("apply_patch").is_some());
    }
}

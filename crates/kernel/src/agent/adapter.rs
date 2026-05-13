//! Adapter that implements the tools crate's [`AgentControlRef`] trait
//! using our [`AgentControl`]. This bridges the kernel and tools crates
//! without creating a circular dependency.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::AgentPath;
use tools::builtin::agents::AgentControlRef;

use super::control::AgentControl;

/// Adapter wrapping `AgentControl` to implement `tools::AgentControlRef`.
/// Public so binary crates can construct it and pass it to `ToolRegistry`.
pub struct AgentControlAdapter {
    inner: Arc<AgentControl>,
}

impl AgentControlAdapter {
    /// Create a new adapter wrapping the given AgentControl.
    pub fn new(inner: Arc<AgentControl>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl AgentControlRef for AgentControlAdapter {
    /// Delegates to [`AgentControl::spawn`] and returns a JSON summary
    /// with the new agent's path and nickname.
    /// Delegates to [`AgentControl::spawn`] and returns a JSON summary
    /// with the new agent's path and nickname.
    ///
    /// Note: `task_name` is currently hardcoded to `"task"` because the
    /// tools crate does not yet receive the task name from the LLM's
    /// tool call arguments. See the plan's Known Limitations.
    async fn spawn_agent(
        &self,
        parent_path: &AgentPath,
        task_name: &str,
        role: &str,
        prompt: &str,
        cwd: PathBuf,
    ) -> Result<String, String> {
        let live = self
            .inner
            .spawn(parent_path, task_name, role, prompt, cwd)
            .await?;

        let path = live
            .metadata
            .agent_path
            .map(|p| p.to_string())
            .unwrap_or_default();

        let nick = live.metadata.agent_nickname.unwrap_or_default();

        Ok(serde_json::json!({
            "agent_path": path,
            "nickname": nick
        })
        .to_string())
    }

    async fn resolve_target(&self, target: &str) -> Result<AgentPath, String> {
        self.inner.resolve_target(target)
    }

    async fn send_message_to(
        &self,
        from: AgentPath,
        to: AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String> {
        self.inner
            .send_message(from, to, content, trigger_turn)
            .await
    }

    fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<String> {
        self.inner
            .list_agents(prefix)
            .into_iter()
            .map(|a| a.agent_name)
            .collect()
    }

    async fn close_agent(&self, agent_path: &AgentPath) -> Result<(), String> {
        self.inner.close_agent(agent_path).await
    }
}

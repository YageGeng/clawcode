//! Adapter that implements the tools crate's [`AgentControlRef`] trait
//! using our [`AgentControl`]. This bridges the kernel and tools crates
//! without creating a circular dependency.

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{AgentPath, AgentStatus};
use tokio::sync::watch;
use tools::builtin::agents::{
    AgentControlRef, AgentToolSummary, MailboxActivitySubscription,
    SpawnAgentRequest,
};

use super::control::{AgentControl, AgentSpawnRequest};

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
    /// Delegates to [`AgentControl::spawn`] and returns the agent protocol JSON summary.
    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> Result<String, String> {
        let live = self
            .inner
            .spawn({
                let mut spawn_request = AgentSpawnRequest::builder()
                    .parent_path(request.parent_path)
                    .task_name(request.task_name)
                    .role_name(request.role)
                    .prompt(request.prompt)
                    .cwd(request.cwd)
                    .build();
                spawn_request.model = request.model;
                spawn_request
            })
            .await?;

        let path = live
            .metadata
            .agent_path
            .map(|p| p.to_string())
            .unwrap_or_default();

        let nick = live.metadata.agent_nickname.unwrap_or_default();

        Ok(serde_json::json!({
            "task_name": path,
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

    fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<AgentToolSummary> {
        self.inner
            .list_agents(prefix)
            .into_iter()
            .map(|agent| AgentToolSummary {
                agent_name: agent.agent_name,
                agent_status: agent.agent_status,
                last_task_message: agent.last_task_message,
            })
            .collect()
    }

    async fn subscribe_status(
        &self,
        agent_path: &AgentPath,
    ) -> Result<watch::Receiver<AgentStatus>, String> {
        let thread_id = self
            .inner
            .registry
            .agent_id_for_path(agent_path)
            .ok_or_else(|| format!("agent not found: {agent_path}"))?;
        self.inner
            .subscribe_status(&thread_id)
            .await
            .ok_or_else(|| format!("agent status not found: {agent_path}"))
    }

    async fn subscribe_mailbox_activity(
        &self,
        agent_path: &AgentPath,
    ) -> Result<MailboxActivitySubscription, String> {
        self.inner.subscribe_mailbox_activity(agent_path).await
    }

    /// Delegates current-session mailbox observation to [`AgentControl`].
    async fn subscribe_session_mailbox_activity(
        &self,
        session_id: &protocol::SessionId,
    ) -> Result<MailboxActivitySubscription, String> {
        self.inner
            .subscribe_session_mailbox_activity(session_id)
            .await
    }

    /// Delegates wait_agent observation cursor updates to [`AgentControl`].
    async fn observe_session_mailbox_activity(
        &self,
        session_id: &protocol::SessionId,
        epoch: u64,
    ) -> Result<(), String> {
        self.inner
            .observe_session_mailbox_activity(session_id, epoch)
            .await
    }

    async fn close_agent(
        &self,
        agent_path: &AgentPath,
    ) -> Result<AgentStatus, String> {
        self.inner.close_agent(agent_path).await
    }
}

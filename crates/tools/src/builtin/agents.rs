//! Agent management tools: spawn, send_message, followup_task, wait_agent,
//! list_agents, close_agent.
//!
//! These tools allow an LLM to orchestrate sub-agents within a session tree.
//! The actual agent operations are performed through the [`AgentControlRef`]
//! trait, which is implemented by the kernel crate.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::Tool;

/// Object-safe reference to AgentControl operations used by tools.
/// Implemented by the kernel crate's adapter to avoid circular deps.
#[async_trait]
pub trait AgentControlRef: Send + Sync {
    /// Spawn a new sub-agent. Returns JSON with agent_path and nickname.
    async fn spawn_agent(
        &self,
        parent_path: &protocol::AgentPath,
        task_name: &str,
        role: &str,
        prompt: &str,
        cwd: std::path::PathBuf,
    ) -> Result<String, String>;

    /// Resolve a target string (nickname or path) to an AgentPath.
    async fn resolve_target(&self, target: &str) -> Result<protocol::AgentPath, String>;

    /// Send a message to another agent.
    async fn send_message_to(
        &self,
        from: protocol::AgentPath,
        to: protocol::AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String>;

    /// List active sub-agents. Returns agent names.
    fn list_agents(&self, prefix: Option<&protocol::AgentPath>) -> Vec<String>;

    /// Close an agent and its descendants.
    async fn close_agent(&self, agent_path: &protocol::AgentPath) -> Result<(), String>;
}

// ── SpawnAgent ──

pub struct SpawnAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

impl SpawnAgent {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for SpawnAgent {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent to work on a task independently. \
         The sub-agent runs in parallel and can be communicated with \
         via send_message/followup_task."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_name": {
                    "type": "string",
                    "description": "Short kebab-case name for the task (used in agent path)"
                },
                "role": {
                    "type": "string",
                    "enum": ["default", "explorer", "worker"],
                    "default": "default",
                    "description": "Role profile for the sub-agent"
                },
                "prompt": {
                    "type": "string",
                    "description": "Initial task description for the sub-agent"
                }
            },
            "required": ["task_name", "prompt"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        #[serde(default)]
        struct Args {
            task_name: String,
            #[serde(default = "default_role")]
            role: String,
            prompt: String,
        }

        fn default_role() -> String {
            "default".to_string()
        }

        impl Default for Args {
            fn default() -> Self {
                Self {
                    task_name: "task".to_string(),
                    role: "default".to_string(),
                    prompt: String::new(),
                }
            }
        }

        let args: Args =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;

        self.agent_control
            .spawn_agent(
                &ctx.agent_path,
                &args.task_name,
                &args.role,
                &args.prompt,
                ctx.cwd.clone(),
            )
            .await
    }
}

// ── SendMessage ──

pub struct SendMessage {
    agent_control: Arc<dyn AgentControlRef>,
}

impl SendMessage {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for SendMessage {
    fn name(&self) -> &str {
        "send_message"
    }
    fn description(&self) -> &str {
        "Send a message to another agent. The message will be queued and \
         delivered when the target agent next checks its mailbox. Does NOT \
         trigger a turn on its own."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Agent path or nickname" },
                "content": { "type": "string", "description": "Message content" }
            },
            "required": ["to", "content"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let to_str = arguments["to"].as_str().ok_or("missing 'to' argument")?;
        let content = arguments["content"]
            .as_str()
            .ok_or("missing 'content' argument")?;
        let to = self.agent_control.resolve_target(to_str).await?;
        let from = ctx.agent_path.clone();

        self.agent_control
            .send_message_to(from, to, content.to_string(), false)
            .await?;
        Ok("message sent".to_string())
    }
}

// ── FollowupTask ──

pub struct FollowupTask {
    agent_control: Arc<dyn AgentControlRef>,
}

impl FollowupTask {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for FollowupTask {
    fn name(&self) -> &str {
        "followup_task"
    }
    fn description(&self) -> &str {
        "Send a message to another agent and trigger a turn. \
         The target agent will wake up and process the message."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Agent path or nickname" },
                "content": { "type": "string", "description": "Task content for the agent to process" }
            },
            "required": ["to", "content"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let to_str = arguments["to"].as_str().ok_or("missing 'to' argument")?;
        let content = arguments["content"]
            .as_str()
            .ok_or("missing 'content' argument")?;
        let to = self.agent_control.resolve_target(to_str).await?;
        let from = ctx.agent_path.clone();

        self.agent_control
            .send_message_to(from, to, content.to_string(), true)
            .await?;
        Ok("followup sent".to_string())
    }
}

// ── WaitAgent ──

pub struct WaitAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

impl WaitAgent {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for WaitAgent {
    fn name(&self) -> &str {
        "wait_agent"
    }
    fn description(&self) -> &str {
        "Wait for a sub-agent to complete. Returns the agent's final status and message."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_path": {
                    "type": ["string", "null"],
                    "description": "Specific agent to wait for, or null to wait for any sub-agent"
                }
            },
            "required": []
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let prefix = arguments["agent_path"]
            .as_str()
            .map(|s| protocol::AgentPath(s.to_string()));

        let agents = self.agent_control.list_agents(prefix.as_ref());
        Ok(serde_json::to_string(&agents).unwrap_or_else(|_| "[]".to_string()))
    }
}

// ── ListAgents ──

pub struct ListAgents {
    agent_control: Arc<dyn AgentControlRef>,
}

impl ListAgents {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for ListAgents {
    fn name(&self) -> &str {
        "list_agents"
    }
    fn description(&self) -> &str {
        "List all active sub-agents and their statuses."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path_prefix": {
                    "type": ["string", "null"],
                    "description": "Filter by agent path prefix"
                }
            },
            "required": []
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let prefix = arguments["path_prefix"]
            .as_str()
            .map(|s| protocol::AgentPath(s.to_string()));

        let agents = self.agent_control.list_agents(prefix.as_ref());
        Ok(serde_json::to_string(&agents).unwrap_or_else(|_| "[]".to_string()))
    }
}

// ── CloseAgent ──

pub struct CloseAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

impl CloseAgent {
    pub fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self {
            agent_control: ctrl,
        }
    }
}

#[async_trait]
impl Tool for CloseAgent {
    fn name(&self) -> &str {
        "close_agent"
    }
    fn description(&self) -> &str {
        "Close a sub-agent and all its descendants. The agent will no longer \
         be available for communication."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_path": { "type": "string", "description": "Agent path or nickname to close" }
            },
            "required": ["agent_path"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let path_str = arguments["agent_path"]
            .as_str()
            .ok_or("missing 'agent_path' argument")?;
        let path = self.agent_control.resolve_target(path_str).await?;
        self.agent_control.close_agent(&path).await?;
        Ok(format!("agent {path} closed"))
    }
}

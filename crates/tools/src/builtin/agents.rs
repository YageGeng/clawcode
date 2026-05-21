//! Agent management tools: spawn, send_message, followup_task, wait_agent,
//! list_agents, close_agent.
//!
//! These tools allow an LLM to orchestrate sub-agents within a session tree.
//! The actual agent operations are performed through the [`AgentControlRef`]
//! trait, which is implemented by the kernel crate.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use protocol::AgentStatus;
use serde_json::json;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio::time::timeout_at;

use crate::Tool;

/// Default wait timeout for sub-agent completion.
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
/// Minimum accepted wait timeout to avoid tight polling loops.
const MIN_WAIT_TIMEOUT_MS: u64 = 20 * 1_000;
/// Maximum accepted wait timeout to keep tool calls bounded.
const MAX_WAIT_TIMEOUT_MS: u64 = 30 * 60 * 1_000;

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

    /// Subscribe to status changes for a specific agent.
    async fn subscribe_status(
        &self,
        agent_path: &protocol::AgentPath,
    ) -> Result<watch::Receiver<AgentStatus>, String>;

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

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
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

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let to_str = arguments
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or("missing 'to' argument")?;
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
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

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let to_str = arguments
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or("missing 'to' argument")?;
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
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
                },
                "timeout_ms": {
                    "type": ["integer", "null"],
                    "minimum": MIN_WAIT_TIMEOUT_MS,
                    "maximum": MAX_WAIT_TIMEOUT_MS,
                    "description": "Maximum time to wait before returning a timeout"
                }
            },
            "required": []
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let prefix = arguments
            .get("agent_path")
            .and_then(|v| v.as_str())
            .map(|s| protocol::AgentPath(s.to_string()));
        let timeout_ms = arguments
            .get("timeout_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
            .clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS);

        let mut receivers = Vec::new();
        let mut immediate_statuses = std::collections::HashMap::new();
        if let Some(target) = prefix {
            let resolved = self.agent_control.resolve_target(target.as_str()).await?;
            match self.agent_control.subscribe_status(&resolved).await {
                Ok(rx) => receivers.push((target.to_string(), rx)),
                Err(_) => {
                    immediate_statuses.insert(target.to_string(), AgentStatus::NotFound);
                }
            }
        } else {
            for agent in self.agent_control.list_agents(None) {
                let resolved = self.agent_control.resolve_target(&agent).await?;
                match self.agent_control.subscribe_status(&resolved).await {
                    Ok(rx) => receivers.push((agent, rx)),
                    Err(_) => {
                        immediate_statuses.insert(agent, AgentStatus::NotFound);
                    }
                }
            }
        }

        let result = wait_for_agent_statuses(receivers, immediate_statuses, timeout_ms).await;

        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}

#[derive(Debug, serde::Serialize)]
struct WaitAgentResult {
    status: std::collections::HashMap<String, AgentStatus>,
    timed_out: bool,
}

/// Waits for at least one subscribed agent to reach a final status.
async fn wait_for_agent_statuses(
    receivers: Vec<(String, watch::Receiver<AgentStatus>)>,
    mut statuses: std::collections::HashMap<String, AgentStatus>,
    timeout_ms: u64,
) -> WaitAgentResult {
    for (target, rx) in &receivers {
        let status = rx.borrow().clone();
        if status.is_final() {
            statuses.insert(target.clone(), status);
        }
    }
    if !statuses.is_empty() {
        return WaitAgentResult {
            status: statuses,
            timed_out: false,
        };
    }

    let mut futures = FuturesUnordered::new();
    for (target, rx) in receivers {
        futures.push(wait_for_final_status(target, rx));
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match timeout_at(deadline, futures.next()).await {
            Ok(Some(Some((target, status)))) => {
                statuses.insert(target, status);
                break;
            }
            Ok(Some(None)) => continue,
            Ok(None) | Err(_) => break,
        }
    }

    if !statuses.is_empty() {
        loop {
            match futures.next().now_or_never() {
                Some(Some(Some((target, status)))) => {
                    statuses.insert(target, status);
                }
                Some(Some(None)) => continue,
                Some(None) | None => break,
            }
        }
    }

    WaitAgentResult {
        timed_out: statuses.is_empty(),
        status: statuses,
    }
}

/// Waits for one status watcher to report a final agent status.
async fn wait_for_final_status(
    target: String,
    mut status_rx: watch::Receiver<AgentStatus>,
) -> Option<(String, AgentStatus)> {
    let status = status_rx.borrow().clone();
    if status.is_final() {
        return Some((target, status));
    }

    loop {
        if status_rx.changed().await.is_err() {
            return None;
        }
        let status = status_rx.borrow().clone();
        if status.is_final() {
            return Some((target, status));
        }
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

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let prefix = arguments
            .get("path_prefix")
            .and_then(|v| v.as_str())
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

    fn needs_approval(&self, _args: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let path_str = arguments
            .get("agent_path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'agent_path' argument")?;
        let path = self.agent_control.resolve_target(path_str).await?;
        self.agent_control.close_agent(&path).await?;
        Ok(format!("agent {path} closed"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use protocol::{AgentPath, AgentStatus, ToolContext};
    use tokio::sync::watch;

    use super::*;

    /// In-memory agent control used by wait-agent tool tests.
    struct FakeAgentControl {
        paths: HashMap<String, AgentPath>,
        statuses: Mutex<HashMap<String, watch::Sender<AgentStatus>>>,
        _status_rx: watch::Receiver<AgentStatus>,
    }

    impl FakeAgentControl {
        /// Creates a fake control with one child agent in a running state.
        fn with_running_child() -> (Arc<Self>, watch::Sender<AgentStatus>) {
            let (tx, status_rx) = watch::channel(AgentStatus::Running);
            let mut paths = HashMap::new();
            paths.insert("child".to_string(), AgentPath::root().join("child"));
            let mut statuses = HashMap::new();
            statuses.insert("/root/child".to_string(), tx.clone());
            (
                Arc::new(Self {
                    paths,
                    statuses: Mutex::new(statuses),
                    _status_rx: status_rx,
                }),
                tx,
            )
        }
    }

    #[async_trait]
    impl AgentControlRef for FakeAgentControl {
        /// Test stub: spawning is not used by wait-agent tests.
        async fn spawn_agent(
            &self,
            _parent_path: &AgentPath,
            _task_name: &str,
            _role: &str,
            _prompt: &str,
            _cwd: std::path::PathBuf,
        ) -> Result<String, String> {
            Err("not implemented".to_string())
        }

        /// Resolves a nickname or path into a fake agent path.
        async fn resolve_target(&self, target: &str) -> Result<AgentPath, String> {
            self.paths
                .get(target)
                .cloned()
                .or_else(|| {
                    target
                        .starts_with('/')
                        .then(|| AgentPath(target.to_string()))
                })
                .ok_or_else(|| format!("agent not found: {target}"))
        }

        /// Test stub: message sending is not used by wait-agent tests.
        async fn send_message_to(
            &self,
            _from: AgentPath,
            _to: AgentPath,
            _content: String,
            _trigger_turn: bool,
        ) -> Result<(), String> {
            Ok(())
        }

        /// Lists the fake child agent by nickname.
        fn list_agents(&self, _prefix: Option<&AgentPath>) -> Vec<String> {
            let statuses = self
                .statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(status) = statuses
                .get("/root/child")
                .map(|sender| sender.borrow().clone())
            else {
                return Vec::new();
            };
            if status.is_final() {
                Vec::new()
            } else {
                vec!["child".to_string()]
            }
        }

        /// Test stub: close is not used by wait-agent tests.
        async fn close_agent(&self, _agent_path: &AgentPath) -> Result<(), String> {
            Ok(())
        }

        /// Subscribes to a fake status watcher for wait-agent tests.
        async fn subscribe_status(
            &self,
            agent_path: &AgentPath,
        ) -> Result<watch::Receiver<AgentStatus>, String> {
            self.statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(agent_path.as_str())
                .map(watch::Sender::subscribe)
                .ok_or_else(|| format!("agent not found: {agent_path}"))
        }
    }

    /// Verifies wait_agent blocks until a child reaches a terminal status.
    #[tokio::test]
    async fn wait_agent_waits_for_child_terminal_status() {
        let (control, status_tx) = FakeAgentControl::with_running_child();
        let tool = WaitAgent::new(control);
        let ctx = ToolContext::for_test(Path::new("."));

        let waiter = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({
                    "agent_path": "child",
                    "timeout_ms": 500
                }),
                &ctx,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        status_tx
            .send(AgentStatus::Completed {
                message: Some("done".to_string()),
            })
            .expect("send status");

        let output = waiter.await.expect("wait task").expect("wait output");

        assert!(output.contains("\"timed_out\":false"), "{output}");
        assert!(output.contains("completed"), "{output}");
        assert!(output.contains("done"), "{output}");
    }

    /// Verifies wait_agent uses the expected timeout bounds.
    #[test]
    fn wait_agent_timeout_bounds_are_configured() {
        assert_eq!(DEFAULT_WAIT_TIMEOUT_MS, 5 * 60 * 1_000);
        assert_eq!(MIN_WAIT_TIMEOUT_MS, 20 * 1_000);
        assert_eq!(MAX_WAIT_TIMEOUT_MS, 30 * 60 * 1_000);
    }

    /// Verifies untargeted waits do not return already-completed agents again.
    #[tokio::test]
    async fn wait_agent_without_target_ignores_initial_completed_agents() {
        let (control, status_tx) = FakeAgentControl::with_running_child();
        status_tx
            .send(AgentStatus::Completed {
                message: Some("done".to_string()),
            })
            .expect("send status");
        let tool = WaitAgent::new(control);
        let ctx = ToolContext::for_test(Path::new("."));

        let output = tool
            .execute(
                serde_json::json!({
                    "timeout_ms": 1
                }),
                &ctx,
            )
            .await
            .expect("wait output");

        assert!(output.contains("\"timed_out\":true"), "{output}");
        assert!(!output.contains("done"), "{output}");
    }
}

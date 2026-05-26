//! Agent management tools: spawn, send_message, followup_task, wait_agent,
//! list_agents, close_agent.
//!
//! These tools allow an LLM to orchestrate sub-agents within a session tree.
//! The actual agent operations are performed through the [`AgentControlRef`]
//! trait, which is implemented by the kernel crate.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
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

/// Public agent summary returned by the control plane to V2 agent tools.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AgentToolSummary {
    pub agent_name: String,
    pub agent_status: AgentStatus,
    pub last_task_message: Option<String>,
}

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

    /// List active sub-agents with their model-visible V2 summary fields.
    fn list_agents(&self, prefix: Option<&protocol::AgentPath>) -> Vec<AgentToolSummary>;

    /// Subscribe to status changes for a specific agent.
    async fn subscribe_status(
        &self,
        agent_path: &protocol::AgentPath,
    ) -> Result<watch::Receiver<AgentStatus>, String>;

    /// Subscribe to mailbox activity for a specific agent.
    async fn subscribe_mailbox_activity(
        &self,
        agent_path: &protocol::AgentPath,
    ) -> Result<watch::Receiver<()>, String>;

    /// Subscribe to mailbox activity for the current executing session.
    async fn subscribe_session_mailbox_activity(
        &self,
        session_id: &protocol::SessionId,
    ) -> Result<watch::Receiver<()>, String>;

    /// Close an agent and its descendants, returning its previous status.
    async fn close_agent(&self, agent_path: &protocol::AgentPath) -> Result<AgentStatus, String>;
}

// ── SpawnAgent ──

pub struct SpawnAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Strict MultiAgent V2 arguments for spawning a sub-agent.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    task_name: String,
    #[serde(default)]
    agent_type: Option<String>,
    message: String,
}

impl SpawnAgentArgs {
    /// Return the internal role name requested by the V2 agent_type field.
    fn role_name(&self) -> &str {
        self.agent_type.as_deref().unwrap_or("default")
    }
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
        "Spawns an agent to work on the specified task. If your current task is `/root/task1` and you spawn_agent with task_name \"task_3\" the agent will have canonical task name `/root/task1/task_3`."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_name": {
                    "type": "string",
                    "description": "Task name for the spawned agent"
                },
                "agent_type": {
                    "type": "string",
                    "enum": ["default", "explorer", "worker"],
                    "default": "default",
                    "description": "Optional role profile for the sub-agent."
                },
                "message": {
                    "type": "string",
                    "description": "Initial task description for the sub-agent"
                }
            },
            "required": ["task_name", "message"]
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
        let args: SpawnAgentArgs =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;

        self.agent_control
            .spawn_agent(
                &ctx.agent_path,
                &args.task_name,
                args.role_name(),
                &args.message,
                ctx.cwd.clone(),
            )
            .await
    }
}

// ── SendMessage ──

pub struct SendMessage {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Strict MultiAgent V2 arguments for text-only agent messaging.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArgs {
    target: String,
    message: String,
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
        "Send a message to an existing agent. The message will be delivered promptly. Does not trigger a new turn."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Relative or canonical task name to message (from spawn_agent)." },
                "message": { "type": "string", "description": "Message text to queue on the target agent." }
            },
            "required": ["target", "message"]
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
        let args: MessageArgs =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;
        validate_message_content(&args.message)?;
        let to = self.agent_control.resolve_target(&args.target).await?;
        let from = ctx.agent_path.clone();

        self.agent_control
            .send_message_to(from, to, args.message, false)
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
        "Send a message to an existing non-root target agent and trigger a turn in that target. If the target is currently mid-turn, the message is queued and will be used to start the target's next turn, after the current turn completes."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Agent id or canonical task name to message (from spawn_agent)." },
                "message": { "type": "string", "description": "Message text to send to the target agent." }
            },
            "required": ["target", "message"]
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
        let args: MessageArgs =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;
        validate_message_content(&args.message)?;
        let to = self.agent_control.resolve_target(&args.target).await?;
        if to.is_root() {
            return Err("Tasks can't be assigned to the root agent".to_string());
        }
        let from = ctx.agent_path.clone();

        self.agent_control
            .send_message_to(from, to, args.message, true)
            .await?;
        Ok("followup sent".to_string())
    }
}

/// Validate that a V2 inter-agent text message contains non-whitespace content.
fn validate_message_content(message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("Empty message can't be sent to an agent".to_string());
    }
    Ok(())
}

// ── WaitAgent ──

pub struct WaitAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Strict MultiAgent V2 arguments for waiting on mailbox activity.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitAgentArgs {
    timeout_ms: Option<i64>,
}

impl WaitAgentArgs {
    /// Validate timeout bounds and return the effective wait duration.
    fn timeout_ms(self) -> Result<u64, String> {
        match self.timeout_ms {
            Some(ms) if ms < MIN_WAIT_TIMEOUT_MS as i64 => {
                Err(format!("timeout_ms must be at least {MIN_WAIT_TIMEOUT_MS}"))
            }
            Some(ms) if ms > MAX_WAIT_TIMEOUT_MS as i64 => {
                Err(format!("timeout_ms must be at most {MAX_WAIT_TIMEOUT_MS}"))
            }
            Some(ms) => u64::try_from(ms)
                .map_err(|_error| format!("timeout_ms must be at least {MIN_WAIT_TIMEOUT_MS}")),
            None => Ok(DEFAULT_WAIT_TIMEOUT_MS),
        }
    }
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
        "Wait for a mailbox update from any live agent, including queued messages and final-status notifications. Does not return the content; returns either a summary of which agents have updates (if any), or a timeout summary if no mailbox update arrives before the deadline."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_ms": {
                    "type": ["integer", "null"],
                    "minimum": MIN_WAIT_TIMEOUT_MS,
                    "maximum": MAX_WAIT_TIMEOUT_MS,
                    "description": "Optional timeout in milliseconds."
                }
            }
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
        let timeout_ms = serde_json::from_value::<WaitAgentArgs>(arguments)
            .map_err(|e| format!("invalid arguments: {e}"))?
            .timeout_ms()?;

        let mailbox_rx = self
            .agent_control
            .subscribe_session_mailbox_activity(&ctx.session_id)
            .await?;
        let result = wait_for_session_mailbox_update(mailbox_rx, timeout_ms).await;

        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}

#[derive(Debug, serde::Serialize)]
struct WaitAgentResult {
    message: String,
    timed_out: bool,
}

/// Wait for the current session mailbox to receive a model-visible update.
async fn wait_for_session_mailbox_update(
    mut mailbox_rx: watch::Receiver<()>,
    timeout_ms: u64,
) -> WaitAgentResult {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let timed_out = !matches!(timeout_at(deadline, mailbox_rx.changed()).await, Ok(Ok(())));
    WaitAgentResult {
        message: wait_agent_message(timed_out),
        timed_out,
    }
}

/// Return the Codex V2 wait summary text for the timeout outcome.
fn wait_agent_message(timed_out: bool) -> String {
    if timed_out {
        "Wait timed out.".to_string()
    } else {
        "Wait completed.".to_string()
    }
}

// ── ListAgents ──

pub struct ListAgents {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Strict MultiAgent V2 arguments for listing agents.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAgentsArgs {
    path_prefix: Option<String>,
}

impl ListAgentsArgs {
    /// Resolve a list prefix relative to the current agent path.
    fn resolved_prefix(
        &self,
        current_agent_path: &protocol::AgentPath,
    ) -> Option<protocol::AgentPath> {
        self.path_prefix.as_deref().map(|prefix| {
            if prefix.starts_with('/') {
                protocol::AgentPath(prefix.to_string())
            } else {
                current_agent_path.join(prefix)
            }
        })
    }
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
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let args: ListAgentsArgs =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;
        let prefix = args.resolved_prefix(&ctx.agent_path);

        let agents = self.agent_control.list_agents(prefix.as_ref());
        serde_json::to_string(&serde_json::json!({ "agents": agents }))
            .map_err(|error| error.to_string())
    }
}

// ── CloseAgent ──

pub struct CloseAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Strict MultiAgent V2 arguments for closing an agent.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseAgentArgs {
    target: String,
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
        "Close an agent and any open descendants when they are no longer needed, and return the target agent's previous status before shutdown was requested. Don't keep agents open for too long if they are not needed anymore."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Agent id or canonical task name to close (from spawn_agent)." }
            },
            "required": ["target"]
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
        let args: CloseAgentArgs =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;
        let path = self.agent_control.resolve_target(&args.target).await?;
        if path.is_root() {
            return Err("The root agent can't be closed with close_agent".to_string());
        }
        let status = self.agent_control.close_agent(&path).await?;
        serde_json::to_string(&serde_json::json!({ "status": status }))
            .map_err(|error| error.to_string())
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
    use crate::ToolRegistry;

    /// Build a test tool context rooted at `cwd`.
    fn test_context(cwd: impl Into<std::path::PathBuf>) -> ToolContext {
        ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(cwd.into())
            .agent_path(AgentPath::root())
            .approval_mode(protocol::ApprovalMode::default())
            .build()
    }

    /// In-memory agent control used by wait-agent tool tests.
    struct FakeAgentControl {
        paths: HashMap<String, AgentPath>,
        statuses: Mutex<HashMap<String, watch::Sender<AgentStatus>>>,
        mailbox_activity: Mutex<HashMap<String, watch::Sender<()>>>,
        session_mailbox_activity: watch::Sender<()>,
        last_list_prefix: Mutex<Option<AgentPath>>,
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
            let (mailbox_tx, _mailbox_rx) = watch::channel(());
            let mut mailbox_activity = HashMap::new();
            mailbox_activity.insert("/root/child".to_string(), mailbox_tx);
            let (session_mailbox_activity, _session_mailbox_rx) = watch::channel(());
            (
                Arc::new(Self {
                    paths,
                    statuses: Mutex::new(statuses),
                    mailbox_activity: Mutex::new(mailbox_activity),
                    session_mailbox_activity,
                    last_list_prefix: Mutex::new(None),
                    _status_rx: status_rx,
                }),
                tx,
            )
        }

        /// Signals a fake mailbox update for the current session.
        fn notify_session_mailbox(&self) {
            self.session_mailbox_activity.send_replace(());
        }

        /// Return the most recent prefix passed to list_agents.
        fn last_prefix(&self) -> Option<AgentPath> {
            self.last_list_prefix
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[async_trait]
    impl AgentControlRef for FakeAgentControl {
        /// Test stub: spawning is not used by wait-agent tests.
        async fn spawn_agent(
            &self,
            parent_path: &AgentPath,
            task_name: &str,
            _role: &str,
            _prompt: &str,
            _cwd: std::path::PathBuf,
        ) -> Result<String, String> {
            Ok(serde_json::json!({
                "task_name": parent_path.join(task_name).to_string(),
                "nickname": "child"
            })
            .to_string())
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

        /// Lists the fake child agent by canonical path.
        fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<AgentToolSummary> {
            *self
                .last_list_prefix
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = prefix.cloned();
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
                vec![AgentToolSummary {
                    agent_name: "/root/child".to_string(),
                    agent_status: status,
                    last_task_message: None,
                }]
            }
        }

        /// Test stub: close is not used by wait-agent tests.
        async fn close_agent(&self, _agent_path: &AgentPath) -> Result<AgentStatus, String> {
            Ok(AgentStatus::Running)
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

        /// Subscribes to a fake mailbox watcher for wait-agent tests.
        async fn subscribe_mailbox_activity(
            &self,
            agent_path: &AgentPath,
        ) -> Result<watch::Receiver<()>, String> {
            self.mailbox_activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(agent_path.as_str())
                .map(watch::Sender::subscribe)
                .ok_or_else(|| format!("agent not found: {agent_path}"))
        }

        /// Subscribes to the fake current-session mailbox watcher.
        async fn subscribe_session_mailbox_activity(
            &self,
            _session_id: &protocol::SessionId,
        ) -> Result<watch::Receiver<()>, String> {
            Ok(self.session_mailbox_activity.subscribe())
        }
    }

    /// Verifies wait_agent validates timeout before observing status-only updates.
    #[tokio::test]
    async fn wait_agent_rejects_short_timeout_before_status_only_updates() {
        let (control, status_tx) = FakeAgentControl::with_running_child();
        let tool = WaitAgent::new(control);
        let ctx = test_context(Path::new("."));

        status_tx
            .send(AgentStatus::Completed {
                message: Some("done".to_string()),
            })
            .expect("send status");

        let error = tool
            .execute(serde_json::json!({ "timeout_ms": 500 }), &ctx)
            .await
            .expect_err("too-short timeout should be rejected");

        assert_eq!(error, "timeout_ms must be at least 20000");
    }

    /// Verifies wait_agent validates timeout before checking terminal child state.
    #[tokio::test]
    async fn wait_agent_rejects_short_timeout_for_completed_target() {
        let (control, status_tx) = FakeAgentControl::with_running_child();
        status_tx
            .send(AgentStatus::Completed {
                message: Some("done".to_string()),
            })
            .expect("send status");
        let tool = WaitAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(serde_json::json!({ "timeout_ms": 500 }), &ctx)
            .await
            .expect_err("too-short timeout should be rejected");

        assert_eq!(error, "timeout_ms must be at least 20000");
    }

    #[tokio::test]
    async fn wait_agent_returns_for_mailbox_activity() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let notifier = Arc::clone(&control);
        let tool = WaitAgent::new(control);
        let ctx = test_context(Path::new("."));

        let waiter = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({
                    "timeout_ms": 20000
                }),
                &ctx,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        notifier.notify_session_mailbox();

        let output = waiter.await.expect("wait task").expect("wait output");

        assert!(output.contains("\"timed_out\":false"), "{output}");
        assert!(
            output.contains("\"message\":\"Wait completed.\""),
            "{output}"
        );
    }

    /// Verifies wait_agent uses the expected timeout bounds.
    #[test]
    fn wait_agent_timeout_bounds_are_configured() {
        assert_eq!(DEFAULT_WAIT_TIMEOUT_MS, 5 * 60 * 1_000);
        assert_eq!(MIN_WAIT_TIMEOUT_MS, 20 * 1_000);
        assert_eq!(MAX_WAIT_TIMEOUT_MS, 30 * 60 * 1_000);
    }

    #[test]
    fn send_message_uses_codex_v2_target_message_parameters() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SendMessage::new(control);
        let parameters = tool.parameters();
        let properties = parameters
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("object properties");

        assert!(properties.contains_key("target"));
        assert!(properties.contains_key("message"));
        assert!(!properties.contains_key("to"));
        assert!(!properties.contains_key("content"));
        assert_eq!(
            parameters.get("required"),
            Some(&serde_json::json!(["target", "message"]))
        );
    }

    #[test]
    fn followup_task_uses_codex_v2_target_message_parameters() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = FollowupTask::new(control);
        let parameters = tool.parameters();
        let properties = parameters
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("object properties");

        assert!(properties.contains_key("target"));
        assert!(properties.contains_key("message"));
        assert!(!properties.contains_key("to"));
        assert!(!properties.contains_key("content"));
        assert_eq!(
            parameters.get("required"),
            Some(&serde_json::json!(["target", "message"]))
        );
    }

    #[test]
    fn wait_agent_uses_codex_v2_timeout_only_parameters() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = WaitAgent::new(control);
        let parameters = tool.parameters();
        let properties = parameters
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("object properties");

        assert_eq!(properties.len(), 1);
        assert!(properties.contains_key("timeout_ms"));
        assert!(parameters.get("required").is_none());
    }

    #[tokio::test]
    async fn spawn_agent_returns_codex_v2_task_name_output() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SpawnAgent::new(control);
        let ctx = test_context(Path::new("."));

        let output = tool
            .execute(
                serde_json::json!({
                    "task_name": "child",
                    "message": "do work"
                }),
                &ctx,
            )
            .await
            .expect("spawn output");
        let payload: serde_json::Value =
            serde_json::from_str(&output).expect("spawn output should be json");

        assert_eq!(
            payload.get("task_name"),
            Some(&serde_json::json!("/root/child"))
        );
        assert_eq!(payload.get("nickname"), Some(&serde_json::json!("child")));
        assert!(payload.get("agent_path").is_none());
    }

    #[tokio::test]
    async fn spawn_agent_accepts_codex_v2_agent_type_parameter() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SpawnAgent::new(control);
        let ctx = test_context(Path::new("."));

        let output = tool
            .execute(
                serde_json::json!({
                    "task_name": "child",
                    "agent_type": "worker",
                    "message": "do work"
                }),
                &ctx,
            )
            .await
            .expect("spawn output");
        let payload: serde_json::Value =
            serde_json::from_str(&output).expect("spawn output should be json");

        assert_eq!(
            payload.get("task_name"),
            Some(&serde_json::json!("/root/child"))
        );
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
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "timeout_ms": 1
                }),
                &ctx,
            )
            .await
            .expect_err("too-short timeout should be rejected");

        assert_eq!(error, "timeout_ms must be at least 20000");
    }

    #[tokio::test]
    async fn spawn_agent_rejects_missing_task_name() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SpawnAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "message": "do work"
                }),
                &ctx,
            )
            .await
            .expect_err("missing task_name should be rejected");

        assert!(error.contains("missing field `task_name`"), "{error}");
    }

    #[tokio::test]
    async fn spawn_agent_rejects_unknown_fields() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SpawnAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "task_name": "child",
                    "message": "do work",
                    "prompt": "legacy"
                }),
                &ctx,
            )
            .await
            .expect_err("unknown fields should be rejected");

        assert!(error.contains("unknown field"), "{error}");
    }

    #[tokio::test]
    async fn wait_agent_rejects_timeout_below_minimum() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = WaitAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(serde_json::json!({ "timeout_ms": 1 }), &ctx)
            .await
            .expect_err("timeout below minimum should be rejected");

        assert_eq!(error, "timeout_ms must be at least 20000");
    }

    #[tokio::test]
    async fn wait_agent_rejects_timeout_above_maximum() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = WaitAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(serde_json::json!({ "timeout_ms": 1_800_001 }), &ctx)
            .await
            .expect_err("timeout above maximum should be rejected");

        assert_eq!(error, "timeout_ms must be at most 1800000");
    }

    #[test]
    fn registered_agent_tools_do_not_include_resume_agent_in_v2() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let registry = ToolRegistry::new();

        registry.register_agent_tools(control);

        assert!(registry.get("resume_agent").is_none());
    }

    #[tokio::test]
    async fn send_message_rejects_empty_message() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = SendMessage::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "target": "child",
                    "message": "   "
                }),
                &ctx,
            )
            .await
            .expect_err("empty messages should be rejected");

        assert_eq!(error, "Empty message can't be sent to an agent");
    }

    #[tokio::test]
    async fn followup_task_rejects_root_target() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = FollowupTask::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "target": "/root",
                    "message": "do this"
                }),
                &ctx,
            )
            .await
            .expect_err("followup_task should not target root");

        assert_eq!(error, "Tasks can't be assigned to the root agent");
    }

    #[tokio::test]
    async fn list_agents_returns_codex_v2_object() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = ListAgents::new(control);
        let ctx = test_context(Path::new("."));

        let output = tool
            .execute(serde_json::json!({}), &ctx)
            .await
            .expect("list output");
        let payload: serde_json::Value =
            serde_json::from_str(&output).expect("list output should be json");

        assert!(payload.get("agents").is_some(), "{output}");
        assert_eq!(
            payload["agents"][0]["agent_name"],
            serde_json::json!("/root/child")
        );
        assert_eq!(
            payload["agents"][0]["agent_status"],
            serde_json::json!("running")
        );
        assert_eq!(
            payload["agents"][0]["last_task_message"],
            serde_json::Value::Null
        );
    }

    #[tokio::test]
    async fn list_agents_resolves_relative_prefix_from_current_agent_path() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool_control: Arc<dyn AgentControlRef> = control.clone();
        let tool = ListAgents::new(tool_control);
        let ctx = ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(Path::new(".").to_path_buf())
            .agent_path(AgentPath::root().join("parent"))
            .approval_mode(protocol::ApprovalMode::default())
            .build();

        let _output = tool
            .execute(serde_json::json!({ "path_prefix": "child" }), &ctx)
            .await
            .expect("list output");

        assert_eq!(
            control.last_prefix(),
            Some(AgentPath::root().join("parent/child"))
        );
    }

    #[test]
    fn close_agent_uses_codex_v2_target_parameter() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = CloseAgent::new(control);
        let parameters = tool.parameters();
        let properties = parameters
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("object properties");

        assert!(properties.contains_key("target"));
        assert!(!properties.contains_key("agent_path"));
        assert_eq!(
            parameters.get("required"),
            Some(&serde_json::json!(["target"]))
        );
    }

    #[tokio::test]
    async fn close_agent_returns_previous_status_object() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = CloseAgent::new(control);
        let ctx = test_context(Path::new("."));

        let output = tool
            .execute(
                serde_json::json!({
                    "target": "child"
                }),
                &ctx,
            )
            .await
            .expect("close output");
        let payload: serde_json::Value =
            serde_json::from_str(&output).expect("close output should be json");

        assert_eq!(payload.get("status"), Some(&serde_json::json!("running")));
    }

    #[tokio::test]
    async fn close_agent_rejects_root_target() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = CloseAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(serde_json::json!({ "target": "/root" }), &ctx)
            .await
            .expect_err("close_agent should reject root");

        assert_eq!(error, "The root agent can't be closed with close_agent");
    }

    #[tokio::test]
    async fn close_agent_rejects_unknown_fields() {
        let (control, _status_tx) = FakeAgentControl::with_running_child();
        let tool = CloseAgent::new(control);
        let ctx = test_context(Path::new("."));

        let error = tool
            .execute(
                serde_json::json!({
                    "target": "child",
                    "extra": true
                }),
                &ctx,
            )
            .await
            .expect_err("unknown fields should be rejected");

        assert!(error.contains("unknown field"), "{error}");
    }
}

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Carries the stable caller identity and environment for collaboration tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRuntimeContext {
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub system_prompt: Option<String>,
    pub cwd: Option<String>,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
    #[serde(default)]
    pub subagent_depth: usize,
    /// Snapshot of the configured depth limit captured when this context was built.
    /// The authoritative policy lives in `AgentLoopConfig.max_subagent_depth`;
    /// this field carries a copy for per-tool visibility decisions.
    pub max_subagent_depth: Option<usize>,
}

/// Describes the latest lifecycle state observed for one agent thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Closed,
}

/// Exposes one agent summary in structured tool responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub thread_id: String,
    pub path: String,
    pub name: Option<String>,
    pub status: AgentStatus,
    pub pending_tasks: usize,
    pub unread_mailbox_events: usize,
}

/// Selects the mailbox event kind surfaced by `wait_agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxEventKind {
    Spawned,
    Running,
    Completed,
    Failed,
    Closed,
}

/// Represents one mailbox event emitted by the supervisor runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxEvent {
    pub event_id: u64,
    pub agent_id: String,
    pub path: String,
    pub event_kind: MailboxEventKind,
    pub message: String,
    pub status: AgentStatus,
}

/// Structured request used by the collaboration runtime to spawn a child agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnAgentRequest {
    pub session_id: String,
    pub thread_id: String,
    pub origin: AgentRuntimeContext,
    pub name: Option<String>,
    pub task: Option<String>,
    pub cwd: Option<String>,
    pub system_prompt: Option<String>,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
}

/// Structured response returned after a child agent is registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnAgentResponse {
    pub agent: AgentSummary,
    pub started: bool,
}

/// Structured request used to enqueue more work on an existing agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendAgentInputRequest {
    pub session_id: String,
    pub thread_id: String,
    pub origin: AgentRuntimeContext,
    pub target: String,
    pub input: String,
    pub interrupt: bool,
}

/// Shared acknowledgement returned by send/close agent commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommandAck {
    pub agent: AgentSummary,
    pub queued: bool,
}

/// Structured wait request that selects which agents may wake the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitAgentRequest {
    pub session_id: String,
    pub thread_id: String,
    pub origin: AgentRuntimeContext,
    pub targets: Vec<String>,
    pub timeout_ms: Option<u64>,
}

/// Structured wait result that returns the next matching mailbox event, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitAgentResponse {
    pub timed_out: bool,
    pub event: Option<MailboxEvent>,
}

/// Structured request that closes one agent and its descendants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseAgentRequest {
    pub session_id: String,
    pub thread_id: String,
    pub origin: AgentRuntimeContext,
    pub target: String,
}

/// Structured request that lists all visible agents for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAgentsRequest {
    pub session_id: String,
    pub thread_id: String,
    pub origin: AgentRuntimeContext,
}

/// Structured response that returns the visible agent graph snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentSummary>,
}

/// Future-facing collaboration capability injected by the kernel runtime into tools.
#[async_trait]
pub trait CollaborationRuntime: Send + Sync {
    /// Registers a child agent and optionally starts its first task.
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> Result<SpawnAgentResponse>;

    /// Queues more work for an existing child agent.
    async fn send_agent_input(&self, request: SendAgentInputRequest) -> Result<AgentCommandAck>;

    /// Waits for the next mailbox event that matches the requested target set.
    async fn wait_agent(&self, request: WaitAgentRequest) -> Result<WaitAgentResponse>;

    /// Closes an agent subtree and prevents further work from being queued into it.
    async fn close_agent(&self, request: CloseAgentRequest) -> Result<AgentCommandAck>;

    /// Returns the visible agent snapshot for the current session.
    async fn list_agents(&self, request: ListAgentsRequest) -> Result<ListAgentsResponse>;
}

/// Shared collaboration runtime handle carried through tool execution contexts.
pub type CollaborationRuntimeHandle = Arc<dyn CollaborationRuntime>;

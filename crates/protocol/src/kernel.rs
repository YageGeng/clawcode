//! Agent kernel trait and associated error/stream types.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, stream};

use crate::agent::AgentPath;
use crate::agent_ui::AgentUiMetadata;
use crate::config::{ModelInfo, SessionMode};
use crate::event::Event;
use crate::mcp::McpServerConfig;
use crate::permission::ReviewDecision;
use crate::session::{SessionCreated, SessionId, SessionListPage};

/// Boxed, pinned stream of kernel events.
///
/// Returned by [`AgentKernel::prompt`]; the frontend consumes this
/// to receive real-time updates during a turn.
pub type EventStream =
    Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>;

/// Frontend-provided options used when creating or loading a session.
#[derive(Debug, Clone, Default)]
pub struct SessionLaunchOptions {
    /// MCP servers injected by the frontend for this session only.
    pub external_mcp_servers: Vec<McpServerConfig>,
}

/// Central agent kernel trait.
///
/// Implemented by the kernel crate, consumed by ACP and other
/// frontend protocol adapters. All session management, LLM
/// interaction, and tool execution flows through this interface.
#[async_trait]
pub trait AgentKernel: Send + Sync {
    /// Create a new session and return its ID plus available config.
    async fn new_session(
        &self,
        cwd: PathBuf,
        options: SessionLaunchOptions,
    ) -> Result<SessionCreated, KernelError>;

    /// Load a previously persisted session.
    async fn load_session(
        &self,
        session_id: &SessionId,
        cwd: PathBuf,
        options: SessionLaunchOptions,
    ) -> Result<SessionCreated, KernelError>;

    /// List persisted sessions with optional cwd filter and cursor-based pagination.
    async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError>;

    /// Submit a user prompt, returning a stream of events.
    ///
    /// The stream yields events until the turn completes, then terminates.
    async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<EventStream, KernelError>;

    /// Subscribe to live events for a session without submitting a new prompt.
    ///
    /// Frontends use this for turns started by internal operations, such as
    /// sub-agent initial tasks, that still need real-time transcript updates.
    async fn subscribe_session_events(
        &self,
        _session_id: &SessionId,
    ) -> Result<EventStream, KernelError> {
        Ok(Box::pin(stream::empty()))
    }

    /// Cancel the currently running turn in a session.
    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Set the session's approval/sandboxing mode.
    async fn set_mode(
        &self,
        session_id: &SessionId,
        mode: &str,
    ) -> Result<(), KernelError>;

    /// Switch the model for a session.
    async fn set_model(
        &self,
        session_id: &SessionId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), KernelError>;

    /// Close a session and release its resources.
    async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError>;

    /// Return UI-only agent metadata for a root session tree.
    async fn agent_ui_snapshot(
        &self,
        root_session_id: &SessionId,
    ) -> Result<Vec<AgentUiMetadata>, KernelError>;

    /// Spawn a sub-agent in a parent session.
    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        agent_path: AgentPath,
        role: &str,
        model: Option<&str>,
        prompt: &str,
    ) -> Result<(), KernelError>;

    /// Deliver a tool approval decision to a waiting turn.
    /// Used by frontend adapters (e.g. ACP) to resolve approval requests.
    async fn resolve_approval(
        &self,
        session_id: &SessionId,
        call_id: &str,
        decision: ReviewDecision,
    ) -> Result<(), KernelError>;

    /// Return the available approval/sandboxing mode presets.
    fn available_modes(&self) -> Vec<SessionMode>;

    /// Return the available models from configured providers.
    fn available_models(&self) -> Vec<ModelInfo>;
}

/// Error type for kernel operations.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    #[error("agent not found: {0}")]
    AgentNotFound(AgentPath),

    #[error("authentication required")]
    AuthRequired,

    #[error("operation cancelled")]
    Cancelled,

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

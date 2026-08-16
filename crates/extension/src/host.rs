use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    AgentMessage, CompactionResult, EntryId, ExtensionEntryData,
    ExtensionEventData, ExtensionExecRequest, ExtensionExecResult,
    ExtensionFlagDefinition, ExtensionInvocation, ExtensionMessageDraft,
    ExtensionSnapshot, ExtensionUserMessage, ModelProfile, SessionId,
    SessionSummary, SessionTreeSnapshot, ThinkingLevel,
};

use crate::RegisteredCommand;

/// Failures returned by extension host capabilities.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionHostError {
    /// The owning session runtime has already been invalidated.
    #[error("extension runtime is stale")]
    StaleRuntime,
    /// The owning session is closing and cannot accept run-gated work.
    #[error("session is closing")]
    SessionClosing,
    /// A capability is unavailable in the current host implementation.
    #[error("extension host capability is unavailable: {0}")]
    Unsupported(&'static str),
    /// The requested active tool does not exist in this session.
    #[error("extension tool is not available: {0}")]
    UnknownTool(String),
    /// A host operation failed with a sanitized diagnostic.
    #[error("extension host operation failed: {0}")]
    Operation(String),
}

/// Kernel-owned capabilities available to ordinary extension event handlers.
#[async_trait]
pub trait ExtensionHost: Send + Sync {
    /// Captures current host state for one invocation.
    fn snapshot(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<ExtensionSnapshot, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("snapshot"))
    }

    /// Reports whether the session has no active run.
    fn is_idle(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<bool, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("is_idle"))
    }

    /// Reports whether steering or follow-up messages are queued.
    fn has_pending_messages(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<bool, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("has_pending_messages"))
    }

    /// Requests cancellation of current agent work.
    async fn abort(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("abort"))
    }

    /// Requests graceful application shutdown.
    async fn shutdown(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("shutdown"))
    }

    /// Runs context compaction for the current session.
    async fn compact(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<CompactionResult, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("compact"))
    }

    /// Returns the current complete system prompt text.
    fn system_prompt(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<String, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("system_prompt"))
    }

    /// Persists and emits one identity-free extension message draft.
    async fn send_extension_message(
        &self,
        _invocation: &ExtensionInvocation,
        _draft: ExtensionMessageDraft,
    ) -> Result<AgentMessage, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("send_extension_message"))
    }

    /// Sends or queues one extension-authored user message.
    async fn send_user_message(
        &self,
        _invocation: &ExtensionInvocation,
        _request: ExtensionUserMessage,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("send_user_message"))
    }

    /// Appends one extension-owned custom entry to the active branch.
    async fn append_entry(
        &self,
        _invocation: &ExtensionInvocation,
        _entry: ExtensionEntryData,
    ) -> Result<EntryId, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("append_entry"))
    }

    /// Sets or clears the persisted session name.
    async fn set_session_name(
        &self,
        _invocation: &ExtensionInvocation,
        _name: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("set_session_name"))
    }

    /// Sets or clears a label on one persisted entry.
    async fn set_label(
        &self,
        _invocation: &ExtensionInvocation,
        _entry_id: EntryId,
        _label: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("set_label"))
    }

    /// Executes one server-local process with cancellation support.
    async fn exec(
        &self,
        _invocation: &ExtensionInvocation,
        _request: ExtensionExecRequest,
    ) -> Result<ExtensionExecResult, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("exec"))
    }

    /// Returns tool names active for the next turn.
    fn active_tools(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<String>, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("active_tools"))
    }

    /// Returns every tool name available to the session.
    fn all_tools(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<String>, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("all_tools"))
    }

    /// Replaces the active tool set for future turns.
    async fn set_active_tools(
        &self,
        _invocation: &ExtensionInvocation,
        _names: Vec<String>,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("set_active_tools"))
    }

    /// Registers or replaces one tool in the current session.
    async fn register_tool(
        &self,
        _invocation: &ExtensionInvocation,
        _tool: Arc<dyn tools::AgentTool>,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("register_tool"))
    }

    /// Returns commands available in the current session snapshot.
    fn commands(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<RegisteredCommand>, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("commands"))
    }

    /// Returns immutable registered command-line flags.
    fn flags(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<ExtensionFlagDefinition>, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("flags"))
    }

    /// Registers or replaces one command in the current session.
    async fn register_command(
        &self,
        _invocation: &ExtensionInvocation,
        _command: RegisteredCommand,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("register_command"))
    }

    /// Removes one command owned by the invoking Extension.
    async fn unregister_command(
        &self,
        _invocation: &ExtensionInvocation,
        _name: &str,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("unregister_command"))
    }

    /// Selects a configured model for future turns.
    async fn set_model(
        &self,
        _invocation: &ExtensionInvocation,
        _provider_id: &str,
        _model_id: &str,
    ) -> Result<ModelProfile, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("set_model"))
    }

    /// Returns the active thinking level.
    fn thinking_level(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<ThinkingLevel, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("thinking_level"))
    }

    /// Selects and model-clamps a thinking level for future turns.
    async fn set_thinking_level(
        &self,
        _invocation: &ExtensionInvocation,
        _level: ThinkingLevel,
    ) -> Result<ThinkingLevel, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("set_thinking_level"))
    }

    /// Publishes one session-local extension event-bus value.
    async fn publish_event(
        &self,
        _invocation: &ExtensionInvocation,
        _event: ExtensionEventData,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("publish_event"))
    }

    /// Subscribes to future values from the session-local extension event bus.
    fn subscribe_events(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<
        tokio::sync::broadcast::Receiver<ExtensionEventData>,
        ExtensionHostError,
    > {
        Err(ExtensionHostError::Unsupported("subscribe_events"))
    }

    /// Waits until current session agent work is idle.
    async fn wait_for_idle(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("wait_for_idle"))
    }

    /// Creates a new session rooted at a server-side directory.
    async fn create_session(
        &self,
        _invocation: &ExtensionInvocation,
        _cwd: PathBuf,
    ) -> Result<SessionSummary, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("create_session"))
    }

    /// Restores or switches to one persisted session.
    async fn switch_session(
        &self,
        _invocation: &ExtensionInvocation,
        _session_id: SessionId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("switch_session"))
    }

    /// Forks the active session at one selected entry.
    async fn fork_session(
        &self,
        _invocation: &ExtensionInvocation,
        _entry_id: EntryId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("fork_session"))
    }

    /// Navigates the active session tree to one selected entry.
    async fn navigate_tree(
        &self,
        _invocation: &ExtensionInvocation,
        _entry_id: EntryId,
        _summarize: bool,
    ) -> Result<SessionTreeSnapshot, ExtensionHostError> {
        Err(ExtensionHostError::Unsupported("navigate_tree"))
    }
}

/// Host implementation that intentionally exposes no operational capabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedExtensionHost;

impl ExtensionHost for UnsupportedExtensionHost {}

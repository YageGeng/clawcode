use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use protocol::{
    AgentMessage, CompactionResult, EntryId, ExtensionEntryData,
    ExtensionEventData, ExtensionExecRequest, ExtensionExecResult,
    ExtensionFlagDefinition, ExtensionId, ExtensionInvocation,
    ExtensionMessageDraft, ExtensionSnapshot, ExtensionUserMessage,
    ModelProfile, SessionId, SessionSummary, SessionTreeSnapshot,
    ThinkingLevel,
};

use crate::{ExtensionHost, ExtensionHostError, UnsupportedExtensionHost};

/// Shared validity token for every context created by one session runtime.
pub struct RuntimeGeneration {
    active: Arc<AtomicBool>,
}

impl RuntimeGeneration {
    /// Creates an active session-runtime generation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Invalidates every context sharing this generation.
    pub fn invalidate(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Rejects actions after the owning session runtime has shut down.
    pub fn ensure_active(&self) -> Result<(), ExtensionHostError> {
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(ExtensionHostError::StaleRuntime)
        }
    }
}

impl Default for RuntimeGeneration {
    /// Creates an independent active generation.
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for RuntimeGeneration {
    /// Clones the shared validity token without duplicating state.
    fn clone(&self) -> Self {
        Self {
            active: Arc::clone(&self.active),
        }
    }
}

/// Immutable data and guarded host actions visible to an event handler.
#[derive(typed_builder::TypedBuilder)]
pub struct ExtensionContext {
    /// Correlation and extension identity for this invocation.
    pub invocation: ExtensionInvocation,
    /// Host state captured before the handler starts.
    pub snapshot: ExtensionSnapshot,
    /// Kernel-owned action boundary.
    #[builder(default = Arc::new(UnsupportedExtensionHost))]
    host: Arc<dyn ExtensionHost>,
    /// Shared validity token for this session runtime.
    #[builder(default)]
    generation: RuntimeGeneration,
}

impl Clone for ExtensionContext {
    /// Clones immutable snapshots and shared host/generation handles.
    fn clone(&self) -> Self {
        Self {
            invocation: self.invocation.clone(),
            snapshot: self.snapshot.clone(),
            host: Arc::clone(&self.host),
            generation: self.generation.clone(),
        }
    }
}

impl ExtensionContext {
    /// Clones this immutable snapshot and binds invocation identity to one handler source.
    #[must_use]
    pub fn for_extension(&self, extension_id: &ExtensionId) -> Self {
        let mut context = self.clone();
        context.invocation.extension_id = extension_id.clone();
        context
    }

    /// Returns the guarded host after validating the session generation.
    fn active_host(
        &self,
    ) -> Result<&Arc<dyn ExtensionHost>, ExtensionHostError> {
        self.generation.ensure_active()?;
        Ok(&self.host)
    }

    /// Persists and emits one extension message through the owning kernel.
    pub async fn send_extension_message(
        &self,
        draft: ExtensionMessageDraft,
    ) -> Result<AgentMessage, ExtensionHostError> {
        self.active_host()?
            .send_extension_message(&self.invocation, draft)
            .await
    }

    /// Captures a fresh host snapshot after validating this runtime generation.
    pub fn snapshot(&self) -> Result<ExtensionSnapshot, ExtensionHostError> {
        self.active_host()?.snapshot(&self.invocation)
    }

    /// Sends or queues one extension-authored user message.
    pub async fn send_user_message(
        &self,
        request: ExtensionUserMessage,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .send_user_message(&self.invocation, request)
            .await
    }

    /// Appends one extension-owned custom entry to the active branch.
    pub async fn append_entry(
        &self,
        entry: ExtensionEntryData,
    ) -> Result<EntryId, ExtensionHostError> {
        self.active_host()?
            .append_entry(&self.invocation, entry)
            .await
    }

    /// Reports whether the owning session currently has no active run.
    pub fn is_idle(&self) -> Result<bool, ExtensionHostError> {
        self.active_host()?.is_idle(&self.invocation)
    }

    /// Reports whether the owning session has queued messages.
    pub fn has_pending_messages(&self) -> Result<bool, ExtensionHostError> {
        self.active_host()?.has_pending_messages(&self.invocation)
    }

    /// Requests cancellation of current agent work.
    pub async fn abort(&self) -> Result<(), ExtensionHostError> {
        self.active_host()?.abort(&self.invocation).await
    }

    /// Requests graceful application shutdown when the host supports it.
    pub async fn shutdown(&self) -> Result<(), ExtensionHostError> {
        self.active_host()?.shutdown(&self.invocation).await
    }

    /// Runs context compaction for the owning session.
    pub async fn compact(
        &self,
    ) -> Result<CompactionResult, ExtensionHostError> {
        self.active_host()?.compact(&self.invocation).await
    }

    /// Returns the current system prompt text.
    pub fn system_prompt(&self) -> Result<String, ExtensionHostError> {
        self.active_host()?.system_prompt(&self.invocation)
    }

    /// Sets or clears the persisted session name.
    pub async fn set_session_name(
        &self,
        name: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .set_session_name(&self.invocation, name)
            .await
    }

    /// Sets or clears one persisted entry label.
    pub async fn set_label(
        &self,
        entry_id: EntryId,
        label: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .set_label(&self.invocation, entry_id, label)
            .await
    }

    /// Executes one server-local process under host cancellation policy.
    pub async fn exec(
        &self,
        request: ExtensionExecRequest,
    ) -> Result<ExtensionExecResult, ExtensionHostError> {
        self.active_host()?.exec(&self.invocation, request).await
    }

    /// Returns tool names active for the next turn.
    pub fn active_tools(&self) -> Result<Vec<String>, ExtensionHostError> {
        self.active_host()?.active_tools(&self.invocation)
    }

    /// Returns every tool name available to this session.
    pub fn all_tools(&self) -> Result<Vec<String>, ExtensionHostError> {
        self.active_host()?.all_tools(&self.invocation)
    }

    /// Replaces the active tool names used by future turns.
    pub async fn set_active_tools(
        &self,
        names: Vec<String>,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .set_active_tools(&self.invocation, names)
            .await
    }

    /// Registers or replaces one tool in this session runtime.
    pub async fn register_tool(
        &self,
        tool: Arc<dyn tools::AgentTool>,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .register_tool(&self.invocation, tool)
            .await
    }

    /// Returns commands available in the current session snapshot.
    pub fn commands(
        &self,
    ) -> Result<Vec<crate::RegisteredCommand>, ExtensionHostError> {
        self.active_host()?.commands(&self.invocation)
    }

    /// Returns immutable extension command-line flags.
    pub fn flags(
        &self,
    ) -> Result<Vec<ExtensionFlagDefinition>, ExtensionHostError> {
        self.active_host()?.flags(&self.invocation)
    }

    /// Registers or replaces one session-local extension command.
    pub async fn register_command(
        &self,
        command: crate::RegisteredCommand,
    ) -> Result<(), ExtensionHostError> {
        let command = command.bind_owner(&self.invocation.extension_id);
        self.active_host()?
            .register_command(&self.invocation, command)
            .await
    }

    /// Removes one session-local command owned by this Extension.
    pub async fn unregister_command(
        &self,
        name: &str,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .unregister_command(&self.invocation, name)
            .await
    }

    /// Selects a configured model for future turns.
    pub async fn set_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelProfile, ExtensionHostError> {
        self.active_host()?
            .set_model(&self.invocation, provider_id, model_id)
            .await
    }

    /// Returns the current model-clamped thinking level.
    pub fn thinking_level(
        &self,
    ) -> Result<protocol::ThinkingLevel, ExtensionHostError> {
        self.active_host()?.thinking_level(&self.invocation)
    }

    /// Selects and model-clamps the thinking level for future turns.
    pub async fn set_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<ThinkingLevel, ExtensionHostError> {
        self.active_host()?
            .set_thinking_level(&self.invocation, level)
            .await
    }

    /// Publishes one session-local extension event-bus value.
    pub async fn publish_event(
        &self,
        event: ExtensionEventData,
    ) -> Result<(), ExtensionHostError> {
        self.active_host()?
            .publish_event(&self.invocation, event)
            .await
    }

    /// Subscribes to future values on the session-local extension event bus.
    pub fn subscribe_events(
        &self,
    ) -> Result<
        tokio::sync::broadcast::Receiver<ExtensionEventData>,
        ExtensionHostError,
    > {
        self.active_host()?.subscribe_events(&self.invocation)
    }
}

/// Context reserved for commands that may replace the active session.
#[derive(Clone)]
pub struct ExtensionCommandContext {
    /// Ordinary event context shared by all command actions.
    pub event: ExtensionContext,
}

impl ExtensionCommandContext {
    /// Waits until the owning session is idle without holding a run gate.
    pub async fn wait_for_idle(&self) -> Result<(), ExtensionHostError> {
        self.event
            .active_host()?
            .wait_for_idle(&self.event.invocation)
            .await
    }

    /// Creates a new server-side session from a command workflow.
    pub async fn create_session(
        &self,
        cwd: std::path::PathBuf,
    ) -> Result<SessionSummary, ExtensionHostError> {
        self.event
            .active_host()?
            .create_session(&self.event.invocation, cwd)
            .await
    }

    /// Switches a command workflow to one persisted session.
    pub async fn switch_session(
        &self,
        session_id: SessionId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        self.event
            .active_host()?
            .switch_session(&self.event.invocation, session_id)
            .await
    }

    /// Forks the active session at one selected entry.
    pub async fn fork_session(
        &self,
        entry_id: EntryId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        self.event
            .active_host()?
            .fork_session(&self.event.invocation, entry_id)
            .await
    }

    /// Navigates the active session tree from a command workflow.
    pub async fn navigate_tree(
        &self,
        entry_id: EntryId,
        summarize: bool,
    ) -> Result<SessionTreeSnapshot, ExtensionHostError> {
        self.event
            .active_host()?
            .navigate_tree(&self.event.invocation, entry_id, summarize)
            .await
    }
}

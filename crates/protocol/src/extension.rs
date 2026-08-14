use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{AgentMessage, SessionId, TurnId};

/// Non-UI lifecycle points exposed by pi-compatible extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionEvent {
    /// Project trust is being resolved before project resources are loaded.
    ProjectTrust,
    /// Extension and resource discovery begins.
    ResourcesDiscover,
    /// A session starts or resumes.
    SessionStart,
    /// Persisted session display metadata changed.
    SessionInfoChanged,
    /// A session switch is about to occur.
    SessionBeforeSwitch,
    /// A session switch completed.
    SessionSwitch,
    /// A session fork is about to occur.
    SessionBeforeFork,
    /// A session fork completed.
    SessionFork,
    /// Context compaction is about to occur.
    SessionBeforeCompact,
    /// Context compaction completed.
    SessionCompact,
    /// Session tree navigation is about to occur.
    SessionBeforeTree,
    /// Session tree navigation completed.
    SessionTree,
    /// Raw caller input was received.
    Input,
    /// An agent run is about to start.
    BeforeAgentStart,
    /// An agent run started.
    AgentStart,
    /// One model turn started.
    TurnStart,
    /// Model context is being assembled.
    Context,
    /// A provider request is about to be sent.
    BeforeProviderRequest,
    /// One persisted message started streaming.
    MessageStart,
    /// One persisted message received an update.
    MessageUpdate,
    /// One persisted message completed.
    MessageEnd,
    /// A tool call is about to execute.
    ToolExecutionStart,
    /// A tool call emitted an incremental update.
    ToolExecutionUpdate,
    /// A tool call completed.
    ToolExecutionEnd,
    /// One model turn completed.
    TurnEnd,
    /// An agent run settled.
    AgentEnd,
    /// Agent work is idle with no automatic retry or follow-up remaining.
    AgentSettled,
    /// A session is shutting down.
    SessionShutdown,
    /// Active model selection changed.
    ModelSelect,
    /// Active thinking level changed or was clamped by model selection.
    ThinkingLevelSelect,
    /// Provider headers are about to be sent and may be adjusted.
    BeforeProviderHeaders,
    /// Provider response headers and status are available before stream consumption.
    AfterProviderResponse,
    /// A tool call was parsed and may be transformed or blocked.
    ToolCall,
    /// A completed tool result may be transformed before persistence.
    ToolResult,
    /// A user-authored shell command is about to execute or has completed.
    UserBash,
    /// A protocol client invoked a registered non-UI extension command.
    Command {
        /// Registered command name.
        name: String,
        /// Opaque command arguments preserved from ACP.
        arguments: serde_json::Value,
    },
}

impl fmt::Display for ExtensionEvent {
    /// Writes the stable snake_case lifecycle discriminator.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if matches!(self, Self::Command { .. }) {
            return formatter.write_str("command");
        }
        let value = serde_json::to_value(self)
            .map_err(|_serialization_error| fmt::Error)?;
        formatter.write_str(value.as_str().ok_or(fmt::Error)?)
    }
}

/// Runtime identity passed to every extension hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionContext {
    /// Active session identifier.
    pub session_id: SessionId,
    /// Active turn when the hook occurs inside a turn.
    pub turn_id: Option<TurnId>,
}

/// Control result returned by one extension hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionDirective {
    /// Continue normal lifecycle dispatch.
    Continue,
    /// Prevent the lifecycle operation from proceeding.
    Block {
        /// Human-readable blocking reason.
        reason: String,
    },
    /// Inject additional messages at the current lifecycle boundary.
    Inject {
        /// Ordered messages inserted by the extension.
        messages: Vec<AgentMessage>,
    },
    /// Attach extension-owned metadata for downstream adapters.
    Metadata {
        /// Opaque JSON metadata.
        value: serde_json::Value,
    },
}

/// Metadata for one non-UI command contributed by an extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCommandDefinition {
    /// Unique invocation name.
    pub name: String,
    /// Human-readable command description.
    pub description: Option<String>,
}

/// Supported non-UI CLI flag value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionFlagKind {
    /// Boolean command-line flag.
    Boolean,
    /// String-valued command-line flag.
    String,
}

/// One CLI flag contributed during extension registration.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ExtensionFlagDefinition {
    /// Unique flag name.
    pub name: String,
    /// Human-readable flag description.
    #[builder(default)]
    pub description: Option<String>,
    /// Accepted flag value kind.
    pub kind: ExtensionFlagKind,
    /// Optional JSON default matching the declared kind.
    #[builder(default)]
    pub default: Option<serde_json::Value>,
}

/// Provider registration contributed before kernel model construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionProviderRegistration {
    /// Provider identifier to register or override.
    pub name: String,
    /// Provider-specific configuration preserved for the provider factory.
    pub config: serde_json::Value,
}

/// Composed effects produced by every extension at one lifecycle boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExtensionEffects {
    /// Ordered messages injected by all participating extensions.
    pub injections: Vec<AgentMessage>,
    /// Ordered metadata values contributed for downstream adapters.
    pub metadata: Vec<serde_json::Value>,
    /// First blocking reason, when dispatch stopped early.
    pub block_reason: Option<String>,
}

impl ExtensionEffects {
    /// Applies one directive and reports whether later extensions may run.
    pub fn apply(&mut self, directive: ExtensionDirective) -> bool {
        match directive {
            ExtensionDirective::Continue => true,
            ExtensionDirective::Block { reason } => {
                self.block_reason = Some(reason);
                false
            }
            ExtensionDirective::Inject { messages } => {
                self.injections.extend(messages);
                true
            }
            ExtensionDirective::Metadata { value } => {
                self.metadata.push(value);
                true
            }
        }
    }
}

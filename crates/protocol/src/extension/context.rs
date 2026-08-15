use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, EntryId, ExtensionId, ModelProfile, RunId, SessionId,
    SessionTreeSnapshot, TimestampMs, TurnId,
};

/// Stable identity and correlation supplied to one extension handler call.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ExtensionInvocation {
    /// Extension receiving the invocation.
    pub extension_id: ExtensionId,
    /// Session that owns the lifecycle point.
    pub session_id: SessionId,
    /// Active run when the point occurs inside agent execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub run_id: Option<RunId>,
    /// Active turn when the point occurs inside a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub turn_id: Option<TurnId>,
    /// Invocation creation time in precision-safe Unix milliseconds.
    pub timestamp_ms: TimestampMs,
    /// Server-side working directory for this session.
    pub cwd: PathBuf,
}

/// Immutable host state visible during one extension handler invocation.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct ExtensionSnapshot {
    /// Currently selected model, when one is available.
    #[builder(default, setter(strip_option))]
    pub active_model: Option<ModelProfile>,
    /// Current thinking level selected for the session.
    pub thinking_level: super::ThinkingLevel,
    /// Immutable configured model catalog visible to the session.
    pub models: Vec<ModelProfile>,
    /// Active branch messages at snapshot creation time.
    pub messages: Vec<AgentMessage>,
    /// Current session tree at snapshot creation time.
    pub tree: SessionTreeSnapshot,
    /// Last known context token usage.
    #[builder(default, setter(strip_option))]
    pub context_usage: Option<u64>,
    /// Number of pending steering messages.
    pub pending_steer: usize,
    /// Number of pending follow-up messages.
    pub pending_follow_up: usize,
    /// Names of tools active for the next turn.
    pub active_tools: Vec<String>,
    /// Names of every tool available in this session.
    pub all_tools: Vec<String>,
    /// Whether the owning operation has been cancelled.
    pub cancelled: bool,
}

/// Identity-free custom message submitted to the kernel by an extension.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ExtensionMessageDraft {
    /// Extension that owns the message schema.
    pub extension_id: ExtensionId,
    /// Extension-defined message subtype.
    pub custom_type: String,
    /// Ordered text, reasoning, image, or tool-call blocks.
    pub blocks: Vec<crate::ContentBlock>,
    /// Whether clients should render this message.
    pub display: bool,
    /// Whether the message is projected into future model context.
    pub include_in_context: bool,
    /// Extension-owned structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub details: Option<serde_json::Value>,
}

/// Extension-owned custom data appended to the current session branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntryData {
    /// Extension that owns the entry schema.
    pub extension_id: ExtensionId,
    /// Extension-defined entry subtype.
    pub custom_type: String,
    /// Structured entry body persisted without interpretation.
    pub data: serde_json::Value,
}

/// User input submitted or queued by an extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionUserMessage {
    /// Text sent through the normal input lifecycle.
    pub text: String,
    /// Queue behavior when a run is already active.
    pub delivery: crate::QueueKind,
    /// Optional correlation entry supplied by a command workflow.
    pub parent_entry_id: Option<EntryId>,
}

/// Server-local process request issued through the extension host.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ExtensionExecRequest {
    /// Executable or shell program.
    pub command: String,
    /// Ordered process arguments.
    #[builder(default)]
    pub arguments: Vec<String>,
    /// Environment additions for the child process.
    #[builder(default)]
    pub environment: BTreeMap<String, String>,
    /// Optional working-directory override.
    #[builder(default, setter(strip_option))]
    pub cwd: Option<PathBuf>,
    /// Optional execution timeout in milliseconds.
    #[builder(default, setter(strip_option))]
    pub timeout_ms: Option<u64>,
}

/// Complete output from a server-local extension process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionExecResult {
    /// Process exit code, absent when terminated by a signal.
    pub exit_code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Session-local extension event-bus message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEventData {
    /// Logical channel within the session runtime.
    pub channel: String,
    /// Extension-owned event payload.
    pub value: serde_json::Value,
}

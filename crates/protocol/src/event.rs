use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, CompactionReason, CompactionResult, ExtensionId, MessageId,
    ModelUsage, RunId, Sequence, TimestampMs, ToolCall, ToolResult, TurnId,
    TurnRecord,
};

/// Typed terminal result shared by run lifecycle events and ACP mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentOutcome {
    /// The run completed normally.
    Succeeded,
    /// The client cancelled active session work.
    Cancelled,
    /// The run stopped because an operation failed.
    Failed {
        /// Stable human-readable failure summary.
        message: String,
    },
}

/// Metadata required on every streamed runtime event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Turn that owns the event.
    pub turn_id: TurnId,

    /// Event creation time as precision-safe Unix milliseconds.
    pub timestamp_ms: TimestampMs,

    /// Monotonic sequence within the session event stream.
    pub sequence: Sequence,
}

/// Typed payload emitted by the agent runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEventPayload {
    /// A run has acquired its session and is beginning its first turn.
    RunStart {
        /// Stable run identifier.
        run_id: RunId,
    },

    /// A model turn has started.
    TurnStart {
        /// Run that owns the turn.
        run_id: RunId,
    },

    /// An assistant message is about to receive streamed content.
    MessageStart {
        /// Stable assistant message identifier.
        message_id: MessageId,
    },

    /// Incremental assistant text produced during a streamed message.
    MessageTextDelta {
        /// Message receiving the text.
        message_id: MessageId,

        /// Text fragment in provider delivery order.
        delta: String,
    },

    /// Incremental assistant reasoning produced during a streamed message.
    MessageReasoningDelta {
        /// Message receiving the reasoning text.
        message_id: MessageId,

        /// Reasoning fragment in provider delivery order.
        delta: String,
    },

    /// A complete assistant or tool-result message is available.
    MessageEnd {
        /// Complete message including exact first/last output timing.
        message: AgentMessage,
    },

    /// Complete token usage changed after an assistant attempt.
    UsageUpdated {
        /// Provider-reported token accounting for the attempt.
        usage: ModelUsage,
        /// Active model context window used to interpret the accounting.
        context_window: u64,
    },

    /// A failed assistant attempt has been scheduled for retry.
    RetryScheduled {
        /// Run that owns the retry sequence.
        run_id: RunId,
        /// One-based retry attempt that will run after the delay.
        attempt: u32,
        /// Maximum number of retry attempts permitted by policy.
        max_attempts: u32,
        /// Backoff delay before the attempt starts.
        delay_ms: u64,
        /// Failure that caused the retry decision.
        error: String,
    },

    /// A scheduled assistant retry is starting.
    RetryStart {
        /// Run that owns the retry sequence.
        run_id: RunId,
        /// One-based retry attempt now starting.
        attempt: u32,
        /// Maximum number of retry attempts permitted by policy.
        max_attempts: u32,
    },

    /// An assistant retry sequence has reached an observable result.
    RetryEnd {
        /// Run that owns the retry sequence.
        run_id: RunId,
        /// One-based retry attempt that ended.
        attempt: u32,
        /// Whether the retry produced a successful assistant result.
        success: bool,
        /// Final failure when no later retry will be scheduled.
        final_error: Option<String>,
    },

    /// One tool invocation is starting.
    ToolExecutionStart {
        /// Run that owns the invocation.
        run_id: RunId,

        /// Parsed tool call in source order.
        call: ToolCall,
    },

    /// One tool invocation published a replaceable partial result.
    ToolExecutionUpdate {
        /// Run that owns the invocation.
        run_id: RunId,

        /// Latest visible result snapshot for the correlated tool call.
        result: ToolResult,
    },

    /// One tool invocation completed, potentially before sibling calls.
    ToolExecutionEnd {
        /// Run that owns the invocation.
        run_id: RunId,

        /// Complete correlated result.
        result: ToolResult,
    },

    /// A model turn and its complete tool batch have settled.
    TurnEnd {
        /// Persistable turn result and interval.
        turn: TurnRecord,
    },

    /// A run settled after all normal, steering, and follow-up turns.
    RunEnd {
        /// Stable run identifier.
        run_id: RunId,

        /// Typed reason the run stopped.
        outcome: AgentOutcome,
    },

    /// All run work, retries, tools, and queued follow-ups have settled.
    AgentSettled {
        /// Stable run identifier.
        run_id: RunId,
        /// Typed reason the final settled work stopped.
        outcome: AgentOutcome,
    },

    /// The persisted display title for this session changed.
    SessionTitleChanged {
        /// New normalized session title.
        title: String,
    },

    /// The complete Session command snapshot changed.
    AvailableCommandsChanged {
        /// Effective commands after runtime precedence and collision handling.
        commands: Vec<crate::AvailableAgentCommand>,
    },

    /// A model-backed context compaction operation started.
    CompactionStart {
        /// Run-shaped operation identifier used for event correlation.
        run_id: RunId,
        /// Cause that initiated this compaction.
        reason: CompactionReason,
    },

    /// A model-backed context compaction operation completed.
    CompactionEnd {
        /// Run-shaped operation identifier used for event correlation.
        run_id: RunId,

        /// Cause that initiated this compaction.
        reason: CompactionReason,

        /// Persisted compaction identity and timing.
        result: CompactionResult,
    },

    /// An extension handler failed and normal lifecycle processing continued.
    ExtensionHandlerFailed {
        /// Extension whose handler returned the error.
        extension_id: ExtensionId,
        /// Stable lifecycle point name.
        point: String,
        /// Sanitized human-readable diagnostic.
        message: String,
    },

    /// A Skill resource failed during automatic user-input expansion.
    SkillDiagnostic {
        /// Structured failure without the Skill body or user prompt.
        diagnostic: crate::SkillDiagnostic,
    },
}

/// Streamed runtime event with mandatory turn, timestamp, and sequence fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Required event metadata flattened onto the wire envelope.
    #[serde(flatten)]
    pub metadata: EventMetadata,

    /// Typed event payload flattened onto the wire envelope.
    #[serde(flatten)]
    pub payload: AgentEventPayload,
}

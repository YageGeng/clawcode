//! Shared domain protocol for the agent runtime.

mod acp;
mod capability;
mod event;
mod extension;
mod id;
mod identity;
mod mcp;
mod message;
mod model;
mod queue;
mod scalar;
mod session;
mod tool;
mod turn;

pub use acp::{
    AcpCommandParameters, AcpCompactParameters, AcpForkParameters,
    AcpNavigateParameters, AcpPendingMessageRemoveParameters,
    AcpQueueMessageParameters, AcpSessionParameters,
    AcpSessionRenameParameters, AcpSkillParameters, AcpWorkingDirectory,
    AcpWorkingDirectoryError,
};
pub use capability::{
    CompactionData, CompactionDetails, CompactionPolicy, CompactionReason,
    CompactionResult, McpConnectionState, McpServerInfo, McpToolInfo,
    ModelProfile, RetryPolicy, SessionTitle, SkillInfo,
};
pub use event::{AgentEvent, AgentEventPayload, AgentOutcome, EventMetadata};
pub use extension::{
    ExtensionCommandDefinition, ExtensionContext, ExtensionDirective,
    ExtensionEffects, ExtensionEvent, ExtensionFlagDefinition,
    ExtensionFlagKind, ExtensionProviderRegistration,
};
pub use id::{IdGenerator, IdKind};
pub use identity::{AcpExtensionMethod, ProductIdentity};
pub use mcp::{McpCallResult, McpToolDescriptor};
pub use message::{
    AgentMessage, AssistantMetadata, ContentBlock, ExtensionMessage,
    MessageContent, MessageIdentity, MessageTiming, MessageTimingError,
    ModelUsage, Role,
};
pub use model::{
    ModelFailure, ModelFinal, ModelRequest, ModelRetryDisposition,
    ModelStreamEvent,
};
pub use queue::{PendingMessages, QueueKind, QueuedMessage};
pub use scalar::{
    EntryId, LaneId, MessageId, QueueId, RecordId, RunId, ScalarError,
    Sequence, SessionId, TimestampMs, ToolCallId, TurnId,
};
pub use session::{
    RunRequest, RunResult, SessionSummary, SessionTreeEntry,
    SessionTreeSnapshot,
};
pub use tool::{
    ToolCall, ToolDefinition, ToolResult, ToolResultDetails, TruncationDetails,
    TruncationLimit,
};
pub use turn::{
    StopReason, TurnIdentity, TurnOutcome, TurnRecord, TurnTiming,
    TurnTimingError,
};

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
    AcpSessionRenameParameters, AcpSkillParameters, AcpUserBashParameters,
    AcpWorkingDirectory, AcpWorkingDirectoryError,
};
pub use capability::{
    CompactionData, CompactionDetails, CompactionPolicy, CompactionReason,
    CompactionResult, McpConnectionState, McpServerInfo, McpToolInfo,
    ModelProfile, RetryPolicy, SessionTitle, SkillInfo,
};
pub use event::{AgentEvent, AgentEventPayload, AgentOutcome, EventMetadata};
pub use extension::{
    AfterProviderResponseEvent, AgentEndEvent, AgentSettledEvent,
    AgentStartEvent, BeforeAgentStartEvent, BeforeAgentStartResult,
    BeforeProviderHeadersEvent, BeforeProviderRequestEvent, ContextEvent,
    ContextResult, ExtensionCommandDefinition, ExtensionDescriptor,
    ExtensionEntryData, ExtensionEventData, ExtensionExecRequest,
    ExtensionExecResult, ExtensionFlagDefinition, ExtensionFlagKind,
    ExtensionInvocation, ExtensionMessageDraft, ExtensionProviderRegistration,
    ExtensionSnapshot, ExtensionUserMessage, HeaderMutation, HeaderPatch,
    InputEvent, InputResult, InputSource, InputStreamingBehavior,
    MessageEndEvent, MessageStartEvent, MessageUpdate, MessageUpdateEvent,
    ModelSelectEvent, ModelSelectSource, ProjectTrustDecision,
    ProjectTrustEvent, ProjectTrustResult, ResourcesDiscoverEvent,
    ResourcesDiscoverReason, ResourcesDiscoverResult,
    SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCancelResult,
    SessionCompactEvent, SessionForkPosition, SessionInfoChangedEvent,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent,
    SessionStartReason, SessionSwitchReason, SessionTreeEvent,
    StaticExtensionRegistration, ThinkingLevel, ThinkingLevelSelectEvent,
    ToolBlock, ToolCallEvent, ToolCallResult, ToolExecutionEndEvent,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultEvent,
    ToolResultPatch, TreeSummary, TurnEndEvent, TurnStartEvent,
    UserBashDisposition, UserBashEvent, UserBashResult,
};
pub use id::{IdGenerator, IdKind};
pub use identity::{
    AcpExtensionMethod, ConfigOverridePath, ConfigOverridePathError,
    ProductIdentity,
};
pub use mcp::{McpCallResult, McpToolDescriptor};
pub use message::{
    AgentMessage, AssistantMetadata, BashExecutionMessage, ContentBlock,
    ExtensionMessage, MessageContent, MessageIdentity, MessageTiming,
    MessageTimingError, ModelUsage, Role,
};
pub use model::{
    ModelFailure, ModelFinal, ModelRequest, ModelRetryDisposition,
    ModelStreamEvent,
};
pub use queue::{PendingMessages, QueueKind, QueuedMessage};
pub use scalar::{
    EntryId, ExtensionId, LaneId, MessageId, QueueId, RecordId, RunId,
    ScalarError, Sequence, SessionId, TimestampMs, ToolCallId, TurnId,
};
pub use session::{
    RunRequest, RunResult, SessionSummary, SessionTreeEntry,
    SessionTreeSnapshot, UserBashInput, UserBashRequest,
};
pub use tool::{
    ToolCall, ToolDefinition, ToolResult, ToolResultDetails, TruncationDetails,
    TruncationLimit,
};
pub use turn::{
    StopReason, TurnIdentity, TurnOutcome, TurnRecord, TurnTiming,
    TurnTimingError,
};

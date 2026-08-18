//! Shared domain protocol for the agent runtime.

mod acp;
mod capability;
mod event;
mod extension;
pub mod hooks;
mod id;
mod identity;
mod mcp;
mod message;
mod model;
mod prompt;
mod queue;
mod scalar;
mod session;
mod skill;
mod slash_command;
mod tool;
mod turn;

pub use acp::{
    AcpCommandParameters, AcpCompactParameters, AcpForkParameters,
    AcpNavigateParameters, AcpPendingMessageRemoveParameters,
    AcpSessionParameters, AcpSessionRenameParameters, AcpSkillParameters,
    AcpUserBashParameters, AcpWorkingDirectory, AcpWorkingDirectoryError,
};
pub use capability::{
    CompactionData, CompactionDetails, CompactionOperationError,
    CompactionOperationFinished, CompactionOperationIntent,
    CompactionOperationKind, CompactionOperationOutcome,
    CompactionOperationStarted, CompactionOutcome, CompactionPolicy,
    CompactionReason, CompactionResult, CompactionStep, CompactionStepAttempt,
    CompactionUsageCause, CompactionUsageRecord, ModelInputModalities,
    ModelInputModalitiesError, ModelInputModality, ModelProfile, RetryPolicy,
    SessionTitle,
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
pub use hooks::model::{
    ModelHeaders, ModelHookError, ModelRequestHooks, ModelResponseMetadata,
};
pub use id::{IdGenerator, IdKind};
pub use identity::{
    AcpExtensionMethod, ConfigOverridePath, ConfigOverridePathError,
    ProductIdentity,
};
pub use mcp::{
    McpArgumentInfo, McpAuthorizationContinueRequest, McpAuthorizationRequest,
    McpAuthorizationResult, McpCapabilityCounts, McpCatalog,
    McpCatalogRevisions, McpCompletionRequest, McpCompletionResult,
    McpCompletionTarget, McpElicitationAction, McpElicitationMode,
    McpElicitationRequest, McpElicitationResponseRequest, McpElicitationResult,
    McpElicitationSnapshot, McpFailureStage, McpHostRequest, McpHostResponse,
    McpIdentityError, McpOAuthState, McpOAuthStatus, McpPromptInfo,
    McpPromptMessage, McpPromptRef, McpPromptRequest, McpPromptResult,
    McpProtocolVersion, McpProtocolVersionError, McpReconnectRequest,
    McpRequestContext, McpResourceInfo, McpResourceRef, McpResourceRequest,
    McpResourceResult, McpResourceTemplateInfo, McpRoot, McpRootsRequest,
    McpRootsResult, McpSamplingRequest, McpSamplingResult,
    McpServerCapabilities, McpServerFailure, McpServerId,
    McpServerImplementation, McpServerState, McpServerStatus, McpSessionChange,
    McpSessionCompletionRequest, McpSessionPromptRequest,
    McpSessionResourceRequest, McpSessionRevisionNotification,
    McpSessionSnapshot, McpTaskState, McpTaskStatus, McpToolInfo, McpToolRef,
    McpToolRequest, McpToolResult, McpTransportKind,
};
pub use message::{
    AgentMessage, AssistantMetadata, BashExecutionMessage,
    CompactionSummaryMessage, ContentBlock, EmbeddedResourceContent,
    ExtensionMessage, MessageContent, MessageIdentity, MessageTiming,
    MessageTimingError, ModelUsage, Role,
};
pub use model::{
    ModelFailure, ModelFinal, ModelRequest, ModelRequestOptions,
    ModelRetryDisposition, ModelStreamEvent,
};
pub use prompt::{
    ProjectInstruction, PromptCollision, PromptContentSource, PromptDiagnostic,
    PromptDiagnosticSeverity, PromptPolicy, PromptResourceRequest,
    PromptSourceInfo, PromptSourceKind, PromptSourceScope, PromptTemplateInfo,
    SkillResourceRequest, SystemPromptBuildOptions, SystemPromptTool,
    ToolPromptContribution,
};
pub use queue::{PendingMessages, QueueKind, QueuedMessage};
pub use scalar::{
    EntryId, ExtensionId, LaneId, MessageId, QueueId, RecordId, RunId,
    ScalarError, Sequence, SessionId, TimestampMs, ToolCallId, TraceId, TurnId,
};
pub use session::{
    RunInput, RunRequest, RunResult, SessionReplayItem, SessionSummary,
    SessionTreeEntry, SessionTreeSnapshot, UserBashInput, UserBashRequest,
};
pub use skill::{
    SkillCollision, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SkillDiscoveryMode, SkillInfo, SkillListResult,
    SkillSelectionRule, SkillSource, SkillSourceKind, SkillSourceScope,
};
pub use slash_command::{
    SlashCommandAliasKind, SlashCommandDefinition, SlashCommandError,
    SlashCommandExpansion, SlashCommandInvocation, SlashCommandMessage,
    SlashCommandOutcome, SlashCommandOutput, SlashCommandParseError,
    SlashCommandSource, SlashCommandStatus,
};
pub use tool::{
    ToolCall, ToolDefinition, ToolResult, ToolResultDetails, TruncationDetails,
    TruncationLimit,
};
pub use turn::{
    StopReason, TurnIdentity, TurnOutcome, TurnRecord, TurnTiming,
    TurnTimingError,
};

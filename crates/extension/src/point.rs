use protocol::*;

/// Associates one lifecycle marker with its exact event and output types.
pub trait ExtensionPoint: Send + Sync + 'static {
    /// Event payload visible to handlers registered for this point.
    type Event: Send + Sync;
    /// Result composed according to this point's domain semantics.
    type Output: Send;
    /// Stable diagnostic name for this point.
    const NAME: &'static str;
}

macro_rules! define_point {
    ($name:ident, $event:ty, $output:ty, $wire_name:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name;

        impl ExtensionPoint for $name {
            type Event = $event;
            type Output = $output;
            const NAME: &'static str = $wire_name;
        }
    };
}

define_point!(
    ProjectTrustPoint,
    ProjectTrustEvent,
    ProjectTrustResult,
    "project_trust",
    "Project trust decision point."
);
define_point!(
    ResourcesDiscoverPoint,
    ResourcesDiscoverEvent,
    ResourcesDiscoverResult,
    "resources_discover",
    "Server-side extension resource discovery point."
);
define_point!(
    SessionStartPoint,
    SessionStartEvent,
    (),
    "session_start",
    "Session runtime start observer point."
);
define_point!(
    SessionInfoChangedPoint,
    SessionInfoChangedEvent,
    (),
    "session_info_changed",
    "Session metadata observer point."
);
define_point!(
    SessionBeforeSwitchPoint,
    SessionBeforeSwitchEvent,
    SessionCancelResult,
    "session_before_switch",
    "Cancellable session switch point."
);
define_point!(
    SessionBeforeForkPoint,
    SessionBeforeForkEvent,
    SessionBeforeForkResult,
    "session_before_fork",
    "Cancellable session fork point."
);
define_point!(
    SessionBeforeCompactPoint,
    SessionBeforeCompactEvent,
    SessionBeforeCompactResult,
    "session_before_compact",
    "Cancellable and replaceable compaction point."
);
define_point!(
    SessionCompactPoint,
    SessionCompactEvent,
    (),
    "session_compact",
    "Completed compaction observer point."
);
define_point!(
    SessionBeforeTreePoint,
    SessionBeforeTreeEvent,
    SessionBeforeTreeResult,
    "session_before_tree",
    "Cancellable and summarizable tree-navigation point."
);
define_point!(
    SessionTreePoint,
    SessionTreeEvent,
    (),
    "session_tree",
    "Completed tree-navigation observer point."
);
define_point!(
    SessionShutdownPoint,
    SessionShutdownEvent,
    (),
    "session_shutdown",
    "Session runtime shutdown observer point."
);
define_point!(
    InputPoint,
    InputEvent,
    InputResult,
    "input",
    "User-input preprocessing point."
);
define_point!(
    BeforeAgentStartPoint,
    BeforeAgentStartEvent,
    BeforeAgentStartResult,
    "before_agent_start",
    "Agent system-prompt and message injection point."
);
define_point!(
    AgentStartPoint,
    AgentStartEvent,
    (),
    "agent_start",
    "Agent start observer point."
);
define_point!(
    AgentEndPoint,
    AgentEndEvent,
    (),
    "agent_end",
    "Agent end observer point."
);
define_point!(
    AgentSettledPoint,
    AgentSettledEvent,
    (),
    "agent_settled",
    "Fully settled agent observer point."
);
define_point!(
    TurnStartPoint,
    TurnStartEvent,
    (),
    "turn_start",
    "Model turn start observer point."
);
define_point!(
    TurnEndPoint,
    TurnEndEvent,
    (),
    "turn_end",
    "Model turn end observer point."
);
define_point!(
    MessageStartPoint,
    MessageStartEvent,
    (),
    "message_start",
    "Message start observer point."
);
define_point!(
    MessageUpdatePoint,
    MessageUpdateEvent,
    (),
    "message_update",
    "Streaming message update observer point."
);
define_point!(
    MessageEndPoint,
    MessageEndEvent,
    Option<AgentMessage>,
    "message_end",
    "Final message replacement point."
);
define_point!(
    ContextPoint,
    ContextEvent,
    ContextResult,
    "context",
    "Model-context transformation point."
);
define_point!(
    BeforeProviderRequestPoint,
    BeforeProviderRequestEvent,
    serde_json::Value,
    "before_provider_request",
    "Provider payload replacement point."
);
define_point!(
    BeforeProviderHeadersPoint,
    BeforeProviderHeadersEvent,
    HeaderPatch,
    "before_provider_headers",
    "Provider header mutation point."
);
define_point!(
    AfterProviderResponsePoint,
    AfterProviderResponseEvent,
    (),
    "after_provider_response",
    "Provider response metadata observer point."
);
define_point!(
    ModelSelectPoint,
    ModelSelectEvent,
    (),
    "model_select",
    "Active model observer point."
);
define_point!(
    ThinkingLevelSelectPoint,
    ThinkingLevelSelectEvent,
    (),
    "thinking_level_select",
    "Thinking-level observer point."
);
define_point!(
    ToolExecutionStartPoint,
    ToolExecutionStartEvent,
    (),
    "tool_execution_start",
    "Tool execution start observer point."
);
define_point!(
    ToolExecutionUpdatePoint,
    ToolExecutionUpdateEvent,
    (),
    "tool_execution_update",
    "Tool execution update observer point."
);
define_point!(
    ToolExecutionEndPoint,
    ToolExecutionEndEvent,
    (),
    "tool_execution_end",
    "Tool execution end observer point."
);
define_point!(
    ToolCallPoint,
    ToolCallEvent,
    ToolCallResult,
    "tool_call",
    "Tool-call transformation and blocking point."
);
define_point!(
    ToolResultPoint,
    ToolResultEvent,
    ToolResultPatch,
    "tool_result",
    "Tool-result transformation point."
);
define_point!(
    UserBashPoint,
    UserBashEvent,
    Option<UserBashResult>,
    "user_bash",
    "Server-side user shell interception point."
);

/// Ordered catalog of Pi's current non-UI extension hook names.
pub const NON_UI_EXTENSION_POINT_NAMES: [&str; 33] = [
    ProjectTrustPoint::NAME,
    ResourcesDiscoverPoint::NAME,
    SessionStartPoint::NAME,
    SessionInfoChangedPoint::NAME,
    SessionBeforeSwitchPoint::NAME,
    SessionBeforeForkPoint::NAME,
    SessionBeforeCompactPoint::NAME,
    SessionCompactPoint::NAME,
    SessionBeforeTreePoint::NAME,
    SessionTreePoint::NAME,
    SessionShutdownPoint::NAME,
    InputPoint::NAME,
    BeforeAgentStartPoint::NAME,
    AgentStartPoint::NAME,
    AgentEndPoint::NAME,
    AgentSettledPoint::NAME,
    TurnStartPoint::NAME,
    TurnEndPoint::NAME,
    MessageStartPoint::NAME,
    MessageUpdatePoint::NAME,
    MessageEndPoint::NAME,
    ContextPoint::NAME,
    BeforeProviderRequestPoint::NAME,
    BeforeProviderHeadersPoint::NAME,
    AfterProviderResponsePoint::NAME,
    ModelSelectPoint::NAME,
    ThinkingLevelSelectPoint::NAME,
    ToolExecutionStartPoint::NAME,
    ToolExecutionUpdatePoint::NAME,
    ToolExecutionEndPoint::NAME,
    ToolCallPoint::NAME,
    ToolResultPoint::NAME,
    UserBashPoint::NAME,
];

//! Public types for the typed, non-UI extension lifecycle.

mod context;
mod events;
mod registration;
mod result;

pub use context::{
    ExtensionEntryData, ExtensionEventData, ExtensionExecRequest,
    ExtensionExecResult, ExtensionInvocation, ExtensionMessageDraft,
    ExtensionSnapshot, ExtensionUserMessage,
};
pub use events::*;
pub use registration::{
    ExtensionCommandDefinition, ExtensionDescriptor, ExtensionFlagDefinition,
    ExtensionFlagKind, ExtensionProviderRegistration,
    StaticExtensionRegistration,
};
pub use result::{
    BeforeAgentStartResult, ContextResult, HeaderMutation, HeaderPatch,
    InputResult, ProjectTrustDecision, ProjectTrustResult,
    ResourcesDiscoverResult, SessionBeforeCompactResult,
    SessionBeforeForkResult, SessionBeforeTreeResult, SessionCancelResult,
    ToolBlock, ToolCallResult, ToolResultPatch, TreeSummary,
    UserBashDisposition, UserBashResult,
};

//! The rig-core items other crates may use, re-exported one by one.
//!
//! rig's module layout is kept so names cannot clash with protocol's own types
//! (`message::StopReason` versus `protocol::StopReason`). Adding an item here is
//! the explicit review point for widening rig's reach; the pre-commit boundary
//! check rejects direct rig dependencies outside protocol, provider and extension.

/// Message content used to replay and project native assistant turns.
pub mod message {
    pub use rig_core::message::{
        AssistantContent, AssistantMessage, CallId, DocumentSourceKind, Image,
        ImageMediaType, Message, MimeType, Reasoning, StopReason, ToolCall,
        ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
    };
}

/// Request and terminal types of one completion.
pub mod completion {
    pub use rig_core::completion::{
        CompletionRequest, FinishReason, ToolDefinition,
    };
}

/// Events read from an upstream completion stream.
pub mod streaming {
    pub use rig_core::streaming::{Item, StreamEvent};
}

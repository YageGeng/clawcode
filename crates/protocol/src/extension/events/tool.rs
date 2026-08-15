use serde::{Deserialize, Serialize};

use crate::{ToolCall, ToolResult};

/// A parsed tool call is about to start execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionStartEvent {
    /// Parsed call and current arguments.
    pub call: ToolCall,
}

/// A running tool emitted a replaceable partial result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionUpdateEvent {
    /// Parsed call being executed.
    pub call: ToolCall,
    /// Latest complete visible result snapshot.
    pub partial_result: ToolResult,
}

/// A tool finished execution after result transformation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionEndEvent {
    /// Parsed call that was executed or blocked.
    pub call: ToolCall,
    /// Complete final result.
    pub result: ToolResult,
}

/// A parsed tool call may be transformed or blocked before validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEvent {
    /// Current parsed call observed by this handler.
    pub call: ToolCall,
}

/// A completed tool result may be transformed before persistence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultEvent {
    /// Final transformed call that produced this result.
    pub call: ToolCall,
    /// Current result observed by this handler.
    pub result: ToolResult,
}

/// A user-authored shell command is about to execute on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserBashEvent {
    /// Shell command supplied by the user.
    pub command: String,
    /// Whether output is excluded from future model context.
    pub exclude_from_context: bool,
    /// Server-side working directory.
    pub cwd: std::path::PathBuf,
}

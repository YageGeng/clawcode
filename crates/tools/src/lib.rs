//! Agent tool contracts, registry, factories, and built-in tools.

mod bash;
mod builtin;
mod contract;
mod output;
mod truncation;

pub use bash::{
    BashExecutionOutput, BashExecutionRequest, BashExecutor,
    BashOutputSnapshot, BashTermination,
};
pub use builtin::BuiltinToolFactory;
pub use contract::{
    AgentTool, DiscardToolUpdates, ToolError, ToolExecutionContext,
    ToolFactory, ToolRegistry, ToolUpdateSink,
};
pub use output::{TOOL_TEXT_BYTES, ToolContentText, ToolResultProjection};
pub use truncation::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head,
    truncate_tail,
};

//! Agent tool contracts, registry, factories, and built-in tools.

mod builtin;
mod contract;
mod truncation;

pub use builtin::BuiltinToolFactory;
pub use contract::{
    AgentTool, DiscardToolUpdates, ToolError, ToolExecutionContext,
    ToolFactory, ToolRegistry, ToolUpdateSink,
};
pub use truncation::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head,
    truncate_tail,
};

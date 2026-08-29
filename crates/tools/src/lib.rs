//! Agent tool contracts, registry, factories, and built-in tools.

mod bash;
mod builtin;
mod contract;
mod terminal;
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
pub use terminal::{
    DEFAULT_MAX_OUTPUT_TOKENS, DiscardTerminalOutput, TERMINAL_OUTPUT_CAPACITY,
    TerminalError, TerminalInteractionOutput, TerminalInteractionRequest,
    TerminalInteractionState, TerminalManager, TerminalOutputSink,
    TerminalService, TerminalSpawnRequest, UnavailableTerminalService,
};
pub use truncation::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head,
    truncate_tail,
};

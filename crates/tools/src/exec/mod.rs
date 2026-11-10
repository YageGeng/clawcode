//! Unified exec: shell commands that may outlive the call that started them.
//!
//! Ported from codex `unified_exec`. A command runs until it exits or its
//! yield time passes; a command still running is kept in a per-Session process
//! table and the model continues it with `write_stdin`.

mod env;
mod manager;
mod output;
mod process;
mod request;
mod response;
mod sessions;
mod shell;
mod snapshot;

use std::path::PathBuf;

use async_trait::async_trait;
use protocol::{ExecExit, ToolCallId, TurnId};

pub use env::ExecEnvironment;
pub use manager::{ExecProcessManager, ExecStart, ExecWrite, prune_candidate};
pub use output::{Captured, HeadTailBuffer, OutputBuffers};
pub use process::{ExecProcess, ProcessState, SpawnSpec};
pub use request::{ExecCommandArgs, WriteStdinArgs};
pub use response::ExecResponse;
pub use sessions::{CommandSessionState, CommandSessions};
pub use shell::{ShellKind, UserShell};
pub use snapshot::ShellSnapshot;

use crate::ToolError;

/// Processes kept per Session before the least recently used is evicted.
pub const MAX_PROCESSES: usize = 64;

/// Configuration a Session's process manager runs with.
#[derive(Debug, Clone)]
pub struct ExecSettings {
    /// User-facing `[tools.exec]` settings.
    pub config: config::ExecConfig,
    /// Directory holding per-Session shell snapshots.
    pub snapshot_dir: PathBuf,
}

/// One replaceable snapshot of a process's visible transcript.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ExecOutputUpdate {
    /// `exec_command` call that started the process.
    pub tool_call_id: ToolCallId,
    /// Turn that owns the starting call.
    pub turn_id: TurnId,
    /// Session ID of the process.
    pub process_id: i32,
    /// Whole visible transcript tail.
    pub output: String,
}

/// Receives process lifecycle reports that happen outside any tool call.
#[async_trait]
pub trait ExecEventSink: Send + Sync {
    /// A process's visible transcript changed.
    async fn output(&self, update: ExecOutputUpdate);
    /// A process ended after its first `exec_command` returned without its exit.
    async fn exited(&self, exit: ExecExit);
}

/// Failures of exec operations, worded as in codex.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    /// The command could not be started.
    #[error("Failed to create unified exec process: {0}")]
    CreateProcess(String),
    /// The process or its I/O failed after starting.
    #[error("Unified exec process failed: {0}")]
    ProcessFailed(String),
    /// No live or uncollected process has this session ID.
    #[error("Unknown process id {0}")]
    UnknownProcessId(i32),
    /// Bytes could not be delivered to the process.
    #[error("failed to write to stdin")]
    WriteToStdin,
    /// Only PTY sessions accept input.
    #[error(
        "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
    )]
    StdinClosed,
    /// The requested working directory does not exist.
    #[error("Working directory does not exist: {}", .0.display())]
    InvalidWorkdir(PathBuf),
    /// The owning turn was cancelled while the call waited.
    #[error("Operation aborted")]
    Cancelled,
}

impl ExecError {
    /// Converts into the tool failure of the named tool, keeping cancellation distinct.
    #[must_use]
    pub fn into_tool_error(self, tool: &str) -> ToolError {
        match self {
            Self::Cancelled => ToolError::Cancelled,
            error => {
                tracing::warn!("{} failed: {}", tool, error);
                ToolError::Execution {
                    tool: tool.to_string(),
                    message: error.to_string(),
                }
            }
        }
    }
}

/// Shortest wait before an `exec_command` or a write yields.
pub const MIN_YIELD_TIME_MS: u64 = 250;
/// Longest wait before an `exec_command` or a non-empty write yields.
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
/// Shortest wait for an empty `write_stdin` poll.
pub const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
/// Default wait for `exec_command`.
pub const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
/// Default wait after a non-empty write.
pub const DEFAULT_WRITE_YIELD_TIME_MS: u64 = 250;
/// Windows shells start slowly, so the first wait is never shorter than this.
pub const WINDOWS_EXEC_YIELD_FLOOR_MS: u64 = 10_000;
/// Default output budget of one call.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
/// Bytes retained per output buffer of one process.
pub const OUTPUT_MAX_BYTES: usize = 1024 * 1024;
/// Largest output budget a call may request.
pub const MAX_OUTPUT_TOKENS: usize = OUTPUT_MAX_BYTES / APPROX_BYTES_PER_TOKEN;
/// Bytes counted as one token by every estimate in this module.
pub const APPROX_BYTES_PER_TOKEN: usize = 4;

mod backend;
mod buffer;
mod manager;
mod process;

pub use manager::TerminalManager;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use protocol::{SessionId, TerminalId, TurnId};
use tokio_util::sync::CancellationToken;

/// Maximum raw output retained for one terminal process.
pub const TERMINAL_OUTPUT_CAPACITY: usize = 1_024 * 1_024;
/// Default and maximum model-visible output budget for one interaction.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
/// Codex-compatible approximate UTF-8 byte count per output token.
pub const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Receives replaceable output snapshots for one active terminal interaction.
pub trait TerminalOutputSink: Send + Sync {
    /// Publishes the latest bounded output without consuming the Agent cursor.
    fn publish(&self, output: String);
}

/// Default observer used when a terminal caller does not consume live output.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardTerminalOutput;

impl TerminalOutputSink for DiscardTerminalOutput {
    /// Intentionally ignores a live snapshot while final output remains available.
    fn publish(&self, _output: String) {}
}

/// Complete request for creating one Session-owned terminal process.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct TerminalSpawnRequest {
    /// Complete shell source.
    pub cmd: String,
    /// Existing server-side working directory.
    pub cwd: PathBuf,
    /// Session that owns the process.
    pub session_id: SessionId,
    /// Turn that created the process.
    pub turn_id: TurnId,
    /// Whether to allocate a pseudo-terminal.
    pub tty: bool,
    /// Maximum initial wait before returning a running identifier.
    pub yield_duration: Duration,
    /// Maximum approximate tokens returned by this interaction.
    pub max_output_tokens: usize,
    /// Cancellation for the current ToolCall wait only.
    pub cancellation: CancellationToken,
    /// Destination for replaceable output snapshots during the initial wait.
    #[builder(
        default = Arc::new(DiscardTerminalOutput) as Arc<dyn TerminalOutputSink>
    )]
    pub output_updates: Arc<dyn TerminalOutputSink>,
}

/// Complete request for writing or polling one retained process.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct TerminalInteractionRequest {
    /// Session-local terminal identifier.
    pub terminal_id: TerminalId,
    /// Characters to write, or no characters for a pure poll.
    #[builder(default)]
    pub chars: Option<String>,
    /// Maximum wait for output or process exit.
    pub yield_duration: Duration,
    /// Maximum approximate tokens returned by this interaction.
    pub max_output_tokens: usize,
    /// Cancellation for the current ToolCall wait only.
    pub cancellation: CancellationToken,
    /// Destination for replaceable output snapshots during this interaction.
    #[builder(
        default = Arc::new(DiscardTerminalOutput) as Arc<dyn TerminalOutputSink>
    )]
    pub output_updates: Arc<dyn TerminalOutputSink>,
}

/// Process state returned after one bounded interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInteractionState {
    /// The process remains available through the returned identifier.
    Running {
        /// Session-local process identifier.
        session_id: TerminalId,
    },
    /// The process exited and has been reaped after this response.
    Exited {
        /// Numeric exit status when supplied by the platform.
        exit_code: Option<i32>,
    },
    /// The backend failed after retaining any output produced before failure.
    Failed {
        /// Sanitized backend diagnostic suitable for the model tool result.
        message: String,
    },
}

/// Model-facing data produced by one bounded terminal interaction.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct TerminalInteractionOutput {
    /// New output visible to the Agent in this interaction.
    pub output: String,
    /// Process state at the response boundary.
    pub state: TerminalInteractionState,
    /// Wall-clock duration spent in this interaction.
    pub wall_time: Duration,
}

impl TerminalInteractionOutput {
    /// Returns the retained terminal identifier when the process is still running.
    #[must_use]
    pub const fn running_id(&self) -> Option<TerminalId> {
        match self.state {
            TerminalInteractionState::Running { session_id } => {
                Some(session_id)
            }
            TerminalInteractionState::Exited { .. } => None,
            TerminalInteractionState::Failed { .. } => None,
        }
    }
}

/// Failures produced by terminal validation, state access, and process backends.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalError {
    /// No Session terminal service was supplied to this direct tool caller.
    #[error("terminal service is unavailable")]
    Unavailable,
    /// The owning Session is permanently closing and rejects new processes.
    #[error("terminal manager is shutting down")]
    ShuttingDown,
    /// The requested working directory was not an existing directory.
    #[error("invalid terminal working directory: {path}")]
    InvalidWorkingDirectory {
        /// Rejected server-side path.
        path: PathBuf,
    },
    /// A shell or process could not be created.
    #[error("failed to spawn terminal process: {message}")]
    Spawn {
        /// Sanitized backend diagnostic.
        message: String,
    },
    /// The identifier was not retained by this Session manager.
    #[error("unknown terminal session: {terminal_id}")]
    UnknownTerminal {
        /// Missing Session-local identifier.
        terminal_id: TerminalId,
    },
    /// Ordinary input was sent to a pipe process.
    #[error(
        "terminal {terminal_id} was started without a PTY and does not accept stdin"
    )]
    InputUnsupported {
        /// Pipe terminal that rejected input.
        terminal_id: TerminalId,
    },
    /// A running backend failed after process creation.
    #[error("terminal backend failed: {message}")]
    Backend {
        /// Sanitized backend diagnostic.
        message: String,
    },
    /// A synchronous state lock was poisoned by a panic.
    #[error("terminal state lock poisoned")]
    StatePoisoned,
    /// The current interaction wait was cancelled.
    #[error("terminal interaction cancelled")]
    Cancelled,
}

/// Session-scoped capability consumed by stateless Agent tool adapters.
#[async_trait]
pub trait TerminalService: Send + Sync {
    /// Starts one Session-owned process and performs its initial bounded wait.
    async fn spawn(
        &self,
        request: TerminalSpawnRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError>;

    /// Writes or polls one existing Session-owned process.
    async fn interact(
        &self,
        request: TerminalInteractionRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError>;
}

/// Default service used by direct callers that do not execute shell tools.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTerminalService;

#[async_trait]
impl TerminalService for UnavailableTerminalService {
    /// Rejects process creation because no Session manager was injected.
    async fn spawn(
        &self,
        _request: TerminalSpawnRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        Err(TerminalError::Unavailable)
    }

    /// Rejects process interaction because no Session manager was injected.
    async fn interact(
        &self,
        _request: TerminalInteractionRequest,
    ) -> Result<TerminalInteractionOutput, TerminalError> {
        Err(TerminalError::Unavailable)
    }
}

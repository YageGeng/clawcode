use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{SessionId, TerminalId, TimestampMs};

/// Public lifecycle state for one Session-owned terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    /// The process is still available for interaction.
    Running,
    /// The process exited and may retain unread output.
    Exited,
    /// The process backend failed and may retain a diagnostic.
    Failed,
}

/// Reason one terminal was removed from its Session manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRemovalReason {
    /// A host explicitly terminated one terminal.
    Terminated,
    /// A host explicitly cleaned every terminal.
    Cleaned,
    /// Capacity pressure evicted one terminal.
    Capacity,
    /// The owning Session closed.
    SessionClosed,
    /// The Kernel shut down.
    KernelShutdown,
    /// A consumer collected a completed terminal.
    Reaped,
}

/// Host-facing snapshot of one retained terminal.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    /// Session-local numeric identifier used by host management APIs.
    pub terminal_id: TerminalId,
    /// Complete shell command supplied at creation.
    pub command: String,
    /// Server-side working directory used by the process.
    pub cwd: PathBuf,
    /// Whether the process owns a pseudo-terminal.
    pub tty: bool,
    /// Latest lifecycle state.
    pub status: TerminalStatus,
    /// Numeric exit code when the platform supplied one.
    #[serde(default)]
    #[builder(default)]
    pub exit_code: Option<i32>,
    /// Process creation time.
    pub started_at: TimestampMs,
    /// Latest input, output, or state-change time.
    pub last_activity_at: TimestampMs,
}

/// Product-scoped notification that invalidates one terminal snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalUpdateNotification {
    /// Session that owns the terminal manager.
    pub session_id: SessionId,
    /// Latest manager revision known when the notification was emitted.
    pub revision: u64,
}

/// Complete terminal snapshot returned by the ACP list method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalListResult {
    /// Monotonic manager revision represented by this snapshot.
    pub revision: u64,
    /// Retained terminals in stable identifier order.
    pub terminals: Vec<TerminalSnapshot>,
}

/// Idempotent result returned by the ACP terminate method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalTerminateResult {
    /// Whether the requested terminal existed and was terminated.
    pub terminated: bool,
}

/// Result returned after cleaning one Session terminal manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCleanResult {
    /// Number of retained terminals removed by the operation.
    pub cleaned: usize,
}

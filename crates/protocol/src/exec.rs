//! Unified exec tool names and the process lifecycle values shared by the
//! Kernel, its persisted entries, and protocol adapters.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ToolCallId, TurnId};

/// Name of the tool that starts a shell command and may leave it running.
pub const EXEC_COMMAND_TOOL: &str = "exec_command";

/// Name of the tool that polls, writes to, or interrupts a running command.
pub const WRITE_STDIN_TOOL: &str = "write_stdin";

/// Line of an exec result's model text after which the command output starts;
/// clients cut the text there to show the output in a terminal.
pub const EXEC_OUTPUT_HEADER: &str = "Output:";

/// Start of the footer naming the file that keeps a truncated result's output.
pub const EXEC_FULL_OUTPUT_LABEL: &str = "Full output: ";

/// How one background command session ended, as reported after the call that
/// started it already returned a session ID.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ExecExit {
    /// `exec_command` call that started the process; also its terminal id.
    pub tool_call_id: ToolCallId,
    /// Turn that owns the starting call.
    pub turn_id: TurnId,
    /// Session ID the model used for `write_stdin`.
    pub process_id: i32,
    /// Exit code; absent when a signal ended the process or it was lost.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Name of the signal that ended the process, such as `SIGKILL`.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Final visible tail of the process transcript.
    pub output: String,
    /// Retained file holding the complete transcript after truncation.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Time from process start to exit.
    pub wall_time_ms: u64,
    /// The process stopped being tracked without an observed exit, such as
    /// after a server restart or in a forked Session.
    #[builder(default)]
    #[serde(default)]
    pub lost: bool,
}

/// One running command session listed for users and protocol clients.
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
pub struct ExecProcessInfo {
    /// Session ID the model uses for `write_stdin`.
    pub process_id: i32,
    /// Command text as the model wrote it.
    pub command: String,
    /// Directory the command runs in.
    pub cwd: PathBuf,
    /// `exec_command` call that started the process.
    pub tool_call_id: ToolCallId,
}

//! Arguments and model-facing definitions of `exec_command` and `write_stdin`.

use std::path::PathBuf;
use std::time::Duration;

use protocol::{EXEC_COMMAND_TOOL, ToolCall, ToolDefinition, WRITE_STDIN_TOOL};
use serde::Deserialize;

use super::{
    DEFAULT_EXEC_YIELD_TIME_MS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_WRITE_YIELD_TIME_MS, MAX_OUTPUT_TOKENS, MAX_YIELD_TIME_MS,
    MIN_EMPTY_YIELD_TIME_MS, MIN_YIELD_TIME_MS,
};
use crate::ToolError;

/// Validated `exec_command` arguments.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecCommandArgs {
    /// Shell command to execute.
    pub cmd: String,
    /// Directory relative to the turn cwd, or absolute.
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// Whether the command gets a PTY and a writable stdin.
    #[serde(default)]
    pub tty: bool,
    /// Requested wait before yielding.
    #[serde(default)]
    pub yield_time_ms: Option<u64>,
    /// Requested output budget.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    /// Shell binary overriding the user's default shell.
    #[serde(default)]
    pub shell: Option<PathBuf>,
    /// Whether the shell runs as a login shell.
    #[serde(default = "login_default")]
    pub login: bool,
}

/// Serde default for `login`, which codex enables unless asked otherwise.
const fn login_default() -> bool {
    true
}

impl TryFrom<&ToolCall> for ExecCommandArgs {
    type Error = ToolError;

    /// Decodes one call without consuming it.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl ExecCommandArgs {
    /// Clamps the requested wait to codex's window, with a floor on Windows.
    #[must_use]
    pub fn yield_time(&self) -> Duration {
        let requested =
            self.yield_time_ms.unwrap_or(DEFAULT_EXEC_YIELD_TIME_MS);
        let floored = if cfg!(windows) {
            requested.max(super::WINDOWS_EXEC_YIELD_FLOOR_MS)
        } else {
            requested
        };
        Duration::from_millis(
            floored.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS),
        )
    }

    /// Returns the output budget, capped at what one buffer can hold.
    #[must_use]
    pub fn max_output_tokens(&self) -> usize {
        self.max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
            .min(MAX_OUTPUT_TOKENS)
    }

    /// Returns the model-facing schema, worded as in codex.
    #[must_use]
    pub fn definition() -> ToolDefinition {
        let yield_description = if cfg!(windows) {
            "Maximum time to wait before returning a session ID for a still-running command. Commands that finish sooner return immediately. For ordinary commands, omit this parameter to use the 60000 ms default. Effective range on Windows is 10000-60000 ms."
        } else {
            "Wait before yielding output. Defaults to 60000 ms; effective range is 250-60000 ms. A command that outlives this wait keeps running as a background terminal."
        };
        ToolDefinition {
            name: EXEC_COMMAND_TOOL.to_string(),
            description: "Runs a command in a PTY, returning output or a session ID for ongoing interaction.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "Shell command to execute." },
                    "workdir": { "type": "string", "description": "Working directory for the command. Defaults to the turn cwd." },
                    "tty": { "type": "boolean", "description": "True allocates a PTY for the command; false or omitted uses plain pipes." },
                    "yield_time_ms": { "type": "number", "description": yield_description },
                    "max_output_tokens": { "type": "number", "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy." },
                    "shell": { "type": "string", "description": "Shell binary to launch. Defaults to the user's default shell." },
                    "login": { "type": "boolean", "description": "True runs the shell with -l/-i semantics; false disables them. Defaults to true." }
                },
                "required": ["cmd"],
                "additionalProperties": false
            }),
        }
    }
}

/// Validated `write_stdin` arguments.
#[derive(Debug, Clone, Deserialize)]
pub struct WriteStdinArgs {
    /// Session ID returned by `exec_command`.
    pub session_id: i32,
    /// Bytes to write; empty polls without writing.
    #[serde(default)]
    pub chars: String,
    /// Requested wait before yielding.
    #[serde(default)]
    pub yield_time_ms: Option<u64>,
    /// Requested output budget.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

impl TryFrom<&ToolCall> for WriteStdinArgs {
    type Error = ToolError;

    /// Decodes one call without consuming it.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl WriteStdinArgs {
    /// Polls wait between five seconds and the configured background limit;
    /// writes default to 250 ms so interactive input stays responsive.
    #[must_use]
    pub fn yield_time(&self, max_background: Duration) -> Duration {
        let requested = self
            .yield_time_ms
            .unwrap_or(DEFAULT_WRITE_YIELD_TIME_MS)
            .max(MIN_YIELD_TIME_MS);
        let millis = if self.chars.is_empty() {
            let max = u64::try_from(max_background.as_millis())
                .unwrap_or(u64::MAX)
                .max(MIN_EMPTY_YIELD_TIME_MS);
            requested.clamp(MIN_EMPTY_YIELD_TIME_MS, max)
        } else {
            requested.min(MAX_YIELD_TIME_MS)
        };
        Duration::from_millis(millis)
    }

    /// Returns the output budget, capped at what one buffer can hold.
    #[must_use]
    pub fn max_output_tokens(&self) -> usize {
        self.max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
            .min(MAX_OUTPUT_TOKENS)
    }

    /// Returns the model-facing schema, worded as in codex.
    #[must_use]
    pub fn definition() -> ToolDefinition {
        ToolDefinition {
            name: WRITE_STDIN_TOOL.to_string(),
            description: "Writes characters to an existing unified exec session and returns recent output.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "number", "description": "Identifier of the running unified exec session." },
                    "chars": { "type": "string", "description": "Bytes to write to stdin. Defaults to empty, which polls without writing." },
                    "yield_time_ms": { "type": "number", "description": "Wait before yielding output. Non-empty writes default to 250 ms and cap at 60000 ms; empty polls wait 5000-300000 ms by default." },
                    "max_output_tokens": { "type": "number", "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy." }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        }
    }
}

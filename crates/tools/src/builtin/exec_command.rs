use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use protocol::{
    ContentBlock, ToolCall, ToolDefinition, ToolPromptContribution, ToolResult,
};
use serde::Deserialize;

use crate::{
    AgentTool, DEFAULT_MAX_OUTPUT_TOKENS, TerminalError,
    TerminalInteractionOutput, TerminalInteractionState, TerminalSpawnRequest,
    ToolError, ToolExecutionContext,
};

use super::terminal_updates::TerminalToolUpdates;

const DEFAULT_YIELD_MS: u64 = 10_000;
const MIN_YIELD_MS: u64 = 250;
const MAX_YIELD_MS: u64 = 30_000;

/// Model arguments for creating one pipe or PTY shell process.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecCommandArguments {
    cmd: String,
    workdir: Option<PathBuf>,
    #[serde(flatten)]
    controls: ExecCommandControls,
}

/// Optional execution controls decoded alongside the required command fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecCommandControls {
    tty: Option<bool>,
    #[serde(rename = "yield\x2dtime_ms")]
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

impl TryFrom<&ToolCall> for ExecCommandArguments {
    type Error = ToolError;

    /// Decodes one exec_command call without consuming its correlation value.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl ExecCommandArguments {
    /// Validates bounded wait and output values before process creation.
    fn validate(&self) -> Result<(), ToolError> {
        if self.cmd.is_empty() {
            return Err(Self::invalid("cmd must not be empty"));
        }
        let yield_time_ms =
            self.controls.yield_time_ms.unwrap_or(DEFAULT_YIELD_MS);
        if !(MIN_YIELD_MS..=MAX_YIELD_MS).contains(&yield_time_ms) {
            return Err(Self::invalid(
                "yield-time_ms must be between 250 and 30000",
            ));
        }
        let max_output_tokens = self
            .controls
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        if !(1..=DEFAULT_MAX_OUTPUT_TOKENS).contains(&max_output_tokens) {
            return Err(Self::invalid(
                "max_output_tokens must be between 1 and 10000",
            ));
        }
        Ok(())
    }

    /// Creates one consistently attributed invalid-argument error.
    fn invalid(message: &str) -> ToolError {
        ToolError::InvalidArguments {
            tool: "exec_command".to_string(),
            message: message.to_string(),
        }
    }
}

/// Codex-style shell creation tool with optional native PTY allocation.
pub(super) struct ExecCommandTool;

#[async_trait]
impl AgentTool for ExecCommandTool {
    /// Returns the bounded model-facing process creation schema.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exec_command".to_string(),
            description: "Run a shell command in a pipe or native PTY. Commands still running after the bounded wait return a session_id for write_stdin.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": {
                        "type": "string",
                        "description": "Complete shell command source"
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Optional absolute or turn-relative working directory"
                    },
                    "tty": {
                        "type": "boolean",
                        "description": "Allocate a native pseudo-terminal",
                        "default": false
                    },
                    "yield-time_ms": {
                        "type": "integer",
                        "minimum": MIN_YIELD_MS,
                        "maximum": MAX_YIELD_MS,
                        "default": DEFAULT_YIELD_MS
                    },
                    "max_output_tokens": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": DEFAULT_MAX_OUTPUT_TOKENS,
                        "default": DEFAULT_MAX_OUTPUT_TOKENS
                    }
                },
                "required": ["cmd"],
                "additionalProperties": false
            }),
        }
    }

    /// Contributes interactive terminal guidance to the dynamic System Prompt.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some("Run shell commands with optional PTY support".to_string()),
            guidelines: vec![
                "Use write_stdin with the returned session_id to poll or interact with background commands.".to_string(),
            ],
        }
    }

    /// Validates process arguments without starting an operating-system process.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        ExecCommandArguments::try_from(call)?.validate()
    }

    /// Starts one Session-owned process and formats a stable JSON text response.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let arguments = ExecCommandArguments::try_from(&call)?;
        arguments.validate()?;
        // Match Codex's public `workdir` name while projecting it into the
        // terminal layer's platform-neutral cwd field.
        let cwd = match arguments.workdir {
            Some(workdir) if workdir.is_absolute() => workdir,
            Some(workdir) => context.cwd.join(workdir),
            None => context.cwd.clone(),
        };
        let output = context
            .terminals
            .spawn(
                TerminalSpawnRequest::builder()
                    .cmd(arguments.cmd)
                    .cwd(cwd)
                    .session_id(context.session_id.clone())
                    .turn_id(context.turn_id.clone())
                    .tty(arguments.controls.tty.unwrap_or(false))
                    .yield_duration(Duration::from_millis(
                        arguments
                            .controls
                            .yield_time_ms
                            .unwrap_or(DEFAULT_YIELD_MS),
                    ))
                    .max_output_tokens(
                        arguments
                            .controls
                            .max_output_tokens
                            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
                    )
                    .cancellation(context.cancellation.clone())
                    .output_updates(Arc::new(TerminalToolUpdates::new(
                        call.tool_call_id.clone(),
                        Arc::clone(&context.updates),
                    )))
                    .build(),
            )
            .await
            .map_err(|error| Self::tool_error(&call.name, error))?;
        Ok(Self::result(call, output))
    }
}

impl ExecCommandTool {
    /// Maps one terminal-layer failure into the shared model tool error contract.
    fn tool_error(tool: &str, error: TerminalError) -> ToolError {
        if error == TerminalError::Cancelled {
            ToolError::Cancelled
        } else {
            ToolError::Execution {
                tool: tool.to_string(),
                message: error.to_string(),
            }
        }
    }

    /// Formats one terminal interaction as a single JSON text content block.
    fn result(call: ToolCall, output: TerminalInteractionOutput) -> ToolResult {
        let wall_time_seconds = output.wall_time.as_secs_f64();
        let (response, is_error) = match output.state {
            TerminalInteractionState::Running { session_id } => (
                serde_json::json!({
                    "output": output.output,
                    "session_id": session_id.get(),
                    "wall_time_seconds": wall_time_seconds
                }),
                false,
            ),
            TerminalInteractionState::Exited { exit_code } => (
                serde_json::json!({
                    "output": output.output,
                    "exit_code": exit_code,
                    "wall_time_seconds": wall_time_seconds
                }),
                false,
            ),
            TerminalInteractionState::Failed { message } => (
                serde_json::json!({
                    "output": output.output,
                    "error": message,
                    "wall_time_seconds": wall_time_seconds
                }),
                true,
            ),
        };
        ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: response.to_string(),
            }])
            .is_error(is_error)
            .build()
    }
}

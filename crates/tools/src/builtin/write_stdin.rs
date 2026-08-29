use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use protocol::{
    ContentBlock, TerminalId, ToolCall, ToolDefinition, ToolResult,
};

use crate::{
    AgentTool, DEFAULT_MAX_OUTPUT_TOKENS, TerminalError,
    TerminalInteractionOutput, TerminalInteractionRequest,
    TerminalInteractionState, ToolError, ToolExecutionContext,
};

use super::terminal_updates::TerminalToolUpdates;

const DEFAULT_WRITE_YIELD_MS: u64 = 250;
const DEFAULT_POLL_YIELD_MS: u64 = 5_000;
const MIN_YIELD_MS: u64 = 250;
const MAX_WRITE_YIELD_MS: u64 = 30_000;
const MAX_POLL_YIELD_MS: u64 = 300_000;

/// Model arguments for writing to or polling one retained process.
#[derive(Debug)]
struct WriteStdinArguments {
    session_id: u32,
    chars: Option<String>,
    controls: WriteStdinControls,
}

/// Optional wait and output controls decoded with one terminal interaction.
#[derive(Debug)]
struct WriteStdinControls {
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

impl TryFrom<&ToolCall> for WriteStdinArguments {
    type Error = ToolError;

    /// Decodes one write_stdin call without consuming its correlation value.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        let decode = || -> Result<Self, String> {
            let object = call
                .arguments
                .as_object()
                .ok_or_else(|| "arguments must be an object".to_string())?;
            if let Some(unknown) = object.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "session_id"
                        | "chars"
                        | "yield\x2dtime_ms"
                        | "max_output_tokens"
                )
            }) {
                return Err(format!("unknown field `{unknown}`"));
            }
            let session_id = object
                .get("session_id")
                .cloned()
                .ok_or_else(|| "missing field `session_id`".to_string())
                .and_then(|value| {
                    serde_json::from_value(value)
                        .map_err(|error| error.to_string())
                })?;
            let chars = object
                .get("chars")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| error.to_string())?;
            let yield_time_ms = object
                .get("yield\x2dtime_ms")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| error.to_string())?;
            let max_output_tokens = object
                .get("max_output_tokens")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok(Self {
                session_id,
                chars,
                controls: WriteStdinControls {
                    yield_time_ms,
                    max_output_tokens,
                },
            })
        };
        decode().map_err(|message| ToolError::InvalidArguments {
            tool: call.name.clone(),
            message,
        })
    }
}

impl WriteStdinArguments {
    /// Validates the terminal identifier and bounded response controls.
    fn validate(&self) -> Result<TerminalId, ToolError> {
        let terminal_id = TerminalId::try_from(self.session_id)
            .map_err(|error| Self::invalid(&error.to_string()))?;
        let maximum_yield_ms =
            if self.chars.as_deref().is_none_or(str::is_empty) {
                MAX_POLL_YIELD_MS
            } else {
                MAX_WRITE_YIELD_MS
            };
        if let Some(yield_time_ms) = self.controls.yield_time_ms
            && !(MIN_YIELD_MS..=maximum_yield_ms).contains(&yield_time_ms)
        {
            return Err(Self::invalid(&format!(
                "yield-time_ms must be between {MIN_YIELD_MS} and {maximum_yield_ms}"
            )));
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
        Ok(terminal_id)
    }

    /// Returns the write-sensitive default and enforces a useful poll minimum.
    fn yield_duration(&self) -> Duration {
        let is_poll = self.chars.as_deref().is_none_or(str::is_empty);
        let default = if is_poll {
            DEFAULT_POLL_YIELD_MS
        } else {
            DEFAULT_WRITE_YIELD_MS
        };
        let requested = self.controls.yield_time_ms.unwrap_or(default);
        Duration::from_millis(if is_poll {
            requested.max(DEFAULT_POLL_YIELD_MS)
        } else {
            requested
        })
    }

    /// Creates one consistently attributed invalid-argument error.
    fn invalid(message: &str) -> ToolError {
        ToolError::InvalidArguments {
            tool: "write_stdin".to_string(),
            message: message.to_string(),
        }
    }
}

/// Codex-style tool for PTY input, Ctrl-C, and non-consuming status waits.
pub(super) struct WriteStdinTool;

#[async_trait]
impl AgentTool for WriteStdinTool {
    /// Returns the bounded model-facing process interaction schema.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_stdin".to_string(),
            description: "Write characters to a retained PTY, send Ctrl-C, or poll a background command. Empty polls wait at least 5000ms.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "integer",
                        "minimum": TerminalId::MIN,
                        "maximum": TerminalId::MAX
                    },
                    "chars": {
                        "type": "string",
                        "description": "Characters to write; omit or use an empty string to poll"
                    },
                    "yield-time_ms": {
                        "type": "integer",
                        "minimum": MIN_YIELD_MS,
                        "maximum": MAX_POLL_YIELD_MS,
                        "description": "Maximum wait; writes allow up to 30000ms and empty polls allow up to 300000ms"
                    },
                    "max_output_tokens": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": DEFAULT_MAX_OUTPUT_TOKENS,
                        "default": DEFAULT_MAX_OUTPUT_TOKENS
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        }
    }

    /// Validates interaction arguments without touching retained process state.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        WriteStdinArguments::try_from(call)?
            .validate()
            .map(|_id| ())
    }

    /// Writes or polls one Session-owned process and returns stable JSON text.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let arguments = WriteStdinArguments::try_from(&call)?;
        let terminal_id = arguments.validate()?;
        let yield_duration = arguments.yield_duration();
        let output = context
            .terminals
            .interact(
                TerminalInteractionRequest::builder()
                    .terminal_id(terminal_id)
                    .chars(arguments.chars)
                    .yield_duration(yield_duration)
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

impl WriteStdinTool {
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

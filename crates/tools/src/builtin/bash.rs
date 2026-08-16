use std::time::Duration;

use async_trait::async_trait;
use protocol::{
    ContentBlock, ToolCall, ToolCallId, ToolDefinition, ToolPromptContribution,
    ToolResult, ToolResultDetails, TruncationLimit,
};
use serde::Deserialize;

use crate::{
    AgentTool, BashExecutionRequest, BashExecutor, BashOutputSnapshot,
    BashTermination, DEFAULT_MAX_BYTES, ToolError, ToolExecutionContext,
    format_size,
};

const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1_000.0;

#[derive(Debug, Deserialize)]
struct BashArguments {
    command: String,
    timeout: Option<f64>,
}

impl TryFrom<&ToolCall> for BashArguments {
    type Error = ToolError;

    /// Decodes one bash call without consuming the correlated protocol value.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl BashArguments {
    /// Converts an optional finite positive second value into a runtime duration.
    fn timeout_duration(&self) -> Result<Option<Duration>, ToolError> {
        let Some(timeout) = self.timeout else {
            return Ok(None);
        };
        if !timeout.is_finite() || timeout <= 0.0 {
            return Err(ToolError::Execution {
                tool: "bash".to_string(),
                message: "Invalid timeout: must be a finite number of seconds"
                    .to_string(),
            });
        }
        if timeout > MAX_TIMEOUT_SECONDS {
            return Err(ToolError::Execution {
                tool: "bash".to_string(),
                message: format!(
                    "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
                ),
            });
        }
        Ok(Some(Duration::from_secs_f64(timeout)))
    }

    /// Formats the original timeout number using JSON/JavaScript-compatible concise text.
    fn timeout_label(&self) -> Option<String> {
        self.timeout.map(|timeout| timeout.to_string())
    }
}

/// Pi-compatible local bash tool with streaming, cancellation, and bounded output.
pub(super) struct BashTool;

#[async_trait]
impl AgentTool for BashTool {
    /// Returns pi's bash schema and output limits.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Timeout in seconds (optional, no default timeout)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    /// Contributes pi's shell capability without unsupported PI environment guidance.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some(
                "Execute bash commands (ls, grep, find, etc.)".to_string(),
            ),
            guidelines: Vec::new(),
        }
    }

    /// Validates bash JSON and timeout bounds without starting a process.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        BashArguments::try_from(call)?.timeout_duration()?;
        Ok(())
    }

    /// Executes one command and streams replaceable tail snapshots until settlement.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments = BashArguments::try_from(&call)?;
        let timeout = arguments.timeout_duration()?;
        context.publish(
            ToolResult::builder()
                .tool_call_id(call.tool_call_id.clone())
                .blocks(Vec::new())
                .is_error(false)
                .build(),
        );
        let request = BashExecutionRequest::builder()
            .command(arguments.command.clone())
            .cwd(context.cwd.clone())
            .session_id(context.session_id.clone())
            .turn_id(context.turn_id.clone())
            .cancellation(context.cancellation.clone());
        let request = match timeout {
            Some(timeout) => request.timeout(timeout).build(),
            None => request.build(),
        };
        let output = BashExecutor::execute(request, |snapshot| {
            Self::publish_snapshot(&call.tool_call_id, context, snapshot);
        })
        .await?;
        let (visible, details) =
            Self::format_output(output.snapshot, "(no output)");
        match output.termination {
            BashTermination::Exited
                if output.result.exit_code.is_some_and(|code| code != 0) =>
            {
                let code = output.result.exit_code.unwrap_or_default();
                Err(ToolError::Execution {
                    tool: call.name,
                    message: Self::append_status(
                        &visible,
                        &format!("Command exited with code {code}"),
                    ),
                })
            }
            BashTermination::Aborted => Err(ToolError::Execution {
                tool: call.name,
                message: Self::append_status(&visible, "Command aborted"),
            }),
            BashTermination::TimedOut => Err(ToolError::Execution {
                tool: call.name,
                message: Self::append_status(
                    &visible,
                    &format!(
                        "Command timed out after {} seconds",
                        arguments.timeout_label().unwrap_or_default()
                    ),
                ),
            }),
            BashTermination::Exited => {
                let builder = ToolResult::builder()
                    .tool_call_id(call.tool_call_id)
                    .blocks(vec![ContentBlock::Text { text: visible }])
                    .is_error(false);
                Ok(match details {
                    Some(details) => builder.details(details).build(),
                    None => builder.build(),
                })
            }
        }
    }
}

impl BashTool {
    /// Publishes one replaceable partial snapshot without marking it final.
    fn publish_snapshot(
        tool_call_id: &ToolCallId,
        context: &ToolExecutionContext,
        snapshot: BashOutputSnapshot,
    ) {
        let details = (snapshot.truncation.truncated
            || snapshot.full_output_path.is_some())
        .then(|| ToolResultDetails::Bash {
            truncation: snapshot
                .truncation
                .truncated
                .then_some(snapshot.truncation.clone()),
            full_output_path: snapshot
                .full_output_path
                .map(|path| path.to_string_lossy().into_owned()),
        });
        let builder = ToolResult::builder()
            .tool_call_id(tool_call_id.clone())
            .blocks(vec![ContentBlock::Text {
                text: snapshot.output,
            }])
            .is_error(false);
        context.publish(match details {
            Some(details) => builder.details(details).build(),
            None => builder.build(),
        });
    }

    /// Adds pi's truncation footer and returns typed bash details when needed.
    fn format_output(
        snapshot: BashOutputSnapshot,
        empty_text: &str,
    ) -> (String, Option<ToolResultDetails>) {
        let truncation = snapshot.truncation;
        let mut text = if snapshot.output.is_empty() {
            empty_text.to_string()
        } else {
            snapshot.output
        };
        if !truncation.truncated {
            return (text, None);
        }
        let full_output_path = snapshot
            .full_output_path
            .map(|path| path.to_string_lossy().into_owned());
        let path = full_output_path.as_deref().unwrap_or_default();
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        let footer = if truncation.last_line_partial {
            format!(
                "[Showing last {} of line {end_line} (line is {}). Full output: {path}]",
                format_size(truncation.output_bytes),
                format_size(snapshot.last_line_bytes)
            )
        } else if truncation.truncated_by == Some(TruncationLimit::Lines) {
            format!(
                "[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
                truncation.total_lines
            )
        } else {
            format!(
                "[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES)
            )
        };
        text.push_str("\n\n");
        text.push_str(&footer);
        (
            text,
            Some(ToolResultDetails::Bash {
                truncation: Some(truncation),
                full_output_path,
            }),
        )
    }

    /// Appends a terminal status after any already captured output.
    fn append_status(output: &str, status: &str) -> String {
        if output.is_empty() {
            status.to_string()
        } else {
            format!("{output}\n\n{status}")
        }
    }
}

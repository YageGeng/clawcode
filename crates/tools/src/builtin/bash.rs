use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use protocol::{
    ContentBlock, ProductIdentity, ToolCall, ToolCallId, ToolDefinition,
    ToolResult, ToolResultDetails, TruncationLimit,
};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::output::{OutputAccumulator, OutputSnapshot};
use crate::{
    AgentTool, DEFAULT_MAX_BYTES, ToolError, ToolExecutionContext, format_size,
};

const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1_000.0;
const UPDATE_THROTTLE: Duration = Duration::from_millis(100);
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Deserialize)]
struct BashArguments {
    command: String,
    timeout: Option<f64>,
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

/// Terminal condition observed while collecting one command's output.
enum CommandTermination {
    Exited(Option<i32>),
    Aborted,
    TimedOut(String),
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

    /// Executes one command and streams replaceable tail snapshots until settlement.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments = serde_json::from_value::<BashArguments>(call.arguments)
            .map_err(|error| ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            })?;
        let timeout = arguments.timeout_duration()?;
        if context.cancellation.is_cancelled() {
            return Err(ToolError::Execution {
                tool: call.name,
                message: "Command aborted".to_string(),
            });
        }
        if !tokio::fs::try_exists(&context.cwd).await.map_err(|error| {
            ToolError::Execution {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })? {
            return Err(ToolError::Execution {
                tool: call.name,
                message: format!(
                    "Working directory does not exist: {}\nCannot execute bash commands.",
                    context.cwd.display()
                ),
            });
        }

        context.publish(
            ToolResult::builder()
                .tool_call_id(call.tool_call_id.clone())
                .blocks(Vec::new())
                .is_error(false)
                .build(),
        );
        let mut command = Command::new(Self::shell_path());
        command
            .arg("-c")
            .arg(&arguments.command)
            .current_dir(&context.cwd)
            .env(ProductIdentity::SESSION_ID_ENV, context.session_id.as_str())
            .env(ProductIdentity::TURN_ID_ENV, context.turn_id.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            std::os::unix::process::CommandExt::process_group(
                command.as_std_mut(),
                0,
            );
        }
        let mut child =
            command.spawn().map_err(|error| ToolError::Execution {
                tool: call.name.clone(),
                message: error.to_string(),
            })?;
        let child_pid = child.id();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut readers = tokio::task::JoinSet::new();
        if let Some(stdout) = child.stdout.take() {
            Self::spawn_reader(&mut readers, stdout, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            Self::spawn_reader(&mut readers, stderr, sender.clone());
        }
        drop(sender);

        let mut output = OutputAccumulator::new();
        let mut last_update = Instant::now()
            .checked_sub(UPDATE_THROTTLE)
            .unwrap_or_else(Instant::now);
        let mut wait = Box::pin(child.wait());
        let mut timeout_wait = Box::pin(async move {
            match timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending::<()>().await,
            }
        });
        let termination = loop {
            tokio::select! {
                () = context.cancellation.cancelled() => {
                    Self::kill_process_tree(child_pid).await;
                    break CommandTermination::Aborted;
                }
                () = &mut timeout_wait => {
                    Self::kill_process_tree(child_pid).await;
                    break CommandTermination::TimedOut(
                        arguments.timeout_label().unwrap_or_default(),
                    );
                }
                status = &mut wait => {
                    let status = status.map_err(|error| ToolError::Execution {
                        tool: call.name.clone(),
                        message: error.to_string(),
                    })?;
                    break CommandTermination::Exited(status.code());
                }
                chunk = receiver.recv() => {
                    let Some(chunk) = chunk else {
                        continue;
                    };
                    output.append(chunk).await.map_err(|error| ToolError::Execution {
                        tool: call.name.clone(),
                        message: error.to_string(),
                    })?;
                    if last_update.elapsed() >= UPDATE_THROTTLE {
                        Self::publish_snapshot(
                            &call.tool_call_id,
                            context,
                            output.snapshot(),
                        );
                        last_update = Instant::now();
                    }
                }
            }
        };
        if !matches!(termination, CommandTermination::Exited(_)) {
            let _status = wait.await;
        }
        // Reader tasks drain normal pipe buffers independently from the more
        // expensive output accumulator. The grace applies only when a
        // descendant inherited a pipe and keeps EOF from arriving.
        let reader_result = tokio::time::timeout(EXIT_STDIO_GRACE, async {
            while readers.join_next().await.is_some() {}
        })
        .await;
        if reader_result.is_err() {
            readers.abort_all();
            while readers.join_next().await.is_some() {}
        }
        while let Some(chunk) = receiver.recv().await {
            output.append(chunk).await.map_err(|error| {
                ToolError::Execution {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
        }
        let snapshot =
            output
                .finish()
                .await
                .map_err(|error| ToolError::Execution {
                    tool: call.name.clone(),
                    message: error.to_string(),
                })?;
        Self::publish_snapshot(
            &call.tool_call_id,
            context,
            OutputSnapshot {
                content: snapshot.content.clone(),
                truncation: snapshot.truncation.clone(),
                full_output_path: snapshot.full_output_path.clone(),
            },
        );
        let (visible, details) =
            Self::format_output(&output, snapshot, "(no output)");
        match termination {
            CommandTermination::Exited(Some(code)) if code != 0 => {
                Err(ToolError::Execution {
                    tool: call.name,
                    message: Self::append_status(
                        &visible,
                        &format!("Command exited with code {code}"),
                    ),
                })
            }
            CommandTermination::Aborted => Err(ToolError::Execution {
                tool: call.name,
                message: Self::append_status(&visible, "Command aborted"),
            }),
            CommandTermination::TimedOut(timeout) => {
                Err(ToolError::Execution {
                    tool: call.name,
                    message: Self::append_status(
                        &visible,
                        &format!("Command timed out after {timeout} seconds"),
                    ),
                })
            }
            CommandTermination::Exited(Some(_))
            | CommandTermination::Exited(None) => {
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
    /// Resolves bash using pi's Unix preference order with a portable sh fallback.
    fn shell_path() -> &'static Path {
        if Path::new("/bin/bash").exists() {
            Path::new("/bin/bash")
        } else {
            Path::new("sh")
        }
    }

    /// Reads one stdout or stderr pipe into the shared arrival-order channel.
    fn spawn_reader(
        readers: &mut tokio::task::JoinSet<()>,
        mut reader: impl AsyncRead + Unpin + Send + 'static,
        sender: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    ) {
        readers.spawn(async move {
            let mut buffer = vec![0_u8; 8 * 1_024];
            loop {
                let read = match reader.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let Some(chunk) = buffer.get(..read) else {
                    break;
                };
                if sender.send(chunk.to_vec()).is_err() {
                    break;
                }
            }
        });
    }

    /// Terminates the command process group so descendants cannot outlive cancellation.
    async fn kill_process_tree(pid: Option<u32>) {
        let Some(pid) = pid else {
            return;
        };
        #[cfg(unix)]
        {
            if let Ok(pid) = i32::try_from(pid) {
                let _result = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        #[cfg(windows)]
        {
            let _result = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    /// Publishes one replaceable partial snapshot without marking it final.
    fn publish_snapshot(
        tool_call_id: &ToolCallId,
        context: &ToolExecutionContext,
        snapshot: OutputSnapshot,
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
                text: snapshot.content,
            }])
            .is_error(false);
        context.publish(match details {
            Some(details) => builder.details(details).build(),
            None => builder.build(),
        });
    }

    /// Adds pi's truncation footer and returns typed bash details when needed.
    fn format_output(
        output: &OutputAccumulator,
        snapshot: OutputSnapshot,
        empty_text: &str,
    ) -> (String, Option<ToolResultDetails>) {
        let truncation = snapshot.truncation;
        let mut text = if snapshot.content.is_empty() {
            empty_text.to_string()
        } else {
            snapshot.content
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
                format_size(output.last_line_bytes())
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

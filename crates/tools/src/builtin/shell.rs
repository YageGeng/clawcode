//! Shell command execution tool.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::Stream;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::Tool;

const OUTPUT_MAX_LEN: usize = 4096;
const SHELL_TIMEOUT_SECS: u64 = 30;

/// Executes arbitrary shell commands.
pub struct ShellCommand;

impl ShellCommand {
    /// Create a new shell tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ShellCommand {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellCommand {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout, stderr, and exit code"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the command"
                }
            },
            "required": ["command"]
        })
    }

    fn capability(&self) -> protocol::ToolCapability {
        protocol::ToolCapability {
            supports_streaming: true,
        }
    }

    fn needs_approval(&self, _: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let command = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("missing 'command' argument")?
            .to_string();

        let work_dir = arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());

        let result = timeout(
            Duration::from_secs(SHELL_TIMEOUT_SECS),
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut formatted = format!(
                    "exit code: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status.code().unwrap_or(-1),
                    truncate(&stdout),
                    truncate(&stderr),
                );
                if formatted.len() > OUTPUT_MAX_LEN {
                    formatted.truncate(OUTPUT_MAX_LEN);
                    formatted.push_str("\n... (output truncated)");
                }
                Ok(formatted)
            }
            Ok(Err(e)) => Err(format!("command execution failed: {e}")),
            Err(_) => Err(format!("command timed out after {SHELL_TIMEOUT_SECS}s")),
        }
    }

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<
        (
            String,
            Pin<Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>>,
        ),
        String,
    > {
        let command_str = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("missing 'command' argument")?
            .to_string();
        let work_dir = arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());

        let command = vec!["/bin/sh".to_string(), "-c".to_string(), command_str.clone()];

        let (delta_tx, delta_rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();

        let begin = protocol::ToolStreamItem::Begin(protocol::TurnItem::ExecCommand(
            protocol::ExecCommandItem::builder()
                .id(String::new())
                .command(command.clone())
                .cwd(work_dir.clone())
                .status(protocol::ExecCommandStatus::InProgress)
                .build(),
        ));

        let command_for_task = command_str.clone();
        let work_dir_for_task = work_dir.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            let result = run_with_streaming(&command_for_task, &work_dir_for_task, delta_tx).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let _ = result_tx.send((result, duration_ms));
        });

        let delta_stream = UnboundedReceiverStream::new(delta_rx);
        let stream = futures::stream::once(async { begin }).chain(delta_stream);

        let (exec_result, duration_ms) = result_rx
            .await
            .map_err(|_e| "internal error: shell task dropped".to_string())?;

        let (model_text, end_item) = match exec_result {
            Ok(result) => build_shell_result(&command, &work_dir, result, duration_ms),
            Err(e) => {
                let err_msg = format!("command execution failed: {e}");
                let end = protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(
                    protocol::ExecCommandItem::builder()
                        .id(String::new())
                        .command(command.clone())
                        .cwd(work_dir.clone())
                        .status(protocol::ExecCommandStatus::Failed)
                        .stderr(err_msg.clone())
                        .exit_code(-1)
                        .duration_ms(duration_ms)
                        .build(),
                ));
                (err_msg, end)
            }
        };

        let stream = stream.chain(futures::stream::once(async { end_item }));
        Ok((model_text, Box::pin(stream)))
    }
}

/// Result of a completed shell command execution.
struct ExecResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
}

/// Spawn a child process and stream stdout/stderr chunks via `delta_tx`.
async fn run_with_streaming(
    command: &str,
    cwd: &Path,
    delta_tx: mpsc::UnboundedSender<protocol::ToolStreamItem>,
) -> std::io::Result<ExecResult> {
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout pipe configured");
    let stderr = child.stderr.take().expect("stderr pipe configured");

    let stdout_tx = delta_tx.clone();
    let stdout_handle = tokio::spawn(read_and_emit(
        stdout,
        protocol::ExecOutputStream::Stdout,
        stdout_tx,
    ));
    let stderr_handle = tokio::spawn(read_and_emit(
        stderr,
        protocol::ExecOutputStream::Stderr,
        delta_tx,
    ));

    let status = child.wait().await?;
    let stdout = stdout_handle.await??;
    let stderr = stderr_handle.await??;

    Ok(ExecResult {
        stdout,
        stderr,
        exit_code: status.code().unwrap_or(-1),
    })
}

/// Read from an async pipe and emit byte chunks as `Delta` stream items.
async fn read_and_emit<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    stream_type: protocol::ExecOutputStream,
    tx: mpsc::UnboundedSender<protocol::ToolStreamItem>,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        let chunk = &tmp.get(..n).expect("read byte count within buffer bounds");
        let _ = tx.send(protocol::ToolStreamItem::Delta {
            stream: stream_type,
            chunk: chunk.to_vec(),
        });
        buf.extend_from_slice(chunk);
    }
    Ok(buf)
}

/// Build the model-facing text and `End` lifecycle item for a completed shell command.
fn build_shell_result(
    command: &[String],
    cwd: &Path,
    result: ExecResult,
    duration_ms: u64,
) -> (String, protocol::ToolStreamItem) {
    let stdout_str = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&result.stderr).to_string();

    let status = if result.exit_code == 0 {
        protocol::ExecCommandStatus::Completed
    } else {
        protocol::ExecCommandStatus::Failed
    };

    let model_text = format!(
        "exit code: {}\nstdout:\n{}\nstderr:\n{}",
        result.exit_code,
        truncate(&stdout_str),
        truncate(&stderr_str),
    );

    let end_item = protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(
        protocol::ExecCommandItem::builder()
            .id(String::new())
            .command(command.to_vec())
            .cwd(cwd.to_path_buf())
            .status(status)
            .stdout(stdout_str)
            .stderr(stderr_str)
            .exit_code(result.exit_code)
            .duration_ms(duration_ms)
            .build(),
    ));

    (model_text, end_item)
}

/// Truncate command output to the per-stream display budget.
///
/// Uses [`str::floor_char_boundary`] to avoid panicking when the byte limit
/// falls in the middle of a multi-byte UTF-8 character.
#[allow(clippy::string_slice)]
fn truncate(s: &str) -> &str {
    if s.len() > OUTPUT_MAX_LEN / 2 {
        let boundary = s.floor_char_boundary(OUTPUT_MAX_LEN / 2);
        &s[..boundary]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    #[tokio::test]
    async fn shell_echo_hello() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({"command": "echo hello"}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello"));
        assert!(output.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn shell_missing_command() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await;
        result.unwrap_err();
    }

    #[tokio::test]
    async fn shell_needs_approval() {
        let tool = ShellCommand::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"command": "ls"}),
            &ToolContext::for_test(Path::new("."))
        ));
    }

    #[test]
    fn truncate_utf8_boundary_does_not_panic() {
        // 2047 ASCII bytes + CJK chars: byte 2048 lands mid-char.
        let s = format!("{}{}", "a".repeat(2047), "你好世界");
        let result = truncate(&s);
        assert!(
            result.len() <= OUTPUT_MAX_LEN / 2,
            "len {} > budget {}",
            result.len(),
            OUTPUT_MAX_LEN / 2
        );
        assert!(
            s.is_char_boundary(result.len()) || result == s,
            "slice ends at non-char-boundary"
        );
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn truncate_exact_boundary_ascii() {
        let s = "a".repeat(OUTPUT_MAX_LEN);
        let result = truncate(&s);
        assert_eq!(result.len(), OUTPUT_MAX_LEN / 2);
    }

    #[tokio::test]
    async fn shell_streaming_produces_delta_events() {
        use futures::stream::StreamExt;

        let tool = ShellCommand::new();
        let (_text, mut stream) = tool
            .execute_streaming(
                serde_json::json!({"command": "echo hello && sleep 0.1 && echo world"}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await
            .expect("execute_streaming");

        let mut has_begin = false;
        let mut has_end = false;
        let mut delta_count = 0;
        while let Some(item) = stream.next().await {
            match item {
                protocol::ToolStreamItem::Begin(_) => has_begin = true,
                protocol::ToolStreamItem::End(_) => has_end = true,
                protocol::ToolStreamItem::Delta { .. } => delta_count += 1,
                _ => {}
            }
        }
        assert!(has_begin, "should have a Begin event");
        assert!(has_end, "should have an End event");
        assert!(delta_count > 0, "should have at least one Delta event");
    }

    #[tokio::test]
    async fn shell_streaming_failed_command() {
        use futures::stream::StreamExt;

        let tool = ShellCommand::new();
        let (_text, mut stream) = tool
            .execute_streaming(
                serde_json::json!({"command": "exit 1"}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await
            .expect("execute_streaming");

        let mut end_status = None;
        while let Some(item) = stream.next().await {
            if let protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(item)) = item {
                end_status = Some(item.status);
            }
        }
        assert_eq!(
            end_status,
            Some(protocol::ExecCommandStatus::Failed),
            "non-zero exit should produce Failed status"
        );
    }
}

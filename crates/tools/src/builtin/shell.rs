//! Shell command execution tool.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::Stream;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{
    LocalTerminalBackend, RunningTerminal, TerminalBackend, TerminalCreateParams,
    TerminalEnvVariable, TerminalOutputSnapshot, Tool, ToolContext,
};

const OUTPUT_MAX_LEN: usize = 4096;
const POLL_INTERVAL_MS: u64 = 100;

/// Executes arbitrary shell commands via a [`TerminalBackend`].
pub struct ShellCommand {
    backend: Arc<dyn TerminalBackend>,
}

impl ShellCommand {
    /// Create a shell tool with the default local backend.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalTerminalBackend::new()))
    }

    /// Create a shell tool with a custom terminal backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
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
                    "type": ["string", "null"],
                    "description": "Optional working directory for the command"
                },
                "env": {
                    "type": ["array", "null"],
                    "description": "Optional environment variables as name/value pairs",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Environment variable name"
                            },
                            "value": {
                                "type": "string",
                                "description": "Environment variable value"
                            }
                        },
                        "required": ["name", "value"],
                        "additionalProperties": false
                    }
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
        let (command_str, work_dir, env_vars) = parse_args(arguments, ctx)?;
        let params = TerminalCreateParams::builder()
            .session_id(ctx.session_id.clone())
            .command("/bin/sh".to_string())
            .args(vec!["-c".to_string(), command_str.clone()])
            .env(env_vars)
            .cwd(work_dir.clone())
            .build();

        let handle = self
            .backend
            .create(params)
            .await
            .map_err(|e| format!("terminal create failed: {e}"))?;

        let exit_result = handle
            .wait_for_exit()
            .await
            .map_err(|e| format!("terminal wait failed: {e}"))?;

        // Get final output snapshot.
        let snapshot = handle
            .output()
            .await
            .map_err(|e| format!("terminal output failed: {e}"))?;

        drop(handle);

        let model_text = format!(
            "exit code: {}\nstdout:\n{}\nstderr:\n{}",
            exit_result.exit_code,
            truncate(&snapshot.stdout),
            truncate(&snapshot.stderr),
        );

        let mut result = model_text;
        if result.len() > OUTPUT_MAX_LEN {
            result.truncate(OUTPUT_MAX_LEN);
            result.push_str("\n... (output truncated)");
        }
        Ok(result)
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
        let (command_str, work_dir, env_vars) = parse_args(arguments, ctx)?;
        let command = vec!["/bin/sh".to_string(), "-c".to_string(), command_str.clone()];

        let params = TerminalCreateParams::builder()
            .session_id(ctx.session_id.clone())
            .command("/bin/sh".to_string())
            .args(vec!["-c".to_string(), command_str.clone()])
            .env(env_vars)
            .cwd(work_dir.clone())
            .build();

        let handle = self
            .backend
            .create(params)
            .await
            .map_err(|e| format!("terminal create failed: {e}"))?;

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

        let handle: Arc<dyn RunningTerminal> = handle.into();
        let handle_for_task = Arc::clone(&handle);
        let work_dir_for_task = work_dir.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            let result = poll_terminal(&*handle_for_task, &delta_tx).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let _ = result_tx.send((result, duration_ms));
        });

        let delta_stream = UnboundedReceiverStream::new(delta_rx);
        let stream = futures::stream::once(async { begin }).chain(delta_stream);

        let (exec_result, duration_ms) = result_rx
            .await
            .map_err(|_e| "internal error: shell task dropped".to_string())?;

        let (model_text, end_item) = match exec_result {
            Ok(snapshot) => build_shell_result(&command, &work_dir_for_task, snapshot, duration_ms),
            Err(e) => {
                let err_msg = format!("command execution failed: {e}");
                let end = protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(
                    protocol::ExecCommandItem::builder()
                        .id(String::new())
                        .command(command.clone())
                        .cwd(work_dir_for_task)
                        .status(protocol::ExecCommandStatus::Failed)
                        .stderr(err_msg.clone())
                        .exit_code(-1)
                        .duration_ms(duration_ms)
                        .build(),
                ));
                (err_msg, end)
            }
        };

        // Release terminal after polling completes.
        drop(handle);

        let stream = stream.chain(futures::stream::once(async { end_item }));
        Ok((model_text, Box::pin(stream)))
    }
}

/// Poll terminal output at intervals and emit delta items for new content.
async fn poll_terminal(
    handle: &dyn RunningTerminal,
    delta_tx: &mpsc::UnboundedSender<protocol::ToolStreamItem>,
) -> Result<TerminalOutputSnapshot, String> {
    let mut prev_stdout_len = 0usize;
    let mut prev_stderr_len = 0usize;
    loop {
        let snapshot = handle
            .output()
            .await
            .map_err(|e| format!("terminal output failed: {e}"))?;

        // Emit new stdout content.
        let stdout_bytes = snapshot.stdout.as_bytes();
        if stdout_bytes.len() > prev_stdout_len {
            // Output is monotonic: prev_stdout_len ≤ stdout_bytes.len() always holds.
            #[allow(clippy::indexing_slicing)]
            let chunk = stdout_bytes[prev_stdout_len..].to_vec();
            let _ = delta_tx.send(protocol::ToolStreamItem::Delta {
                stream: protocol::ExecOutputStream::Stdout,
                chunk,
            });
            prev_stdout_len = stdout_bytes.len();
        }

        // Emit new stderr content.
        let stderr_bytes = snapshot.stderr.as_bytes();
        if stderr_bytes.len() > prev_stderr_len {
            // Output is monotonic: prev_stderr_len ≤ stderr_bytes.len() always holds.
            #[allow(clippy::indexing_slicing)]
            let chunk = stderr_bytes[prev_stderr_len..].to_vec();
            let _ = delta_tx.send(protocol::ToolStreamItem::Delta {
                stream: protocol::ExecOutputStream::Stderr,
                chunk,
            });
            prev_stderr_len = stderr_bytes.len();
        }

        if snapshot.exit_status.is_some() {
            return Ok(snapshot);
        }

        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Parse tool arguments into command string, working directory, and env vars.
fn parse_args(
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<(String, PathBuf, Vec<TerminalEnvVariable>), String> {
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
    let env_vars = arguments
        .get("env")
        .map(parse_env_vars)
        .transpose()?
        .unwrap_or_default();
    Ok((command, work_dir, env_vars))
}

/// Parse environment variables from the strict array shape or legacy object shape.
fn parse_env_vars(value: &serde_json::Value) -> Result<Vec<TerminalEnvVariable>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(obj) = value.as_object() {
        return Ok(obj
            .iter()
            .map(|(k, v)| {
                TerminalEnvVariable::builder()
                    .name(k.clone())
                    .value(v.as_str().unwrap_or_default().to_string())
                    .build()
            })
            .collect());
    }
    let Some(items) = value.as_array() else {
        return Err("'env' must be an array of {name, value} objects".to_string());
    };
    items
        .iter()
        .map(|item| {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("'env' entries must include string 'name'")?;
            let value = item
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or("'env' entries must include string 'value'")?;
            Ok(TerminalEnvVariable::builder()
                .name(name.to_string())
                .value(value.to_string())
                .build())
        })
        .collect()
}

/// Build the model-facing text and `End` lifecycle item for a completed shell command.
fn build_shell_result(
    command: &[String],
    cwd: &std::path::Path,
    snapshot: TerminalOutputSnapshot,
    duration_ms: u64,
) -> (String, protocol::ToolStreamItem) {
    let status = if snapshot
        .exit_status
        .as_ref()
        .is_none_or(|es| es.exit_code == 0)
    {
        protocol::ExecCommandStatus::Completed
    } else {
        protocol::ExecCommandStatus::Failed
    };

    let exit_code = snapshot.exit_status.as_ref().map_or(-1, |es| es.exit_code);

    let model_text = format!(
        "exit code: {}\nstdout:\n{}\nstderr:\n{}",
        exit_code,
        truncate(&snapshot.stdout),
        truncate(&snapshot.stderr),
    );

    let end_item = protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(
        protocol::ExecCommandItem::builder()
            .id(String::new())
            .command(command.to_vec())
            .cwd(cwd.to_path_buf())
            .status(status)
            .stdout(snapshot.stdout)
            .stderr(snapshot.stderr)
            .exit_code(exit_code)
            .duration_ms(duration_ms)
            .build(),
    ));

    (model_text, end_item)
}

/// Truncate command output to the per-stream display budget.
///
/// Calls [`str::floor_char_boundary`] before slicing, guaranteeing the index
/// lands on a valid UTF-8 character boundary.
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
    use std::path::Path;

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
    fn shell_parameters_use_strict_env_array_schema() {
        let parameters = ShellCommand::new().parameters();
        let env = &parameters["properties"]["env"];

        assert_eq!(env["type"], serde_json::json!(["array", "null"]));
        assert_eq!(
            env["items"]["additionalProperties"],
            serde_json::json!(false)
        );
        assert!(env.get("additionalProperties").is_none());
    }

    #[test]
    fn parse_args_accepts_strict_env_array() {
        let (command, _cwd, env) = parse_args(
            serde_json::json!({
                "command": "printenv FOO",
                "env": [{"name": "FOO", "value": "bar"}]
            }),
            &ToolContext::for_test(Path::new(".")),
        )
        .unwrap();

        assert_eq!(command, "printenv FOO");
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].name, "FOO");
        assert_eq!(env[0].value, "bar");
    }

    #[test]
    fn parse_args_accepts_legacy_env_object() {
        let (_command, _cwd, env) = parse_args(
            serde_json::json!({
                "command": "printenv FOO",
                "env": {"FOO": "bar"}
            }),
            &ToolContext::for_test(Path::new(".")),
        )
        .unwrap();

        assert_eq!(env.len(), 1);
        assert_eq!(env[0].name, "FOO");
        assert_eq!(env[0].value, "bar");
    }

    #[test]
    fn truncate_utf8_boundary_does_not_panic() {
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

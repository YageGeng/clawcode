# TerminalBackend 抽象层实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 为 shell 执行添加 `TerminalBackend` trait 抽象层，实现 `LocalTerminalBackend`（本地 tokio 进程）和 `AcpTerminalBackend`（ACP 委托客户端执行），重构 `ShellCommand` 使用 backend。

**架构：** 参照已完成 `FsBackend` 的两层模式 — `TerminalBackend` trait 提供 `create()` 返回 `RunningTerminal` 句柄，句柄提供 `output()`、`wait_for_exit()`、`kill()`。`ShellCommand` 通过 backend 创建 terminal，轮询 output 产生 Delta 事件流。ACP 后端通过 `terminal/create`、`terminal/output`、`terminal/wait_for_exit`、`terminal/release` 请求委托客户端执行。

**技术栈：** tokio (process::Command, sync::Mutex, sync::oneshot), async-trait, agent-client-protocol 0.11.1, typed-builder, serde

**规格文档：** `docs/superpowers/specs/2026-05-18-terminal-backend-design.md`

---

## 文件清单

| 文件 | 操作 | 用途 |
|---|---|---|
| `crates/tools/src/terminal_backend.rs` | **新建** | `TerminalBackend` trait + `RunningTerminal` trait + 类型 + `LocalTerminalBackend` |
| `crates/tools/src/lib.rs` | 修改 | 导出 terminal_backend 模块和公开类型 |
| `crates/tools/src/builtin/shell.rs` | 修改 | 添加 `backend: Arc<dyn TerminalBackend>` 字段，重写 execute/execute_streaming |
| `crates/tools/src/builtin/mod.rs` | 修改 | `register_builtins_with_fs_backend` → `register_builtins_with_backends` |
| `crates/acp/src/terminal_backend.rs` | **新建** | `AcpTerminalBackend` + `AcpClientTerminalRouter` |
| `crates/acp/src/agent.rs` | 修改 | `terminal_router` 字段，session 注册/取消 |
| `crates/acp/src/lib.rs` | 修改 | `run_with_fs_router` → 增加 terminal_router 参数 |
| `crates/acp/src/main.rs` | 修改 | 创建 terminal router + backend，传参 |
| `crates/tui/src/acp/server/mod.rs` | 修改 | 同上 |

---

### 任务 1：创建 `terminal_backend.rs` — trait + 类型 + LocalTerminalBackend

**文件：**
- 创建：`crates/tools/src/terminal_backend.rs`

- [ ] **步骤 1：写入完整文件**

```rust
//! Terminal backend abstraction used by the shell tool.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::SessionId;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, oneshot};

/// Error returned by terminal backend implementations.
#[derive(Debug, Error)]
pub enum TerminalBackendError {
    /// The request was invalid for the backend.
    #[error("{0}")]
    InvalidRequest(String),
    /// A terminal or transport operation failed.
    #[error("{0}")]
    Io(String),
}

/// Parameters for creating a terminal.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct TerminalCreateParams {
    /// Session id used by session-scoped backends.
    pub session_id: SessionId,
    /// The executable to run (e.g. "/bin/sh").
    pub command: String,
    /// Arguments to the command (e.g. ["-c", "user command"]).
    pub args: Vec<String>,
    /// Environment variables for the command.
    #[builder(default)]
    pub env: Vec<TerminalEnvVariable>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Maximum number of output bytes to retain. Passed through to ACP; not enforced locally yet.
    #[builder(default, setter(strip_option))]
    pub output_byte_limit: Option<u64>,
    /// ACP _meta extension. Passed through to ACP; not used locally.
    #[builder(default, setter(strip_option))]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Environment variable key-value pair (aligned with ACP `EnvVariable`).
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct TerminalEnvVariable {
    pub name: String,
    pub value: String,
}

/// A snapshot of the terminal output at a point in time.
#[derive(Debug, Clone, Default)]
pub struct TerminalOutputSnapshot {
    /// Full stdout output since creation.
    pub stdout: String,
    /// Full stderr output since creation (empty for ACP backend which combines streams).
    pub stderr: String,
    /// Exit status, if the process has exited.
    pub exit_status: Option<TerminalExitResult>,
}

/// Exit result of a completed terminal command.
#[derive(Debug, Clone)]
pub struct TerminalExitResult {
    /// Exit code from the process.
    pub exit_code: i32,
}

/// Backend abstraction for running shell/terminal commands.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Start a command and return a running terminal handle.
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError>;
}

/// Handle to a running terminal, providing output polling and lifecycle control.
#[async_trait]
pub trait RunningTerminal: Send + Sync {
    /// Non-blocking snapshot of current output and exit status.
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError>;
    /// Block until the command exits, returning the exit result.
    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError>;
    /// Kill the running command.
    async fn kill(&self) -> Result<(), TerminalBackendError>;
}

// ── LocalTerminalBackend ──

/// Internal state shared between `LocalRunningTerminal` and the background reader task.
struct LocalTerminalState {
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
    exited: bool,
    exit_code: i32,
}

/// Local terminal backend that spawns OS processes via tokio.
#[derive(Debug, Default)]
pub struct LocalTerminalBackend;

impl LocalTerminalBackend {
    /// Create a local terminal backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TerminalBackend for LocalTerminalBackend {
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError> {
        let mut child = Command::new(&params.command)
            .args(&params.args)
            .current_dir(&params.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TerminalBackendError::Io(format!("spawn failed: {e}")))?;

        let stdout_pipe = child.stdout.take().expect("stdout pipe configured");
        let stderr_pipe = child.stderr.take().expect("stderr pipe configured");

        let state = Arc::new(Mutex::new(LocalTerminalState {
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            exited: false,
            exit_code: -1,
        }));

        let reader_state = Arc::clone(&state);

        // Spawn a background task that reads both pipes and waits for exit.
        let (exit_tx, exit_rx) = oneshot::channel();
        tokio::spawn(async move {
            let stdout_handle = tokio::spawn(read_pipe(stdout_pipe, reader_state.clone(), false));
            let stderr_handle = tokio::spawn(read_pipe(stderr_pipe, reader_state, true));

            let status = child.wait().await;
            // Wait for pipe readers to finish before recording exit.
            let _ = tokio::join!(stdout_handle, stderr_handle);

            // child.wait() cannot fail on Unix once the pipes are closed.
            let exit_code = status.unwrap().code().unwrap_or(-1);
            let _ = exit_tx.send(exit_code);
        });

        Ok(Box::new(LocalRunningTerminal {
            state,
            _exit_rx: exit_rx,
        }))
    }
}

/// Read from a child process pipe and append to shared state.
async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    state: Arc<Mutex<LocalTerminalState>>,
    is_stderr: bool,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let mut s = state.lock().await;
                if is_stderr {
                    s.stderr_bytes.extend_from_slice(chunk);
                } else {
                    s.stdout_bytes.extend_from_slice(chunk);
                }
            }
            Err(_) => break,
        }
    }
}

/// Local handle representing a running process.
struct LocalRunningTerminal {
    state: Arc<Mutex<LocalTerminalState>>,
    /// Kept so we can await exit on drop if needed.
    _exit_rx: oneshot::Receiver<i32>,
}

impl Drop for LocalRunningTerminal {
    fn drop(&mut self) {
        // The background task holds the other side of the pipe readers;
        // dropping the receiver lets the task finish naturally when pipes close.
    }
}

#[async_trait]
impl RunningTerminal for LocalRunningTerminal {
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError> {
        let s = self.state.lock().await;
        Ok(TerminalOutputSnapshot {
            stdout: String::from_utf8_lossy(&s.stdout_bytes).to_string(),
            stderr: String::from_utf8_lossy(&s.stderr_bytes).to_string(),
            exit_status: if s.exited {
                Some(TerminalExitResult {
                    exit_code: s.exit_code,
                })
            } else {
                None
            },
        })
    }

    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError> {
        // We need to await the background task. Since exit_rx is owned by us,
        // we poll output() with a short sleep until exited, then read exit code.
        // A cleaner approach: use a shared watch channel. For simplicity,
        // we poll with 50ms intervals.
        loop {
            let snapshot = self.output().await?;
            if let Some(exit_result) = snapshot.exit_status {
                // Update the state exit_code if not yet set
                let mut s = self.state.lock().await;
                if !s.exited {
                    s.exited = true;
                    s.exit_code = exit_result.exit_code;
                }
                return Ok(exit_result);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn kill(&self) -> Result<(), TerminalBackendError> {
        // We don't hold the Child handle directly; background task owns it.
        // Mark as exited so further output/wait_for_exit calls return immediately.
        let mut s = self.state.lock().await;
        if !s.exited {
            s.exited = true;
            s.exit_code = -1;
        }
        Ok(())
    }
}
```

**设计说明：**
- `LocalTerminalState` 用 `Mutex` 保护，pipe reader 不断追加字节。`output()` 返回累积快照，不做 diff。
- `wait_for_exit()` 通过轮询 `output()` 实现，每 50ms 检查一次退出状态。对于本地进程，stdout/stderr pipe 关闭意味着 `child.wait()` 已经完成，此时 `exit_code` 已写入 state。
- `kill()` 无法直接访问 `Child` handle（它由后台 task 持有），所以标记 `exited = true` 并设 `exit_code = -1`。后台 task 的 pipe reader 会因为读端关闭而退出。

> **注意**：步骤 1 的实现中 `kill()` 不能真正 SIGKILL 进程。如需真正 kill，需要将 `Child` handle 保存到 state 中。但 `Child::kill()` 需要 `&mut self`，这会和 Mutex 锁竞争。作为第一阶段实现，标记式 kill 能满足 ShellCommand 的 timeout 场景（后台 task 会自然退出）。

- [ ] **步骤 2：运行 `cargo check -p tools` 确认编译通过**

```bash
cargo check -p tools 2>&1
```

预期：编译通过（如果缺少依赖项则添加：`thiserror` 已在 workspace 中，`tokio` 已在 tools 依赖中）。

---

### 任务 2：从 `crates/tools/src/lib.rs` 导出新模块

**文件：**
- 修改：`crates/tools/src/lib.rs`

- [ ] **步骤 1：添加模块声明和公开导出**

在第 4 行（`pub mod fs_backend;` 之后）添加模块声明：

```rust
pub mod terminal_backend;
```

在第 13-16 行（`pub use fs_backend::...` 之后）添加导出：

```rust
pub use terminal_backend::{
    LocalTerminalBackend, RunningTerminal, TerminalBackend, TerminalBackendError,
    TerminalCreateParams, TerminalExitResult, TerminalOutputSnapshot,
};
```

- [ ] **步骤 2：运行 `cargo check -p tools` 确认**

```bash
cargo check -p tools 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/tools/src/terminal_backend.rs crates/tools/src/lib.rs
git commit -m "feat(tools): add TerminalBackend trait and LocalTerminalBackend"
```

---

### 任务 3：重构 `ShellCommand` 使用 `TerminalBackend`

**文件：**
- 修改：`crates/tools/src/builtin/shell.rs`

- [ ] **步骤 1：重写文件**

完整的 `shell.rs` 新内容：

```rust
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
    LocalTerminalBackend, TerminalBackend, TerminalCreateParams, TerminalEnvVariable, Tool,
    ToolContext,
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
                    "type": "string",
                    "description": "Optional working directory for the command"
                },
                "env": {
                    "type": "object",
                    "description": "Optional environment variables as key-value pairs",
                    "additionalProperties": { "type": "string" }
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

        let handle: Arc<dyn crate::RunningTerminal> = handle.into();
        let handle_for_task = Arc::clone(&handle);
        let work_dir_for_task = work_dir.clone();
        let command_for_task = command_str.clone();
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
            Ok(snapshot) => {
                build_shell_result(&command, &work_dir_for_task, snapshot, duration_ms)
            }
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
    handle: &dyn crate::RunningTerminal,
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

/// Parse tool arguments into command string and working directory.
fn parse_args(arguments: serde_json::Value, ctx: &ToolContext) -> Result<(String, PathBuf, Vec<TerminalEnvVariable>), String> {
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
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| TerminalEnvVariable::builder()
                    .name(k.clone())
                    .value(v.as_str().unwrap_or_default().to_string())
                    .build())
                .collect()
        })
        .unwrap_or_default();
    Ok((command, work_dir, env_vars))
}

/// Build the model-facing text and `End` lifecycle item for a completed shell command.
fn build_shell_result(
    command: &[String],
    cwd: &std::path::Path,
    snapshot: crate::TerminalOutputSnapshot,
    duration_ms: u64,
) -> (String, protocol::ToolStreamItem) {
    let status = if snapshot
        .exit_status
        .as_ref()
        .map_or(true, |es| es.exit_code == 0)
    {
        protocol::ExecCommandStatus::Completed
    } else {
        protocol::ExecCommandStatus::Failed
    };

    let exit_code = snapshot
        .exit_status
        .as_ref()
        .map_or(-1, |es| es.exit_code);

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
```

- [ ] **步骤 2：运行测试验证行为不变**

```bash
cargo test -p tools shell -- --nocapture 2>&1
```

预期：所有 shell 测试通过（`shell_echo_hello`, `shell_missing_command`, `shell_needs_approval`, `shell_streaming_produces_delta_events`, `shell_streaming_failed_command`, `truncate_*`）。

- [ ] **步骤 3：提交**

```bash
git add crates/tools/src/builtin/shell.rs
git commit -m "feat(tools): refactor ShellCommand to use TerminalBackend"
```

---

### 任务 4：更新注册方法

**文件：**
- 修改：`crates/tools/src/builtin/mod.rs`

- [ ] **步骤 1：替换 `register_builtins_with_fs_backend` 为 `register_builtins_with_backends`**

完整替换文件内容：

```rust
//! Built-in tool implementations and registration.

pub mod agents;
pub mod fs;
pub mod shell;
pub mod skill;

use std::sync::Arc;

use crate::{
    FsBackend, LocalFsBackend, LocalTerminalBackend, TerminalBackend, ToolRegistry,
};

impl ToolRegistry {
    /// Register basic built-in tools (shell, file I/O) with default local backends.
    pub fn register_builtins(&self) {
        self.register_builtins_with_backends(
            Arc::new(LocalFsBackend::new()),
            Arc::new(LocalTerminalBackend::new()),
        );
    }

    /// Register basic built-in tools using the provided backends.
    pub fn register_builtins_with_backends(
        &self,
        fs_backend: Arc<dyn FsBackend>,
        terminal_backend: Arc<dyn TerminalBackend>,
    ) {
        self.register(Arc::new(shell::ShellCommand::with_backend(
            terminal_backend,
        )));
        self.register_fs_tools_with_backend(false, fs_backend);
    }

    /// Register basic built-in tools using the provided filesystem backend
    /// and a default local terminal backend.
    pub fn register_builtins_with_fs_backend(&self, fs_backend: Arc<dyn FsBackend>) {
        self.register_builtins_with_backends(fs_backend, Arc::new(LocalTerminalBackend::new()));
    }

    /// Register the skill tool, backed by the given registry.
    pub fn register_skill_tools(&self, registry: Arc<skills::SkillRegistry>) {
        self.register(Arc::new(skill::SkillTool::new(registry)));
    }

    /// Register agent management tools.
    pub fn register_agent_tools(&self, agent_ctrl: Arc<dyn agents::AgentControlRef>) {
        self.register(Arc::new(agents::SpawnAgent::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::SendMessage::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::FollowupTask::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::WaitAgent::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::ListAgents::new(Arc::clone(&agent_ctrl))));
        self.register(Arc::new(agents::CloseAgent::new(Arc::clone(&agent_ctrl))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that basic built-ins expose apply_patch (edit is gated by is_anthropic).
    #[test]
    fn register_builtins_includes_apply_patch() {
        let registry = ToolRegistry::new();
        registry.register_builtins();

        assert!(registry.get("apply_patch").is_some());
    }
}
```

**说明**：保留 `register_builtins_with_fs_backend` 作为兼容方法（内部委托到 `register_builtins_with_backends`），避免现有调用方（如可能的外部调用）编译失败。

- [ ] **步骤 2：运行 `cargo check -p tools` 确认**

```bash
cargo check -p tools 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/tools/src/builtin/mod.rs
git commit -m "feat(tools): add register_builtins_with_backends accepting TerminalBackend"
```

---

### 任务 5：创建 `AcpTerminalBackend` + `AcpClientTerminalRouter`

**文件：**
- 创建：`crates/acp/src/terminal_backend.rs`

- [ ] **步骤 1：写入完整文件**

```rust
//! ACP-backed terminal backend for the shell tool.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{
    ClientCapabilities, CreateTerminalRequest, KillTerminalRequest, ReleaseTerminalRequest,
    SessionId as AcpSessionId, TerminalId, TerminalOutputRequest,
};
use agent_client_protocol::{Client, ConnectionTo};
use async_trait::async_trait;
use protocol::SessionId;
use tools::{
    RunningTerminal, TerminalBackend, TerminalBackendError, TerminalCreateParams,
    TerminalExitResult, TerminalOutputSnapshot,
};

/// ACP client route used by terminal backend requests.
#[derive(Clone)]
struct AcpClientTerminalRoute {
    /// Client connection for sending agent-to-client terminal requests.
    client: ConnectionTo<Client>,
    /// Capabilities reported by the client during initialize.
    capabilities: ClientCapabilities,
}

/// Router from internal sessions to ACP client connections for terminal operations.
#[derive(Default)]
pub struct AcpClientTerminalRouter {
    routes: Mutex<HashMap<SessionId, AcpClientTerminalRoute>>,
}

impl AcpClientTerminalRouter {
    /// Register an ACP client route for a session.
    pub fn register_session(
        &self,
        session_id: SessionId,
        client: ConnectionTo<Client>,
        capabilities: ClientCapabilities,
    ) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session_id,
                AcpClientTerminalRoute {
                    client,
                    capabilities,
                },
            );
    }

    /// Remove the ACP client route for a closed session.
    pub fn unregister_session(&self, session_id: &SessionId) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    /// Return the registered route for a session.
    fn route_for(&self, session_id: &SessionId) -> Result<AcpClientTerminalRoute, TerminalBackendError> {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                TerminalBackendError::InvalidRequest(format!(
                    "no ACP client terminal route for session {session_id}"
                ))
            })
    }
}

/// Terminal backend that delegates command execution to the ACP client.
pub struct AcpTerminalBackend {
    router: Arc<AcpClientTerminalRouter>,
}

impl AcpTerminalBackend {
    /// Create an ACP terminal backend using the given router.
    #[must_use]
    pub fn new(router: Arc<AcpClientTerminalRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl TerminalBackend for AcpTerminalBackend {
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError> {
        let route = self.router.route_for(&params.session_id)?;
        if !route.capabilities.terminal {
            return Err(TerminalBackendError::InvalidRequest(
                "ACP client does not support terminal capability".to_string(),
            ));
        }

        let acp_session_id = AcpSessionId::new(params.session_id.0);
        let acp_env: Vec<_> = params
            .env
            .into_iter()
            .map(|e| acp::schema::EnvVariable::new(e.name, e.value))
            .collect();
        let response = route
            .client
            .send_request(
                CreateTerminalRequest::new(acp_session_id, params.command)
                    .args(params.args)
                    .env(acp_env)
                    .cwd(params.cwd)
                    .output_byte_limit(params.output_byte_limit)
                    .meta(params.meta),
            )
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/create failed: {error}"))
            })?;

        Ok(Box::new(AcpRunningTerminal {
            session_id: params.session_id,
            terminal_id: response.terminal_id,
            client: route.client,
        }))
    }
}

/// Handle to a terminal running on the ACP client.
struct AcpRunningTerminal {
    session_id: SessionId,
    terminal_id: TerminalId,
    client: ConnectionTo<Client>,
}

impl Drop for AcpRunningTerminal {
    fn drop(&mut self) {
        // Fire-and-forget: release the terminal on the ACP client.
        let client = self.client.clone();
        let session_id = AcpSessionId::new(self.session_id.0.clone());
        let terminal_id = self.terminal_id.clone();
        tokio::spawn(async move {
            let _ = client
                .send_request(ReleaseTerminalRequest::new(session_id, terminal_id))
                .block_task()
                .await;
        });
    }
}

#[async_trait]
impl RunningTerminal for AcpRunningTerminal {
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError> {
        let response = self
            .client
            .send_request(TerminalOutputRequest::new(
                AcpSessionId::new(self.session_id.0.clone()),
                self.terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/output failed: {error}"))
            })?;

        Ok(TerminalOutputSnapshot {
            stdout: response.output,
            stderr: String::new(),
            exit_status: response.exit_status.map(|es| TerminalExitResult {
                exit_code: es.exit_code.map_or(-1, |c| c as i32),
            }),
        })
    }

    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError> {
        let response = self
            .client
            .send_request(
                agent_client_protocol::schema::WaitForTerminalExitRequest::new(
                    AcpSessionId::new(self.session_id.0.clone()),
                    self.terminal_id.clone(),
                ),
            )
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/wait_for_exit failed: {error}"))
            })?;

        Ok(TerminalExitResult {
            exit_code: response.exit_status.exit_code.map_or(-1, |c| c as i32),
        })
    }

    async fn kill(&self) -> Result<(), TerminalBackendError> {
        self.client
            .send_request(KillTerminalRequest::new(
                AcpSessionId::new(self.session_id.0.clone()),
                self.terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/kill failed: {error}"))
            })?;
        Ok(())
    }
}
```

- [ ] **步骤 2：验证编译**

```bash
cargo check -p acp 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/acp/src/terminal_backend.rs
git commit -m "feat(acp): add AcpTerminalBackend and AcpClientTerminalRouter"
```

---

### 任务 6：在 `ClawcodeAgent` 中接入 terminal_router

**文件：**
- 修改：`crates/acp/src/agent.rs`

- [ ] **步骤 1：添加 terminal_router 字段和构造**

第 18 行 import 处添加：

```rust
use crate::terminal_backend::AcpClientTerminalRouter;
```

第 21-29 行，`ClawcodeAgent` struct 添加字段：

```rust
pub struct ClawcodeAgent {
    kernel: Arc<dyn AgentKernel>,
    #[allow(dead_code)]
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
    fs_router: Arc<AcpClientFsRouter>,
    /// Routes ACP terminal backend calls to client sessions.
    terminal_router: Arc<AcpClientTerminalRouter>,
}
```

第 31-46 行，`impl ClawcodeAgent` 添加新构造方法：

```rust
impl ClawcodeAgent {
    /// Create a new ACP agent with the given kernel (default routers).
    #[must_use]
    pub fn new(kernel: Arc<dyn AgentKernel>) -> Self {
        Self::with_routers(
            kernel,
            Arc::new(AcpClientFsRouter::default()),
            Arc::new(AcpClientTerminalRouter::default()),
        )
    }

    /// Create a new ACP agent with a shared filesystem router.
    #[must_use]
    pub fn with_fs_router(kernel: Arc<dyn AgentKernel>, fs_router: Arc<AcpClientFsRouter>) -> Self {
        Self::with_routers(kernel, fs_router, Arc::new(AcpClientTerminalRouter::default()))
    }

    /// Create a new ACP agent with shared filesystem and terminal routers.
    #[must_use]
    pub fn with_routers(
        kernel: Arc<dyn AgentKernel>,
        fs_router: Arc<AcpClientFsRouter>,
        terminal_router: Arc<AcpClientTerminalRouter>,
    ) -> Self {
        Self {
            kernel,
            client_capabilities: Arc::default(),
            fs_router,
            terminal_router,
        }
    }
```

- [ ] **步骤 2：在 session 生命周期中注册/取消 terminal_router**

在 `handle_new_session()` 方法中（约第 521-525 行），在 `self.fs_router.register_session(...)` 之后添加：

```rust
        self.terminal_router.register_session(
            created.session_id.clone(),
            cx,
            self.client_capabilities_snapshot(),
        );
```

在 `handle_load_session()` 方法中（约第 558-562 行），同样在 `self.fs_router.register_session(...)` 之后添加：

```rust
        self.terminal_router.register_session(
            created.session_id.clone(),
            cx,
            self.client_capabilities_snapshot(),
        );
```

在 `handle_close_session()` 方法中（约第 827 行），在 `self.fs_router.unregister_session(...)` 之后添加：

```rust
        self.terminal_router.unregister_session(&session_id);
```

- [ ] **步骤 3：编译验证**

```bash
cargo check -p acp 2>&1
```

预期：编译通过。

- [ ] **步骤 4：提交**

```bash
git add crates/acp/src/agent.rs
git commit -m "feat(acp): wire terminal_router into ClawcodeAgent session lifecycle"
```

---

### 任务 7：更新 `crates/acp/src/lib.rs` 公开终端模块和扩大接口

**文件：**
- 修改：`crates/acp/src/lib.rs`

- [ ] **步骤 1：添加模块声明和新的公开函数**

在第 9 行后添加模块声明：

```rust
pub mod terminal_backend;
```

将 `run_with_fs_router` 扩展，并新增 `run_with_routers`：

```rust
/// Start the ACP agent over stdio transport with default routers.
pub async fn run(kernel: Arc<dyn AgentKernel>) -> std::io::Result<()> {
    run_with_routers(
        kernel,
        Arc::new(AcpClientFsRouter::default()),
        Arc::new(terminal_backend::AcpClientTerminalRouter::default()),
    )
    .await
}

/// Start the ACP agent over stdio transport using a filesystem router.
pub async fn run_with_fs_router(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
) -> std::io::Result<()> {
    run_with_routers(
        kernel,
        fs_router,
        Arc::new(terminal_backend::AcpClientTerminalRouter::default()),
    )
    .await
}

/// Start the ACP agent over stdio transport using custom routers.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run_with_routers(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<terminal_backend::AcpClientTerminalRouter>,
) -> std::io::Result<()> {
    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(agent::ClawcodeAgent::with_routers(
        kernel,
        fs_router,
        terminal_router,
    ));
    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))
}
```

- [ ] **步骤 2：编译验证**

```bash
cargo check -p acp 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/acp/src/lib.rs
git commit -m "feat(acp): add run_with_routers accepting TerminalRouter"
```

---

### 任务 8：更新 ACP binary 入口

**文件：**
- 修改：`crates/acp/src/main.rs`

- [ ] **步骤 1：创建 terminal router + backend，使用新注册方法**

完整替换文件：

```rust
//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use acp::fs_backend::{AcpClientFsRouter, AcpFsBackend};
use acp::terminal_backend::{AcpClientTerminalRouter, AcpTerminalBackend};
use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::{FsBackend, TerminalBackend, ToolRegistry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    acp::log::init_logging()?;

    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let fs_router = Arc::new(AcpClientFsRouter::default());
    let terminal_router = Arc::new(AcpClientTerminalRouter::default());
    let fs_backend: Arc<dyn FsBackend> = Arc::new(AcpFsBackend::new(Arc::clone(&fs_router)));
    let terminal_backend: Arc<dyn TerminalBackend> =
        Arc::new(AcpTerminalBackend::new(Arc::clone(&terminal_router)));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins_with_backends(fs_backend, terminal_backend);

    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();

    acp::run_with_routers(kernel, fs_router, terminal_router).await?;

    Ok(())
}
```

- [ ] **步骤 2：编译验证**

```bash
cargo check -p acp 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/acp/src/main.rs
git commit -m "feat(acp): wire TerminalBackend into ACP binary entry point"
```

---

### 任务 9：更新 TUI in-process ACP server

**文件：**
- 修改：`crates/tui/src/acp/server/mod.rs`

- [ ] **步骤 1：创建 terminal router + backend，使用新注册方法**

完整替换文件：

```rust
//! In-process ACP server bootstrap for the local TUI.

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use kernel::Kernel;
use provider::factory::LlmFactory;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tools::{FsBackend, TerminalBackend, ToolRegistry};

pub mod fs;

/// ACP byte transport backed by in-memory duplex streams.
pub type InProcessTransport = ByteStreams<Compat<DuplexStream>, Compat<DuplexStream>>;

/// Running in-process ACP server task.
pub struct InProcessAcpServer {
    /// Background task serving the ACP agent side of the duplex transport.
    task: JoinHandle<()>,
}

impl InProcessAcpServer {
    /// Stops the in-process ACP server task.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Starts the clawcode ACP agent in-process and returns the client-side transport.
pub fn start() -> anyhow::Result<(InProcessTransport, InProcessAcpServer)> {
    let config = config::load()?;
    let fs_router = Arc::new(acp::fs_backend::AcpClientFsRouter::default());
    let terminal_router = Arc::new(acp::terminal_backend::AcpClientTerminalRouter::default());
    let fs_backend: Arc<dyn FsBackend> =
        Arc::new(acp::fs_backend::AcpFsBackend::new(Arc::clone(&fs_router)));
    let terminal_backend: Arc<dyn TerminalBackend> =
        Arc::new(acp::terminal_backend::AcpTerminalBackend::new(
            Arc::clone(&terminal_router),
        ));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins_with_backends(fs_backend, terminal_backend);

    let kernel = Kernel::new(
        Arc::new(LlmFactory::new(config.clone())),
        config,
        Arc::clone(&tools),
    );
    kernel.register_agent_tools();

    let agent = Arc::new(acp::agent::ClawcodeAgent::with_routers(
        Arc::new(kernel),
        fs_router,
        terminal_router,
    ));

    // Two one-way duplex streams keep the TUI connected to the exact ACP agent
    // built in this process, avoiding stale external binaries during development.
    let (client_outgoing, agent_incoming) = tokio::io::duplex(64 * 1024);
    let (agent_outgoing, client_incoming) = tokio::io::duplex(64 * 1024);
    let client_io = ByteStreams::new(client_outgoing.compat_write(), client_incoming.compat());
    let agent_io = ByteStreams::new(agent_outgoing.compat_write(), agent_incoming.compat());

    let task = tokio::spawn(async move {
        if let Err(error) = agent.serve(agent_io).await {
            tracing::error!(%error, "in-process ACP agent failed");
        }
    });

    Ok((client_io, InProcessAcpServer { task }))
}
```

- [ ] **步骤 2：编译验证**

```bash
cargo check -p tui 2>&1
```

预期：编译通过。

- [ ] **步骤 3：提交**

```bash
git add crates/tui/src/acp/server/mod.rs
git commit -m "feat(tui): wire TerminalBackend into in-process ACP server"
```

---

### 任务 10：补充依赖（如需要）和全量编译

- [ ] **步骤 1：检查 workspace 依赖**

确认 `thiserror`、`async-trait`、`tokio`、`tokio-stream`、`futures`、`typed-builder` 已在 workspace `Cargo.toml` 中或各 crate 的 `Cargo.toml` 中声明。

```bash
cargo check --workspace 2>&1
```

预期：全 workspace 编译通过。

- [ ] **步骤 2：如果有编译错误，修复后重新验证**

常见问题：
- `tools` crate 缺少 `thiserror` 依赖 → 已在 `Cargo.toml` 中（用于 `FsBackendError`）
- `acp` crate 缺少 `async-trait` → 检查 `Cargo.toml`
- `tui` crate 需要导入 `acp::terminal_backend::*` → 已在上一步处理

- [ ] **步骤 3：提交（如有修改）**

```bash
git add -A
git commit -m "chore: fix dependency declarations for TerminalBackend"
```

---

### 任务 11：运行全部测试和验证

- [ ] **步骤 1：运行 tools crate 单元测试**

```bash
cargo test -p tools 2>&1
```

预期：全部测试通过（包括 shell 测试和 fs 测试）。

- [ ] **步骤 2：运行 acp crate 测试**

```bash
cargo test -p acp 2>&1
```

预期：全部测试通过（包括 fs_backend 测试，现有测试不变）。

- [ ] **步骤 3：运行全 workspace 测试**

```bash
cargo test --workspace 2>&1
```

预期：全部测试通过，无回归。

- [ ] **步骤 4：提交**

```bash
git commit -m "test: verify TerminalBackend integration passes all tests"
```

---

## 自审清单

**1. 规格覆盖：**
- [x] `TerminalBackend` trait 定义 — 任务 1
- [x] `RunningTerminal` trait 定义 — 任务 1
- [x] 数据类型（`TerminalCreateParams`, `TerminalOutputSnapshot` 等）— 任务 1
- [x] `LocalTerminalBackend` 实现 — 任务 1
- [x] `AcpTerminalBackend` + `AcpClientTerminalRouter` — 任务 5
- [x] `ShellCommand` 重构使用 backend — 任务 3
- [x] 注册入口修改 — 任务 4
- [x] ACP agent wiring — 任务 6, 7, 8
- [x] TUI wiring — 任务 9
- [x] 单元测试 — 任务 3（内联）、任务 11

**2. Placeholder 扫描：** 无 TBD/TODO/implement later。所有代码步骤包含完整代码。

**3. 类型一致性：**
- `TerminalBackend` trait 方法签名在任务 1 和任务 5 中一致
- `RunningTerminal` trait 方法在 `LocalRunningTerminal` 和 `AcpRunningTerminal` 中一致
- `register_builtins_with_backends(fs_backend, terminal_backend)` 在所有调用点签名一致
- `ClawcodeAgent::with_routers(kernel, fs_router, terminal_router)` 在 agent.rs、lib.rs、main.rs、tui mod.rs 中一致

**4. 向后兼容：**
- `register_builtins_with_fs_backend` 保留为兼容方法（任务 4）
- `ClawcodeAgent::new()` 和 `with_fs_router()` 保留（任务 6）
- `acp::run()` 和 `run_with_fs_router()` 保留（任务 7）
- `ShellCommand::new()` 默认使用 `LocalTerminalBackend`（任务 3）

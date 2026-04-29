use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, ensure};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
};

use crate::{
    ApprovalRequirement, Result, RiskLevel,
    builtin::apply_patch::ApplyPatchTool,
    context::{
        ShellStructuredOutput, StructuredToolOutput, ToolInvocation, ToolMetadata, ToolOutput,
    },
    error::{RuntimeSnafu, ToolExecutionSnafu, ToolIoSnafu},
    handler::ToolHandler,
};

const DEFAULT_YIELD_TIME_MS: u64 = 100;
const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Parses arguments for the `exec_command` tool.
#[derive(Debug, Deserialize)]
struct ExecCommandArgs {
    cmd: String,
    workdir: Option<String>,
    shell: Option<String>,
    login: Option<bool>,
    tty: Option<bool>,
    yield_time_ms: Option<u64>,
}

/// Parses arguments for the `write_stdin` tool.
#[derive(Debug, Deserialize)]
struct WriteStdinArgs {
    session_id: String,
    chars: Option<String>,
    yield_time_ms: Option<u64>,
}

/// Captures the patch payload plus any relative `cd` prefix recognized by interception.
struct InterceptedApplyPatchCommand {
    relative_workdir: Option<String>,
    patch: String,
}

/// Stores buffered output plus a read cursor for one stream.
struct SessionStream {
    data: Mutex<Vec<u8>>,
    cursor: Mutex<usize>,
}

impl SessionStream {
    /// Creates an empty buffered stream.
    fn new() -> Self {
        Self {
            data: Mutex::new(Vec::new()),
            cursor: Mutex::new(0),
        }
    }

    /// Appends bytes to the stream while keeping only the most recent window.
    async fn push_bytes(&self, chunk: &[u8]) {
        let mut data = self.data.lock().await;
        data.extend_from_slice(chunk);
        if data.len() > MAX_CAPTURE_BYTES {
            let trim = data.len() - MAX_CAPTURE_BYTES;
            data.drain(..trim);
            let mut cursor = self.cursor.lock().await;
            *cursor = cursor.saturating_sub(trim);
        }
    }

    /// Returns unread bytes as UTF-8 lossily decoded text.
    async fn take_unread_text(&self) -> String {
        let data = self.data.lock().await;
        let mut cursor = self.cursor.lock().await;
        let unread = &data[*cursor..];
        *cursor = data.len();
        String::from_utf8_lossy(unread).to_string()
    }
}

/// Holds the mutable state for one background shell session.
struct ExecSession {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Arc<SessionStream>,
    stderr: Arc<SessionStream>,
}

impl ExecSession {
    /// Creates a tracked session from a spawned child process.
    fn new(
        child: Child,
        stdin: ChildStdin,
        stdout: Arc<SessionStream>,
        stderr: Arc<SessionStream>,
    ) -> Self {
        Self {
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            stdout,
            stderr,
        }
    }
}

/// Owns the live shell sessions backing `exec_command` and `write_stdin`.
pub struct UnifiedExecProcessManager {
    next_session_id: AtomicU64,
    sessions: Mutex<HashMap<String, Arc<ExecSession>>>,
}

impl UnifiedExecProcessManager {
    /// Creates an empty process manager for unified-exec sessions.
    pub fn new() -> Self {
        Self {
            next_session_id: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Writes optional stdin bytes to a running session and returns newly captured output.
    async fn write_stdin(&self, args: WriteStdinArgs, tool: &str) -> Result<ToolOutput> {
        let session = self
            .sessions
            .lock()
            .await
            .get(&args.session_id)
            .cloned()
            .context(RuntimeSnafu {
                message: format!("unknown session_id `{}`", args.session_id),
                stage: "write-stdin-session-lookup".to_string(),
            })?;

        if let Some(chars) = args.chars {
            let mut stdin = session.stdin.lock().await;
            let handle = stdin.as_mut().context(RuntimeSnafu {
                message: "stdin is already closed".to_string(),
                stage: "write-stdin-handle".to_string(),
            })?;
            handle
                .write_all(chars.as_bytes())
                .await
                .context(ToolIoSnafu {
                    tool: tool.to_string(),
                    stage: "write-stdin-write".to_string(),
                })?;
            handle.flush().await.context(ToolIoSnafu {
                tool: tool.to_string(),
                stage: "write-stdin-flush".to_string(),
            })?;
        }

        tokio::time::sleep(Duration::from_millis(
            args.yield_time_ms.unwrap_or(DEFAULT_YIELD_TIME_MS),
        ))
        .await;

        self.collect_session_output(tool, &args.session_id, &session)
            .await
    }

    /// Spawns a long-lived shell session and returns its initial output.
    async fn spawn_session(
        &self,
        tool: &str,
        shell: &str,
        login: bool,
        workdir: &Path,
        cmd: &str,
        yield_time: Duration,
    ) -> Result<ToolOutput> {
        let mut child = build_shell_command(shell, login, workdir, cmd)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(ToolIoSnafu {
                tool: tool.to_string(),
                stage: "exec-command-spawn".to_string(),
            })?;

        let stdin = child.stdin.take().context(RuntimeSnafu {
            message: "spawned process did not expose stdin".to_string(),
            stage: "exec-command-stdin".to_string(),
        })?;
        let stdout_reader = child.stdout.take().context(RuntimeSnafu {
            message: "spawned process did not expose stdout".to_string(),
            stage: "exec-command-stdout".to_string(),
        })?;
        let stderr_reader = child.stderr.take().context(RuntimeSnafu {
            message: "spawned process did not expose stderr".to_string(),
            stage: "exec-command-stderr".to_string(),
        })?;

        let stdout = Arc::new(SessionStream::new());
        let stderr = Arc::new(SessionStream::new());
        start_stream_pump(stdout_reader, Arc::clone(&stdout));
        start_stream_pump(stderr_reader, Arc::clone(&stderr));

        let session_id = format!(
            "exec-{}",
            self.next_session_id.fetch_add(1, Ordering::Relaxed)
        );
        let session = Arc::new(ExecSession::new(child, stdin, stdout, stderr));
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), Arc::clone(&session));

        tokio::time::sleep(yield_time).await;
        self.collect_session_output(tool, &session_id, &session)
            .await
    }

    /// Collects unread output and child status for one session.
    async fn collect_session_output(
        &self,
        tool: &str,
        session_id: &str,
        session: &Arc<ExecSession>,
    ) -> Result<ToolOutput> {
        let stdout = session.stdout.take_unread_text().await;
        let stderr = session.stderr.take_unread_text().await;
        let mut child = session.child.lock().await;
        let exit_code = child.try_wait().context(ToolIoSnafu {
            tool: tool.to_string(),
            stage: "exec-command-try-wait".to_string(),
        })?;
        let running = exit_code.is_none();

        if !running {
            self.sessions.lock().await.remove(session_id);
        }

        let text = render_shell_text(
            &stdout,
            &stderr,
            exit_code.and_then(|status| status.code()),
            running,
            Some(session_id),
        );
        Ok(ToolOutput {
            text,
            structured: StructuredToolOutput::Shell(ShellStructuredOutput {
                running,
                session_id: Some(session_id.to_string()),
                stdout,
                stderr,
                exit_code: exit_code.and_then(|status| status.code()),
            }),
        })
    }
}

impl Default for UnifiedExecProcessManager {
    /// Builds the default empty process manager.
    fn default() -> Self {
        Self::new()
    }
}

/// Owns workspace-scoped shell execution behavior and delegates live sessions to the process manager.
pub struct UnifiedExecRuntime {
    root_dir: PathBuf,
    manager: Arc<UnifiedExecProcessManager>,
}

impl UnifiedExecRuntime {
    /// Creates a runtime scoped to one workspace root.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self::with_manager(root_dir, Arc::new(UnifiedExecProcessManager::new()))
    }

    /// Creates a runtime from a shared process manager plus workspace root.
    pub fn with_manager(
        root_dir: impl Into<PathBuf>,
        manager: Arc<UnifiedExecProcessManager>,
    ) -> Self {
        Self {
            root_dir: root_dir.into(),
            manager,
        }
    }

    /// Executes a one-shot or session-based shell command.
    async fn exec_command(&self, args: ExecCommandArgs, tool: &str) -> Result<ToolOutput> {
        ensure!(
            !args.cmd.trim().is_empty(),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: "exec-command-parse-args".to_string(),
                message: "cmd must not be empty".to_string(),
            }
        );

        let workdir =
            self.resolve_workdir(args.workdir.as_deref(), tool, "exec-command-workdir")?;
        if let Some(intercepted) = intercept_apply_patch_command(&args.cmd) {
            let patch_workdir = self.resolve_subdir(
                &workdir,
                intercepted.relative_workdir.as_deref(),
                tool,
                "exec-command-apply-patch-workdir",
            )?;
            return ApplyPatchTool::new(&patch_workdir).apply_patch_text(&intercepted.patch);
        }

        let shell = args.shell.as_deref().unwrap_or(default_shell());
        let login = args.login.unwrap_or(false);
        let yield_time = Duration::from_millis(args.yield_time_ms.unwrap_or(DEFAULT_YIELD_TIME_MS));

        if args.tty.unwrap_or(false) {
            self.manager
                .spawn_session(tool, shell, login, &workdir, &args.cmd, yield_time)
                .await
        } else {
            self.run_one_shot(tool, shell, login, &workdir, &args.cmd)
                .await
        }
    }

    /// Writes optional stdin bytes to a running session and returns newly captured output.
    async fn write_stdin(&self, args: WriteStdinArgs, tool: &str) -> Result<ToolOutput> {
        self.manager.write_stdin(args, tool).await
    }

    /// Resolves an optional working directory under the configured root.
    ///
    /// Relative paths are interpreted from the workspace root, and workspace-absolute
    /// paths are accepted when they stay within that root.
    fn resolve_workdir(
        &self,
        workdir: Option<&str>,
        tool: &str,
        stage: &'static str,
    ) -> Result<PathBuf> {
        let canonical_root = self.root_dir.canonicalize().context(ToolIoSnafu {
            tool: tool.to_string(),
            stage: format!("{stage}-root-canonicalize"),
        })?;
        let requested = workdir.unwrap_or(".");
        let request_path = Path::new(requested);
        let relative_path = if request_path.is_absolute() {
            let canonical_request_path = request_path.canonicalize().context(ToolIoSnafu {
                tool: tool.to_string(),
                stage: format!("{stage}-workdir-canonicalize"),
            })?;
            ensure!(
                canonical_request_path.starts_with(&canonical_root),
                ToolExecutionSnafu {
                    tool: tool.to_string(),
                    stage: stage.to_string(),
                    message: "workdir must be relative to the workspace root".to_string(),
                }
            );
            canonical_request_path
                .strip_prefix(&canonical_root)
                .map_err(|_| {
                    ToolExecutionSnafu {
                        tool: tool.to_string(),
                        stage: stage.to_string(),
                        message: "workdir must be relative to the workspace root".to_string(),
                    }
                    .build()
                })?
                .to_path_buf()
        } else {
            request_path.to_path_buf()
        };

        ensure!(
            !relative_path
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "workdir traversal via `..` is not allowed".to_string(),
            }
        );

        let resolved = canonical_root.join(relative_path);
        ensure!(
            resolved.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "workdir must stay inside the workspace root".to_string(),
            }
        );
        Ok(resolved)
    }

    /// Resolves an intercepted `cd <dir> && apply_patch` path under an already checked workdir.
    fn resolve_subdir(
        &self,
        base_dir: &Path,
        relative_subdir: Option<&str>,
        tool: &str,
        stage: &'static str,
    ) -> Result<PathBuf> {
        let Some(relative_subdir) = relative_subdir else {
            return Ok(base_dir.to_path_buf());
        };

        let canonical_base = base_dir.canonicalize().context(ToolIoSnafu {
            tool: tool.to_string(),
            stage: format!("{stage}-base-canonicalize"),
        })?;
        let subdir = Path::new(relative_subdir);
        ensure!(
            !subdir.is_absolute(),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "intercepted apply_patch workdir must stay relative".to_string(),
            }
        );
        ensure!(
            !subdir
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "intercepted apply_patch workdir traversal via `..` is not allowed"
                    .to_string(),
            }
        );

        let resolved = base_dir.join(subdir);
        let canonical_resolved = resolved.canonicalize().context(ToolIoSnafu {
            tool: tool.to_string(),
            stage: format!("{stage}-path-canonicalize"),
        })?;
        ensure!(
            canonical_resolved.starts_with(&canonical_base),
            ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "intercepted apply_patch workdir must stay inside the workspace root"
                    .to_string(),
            }
        );
        Ok(canonical_resolved)
    }

    /// Runs one shell command to completion and captures its combined output.
    async fn run_one_shot(
        &self,
        tool: &str,
        shell: &str,
        login: bool,
        workdir: &Path,
        cmd: &str,
    ) -> Result<ToolOutput> {
        let output = build_shell_command(shell, login, workdir, cmd)?
            .output()
            .await
            .context(ToolIoSnafu {
                tool: tool.to_string(),
                stage: "exec-command-output".to_string(),
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let text = render_shell_text(&stdout, &stderr, output.status.code(), false, None);

        Ok(ToolOutput {
            text,
            structured: StructuredToolOutput::Shell(ShellStructuredOutput {
                running: false,
                session_id: None,
                stdout,
                stderr,
                exit_code: output.status.code(),
            }),
        })
    }
}

/// Exposes the codex-style `exec_command` tool.
pub struct ExecCommandTool {
    runtime: Arc<UnifiedExecRuntime>,
}

impl ExecCommandTool {
    /// Creates a shared `exec_command` tool bound to the given runtime.
    pub fn new(runtime: Arc<UnifiedExecRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl ToolHandler for ExecCommandTool {
    fn name(&self) -> &'static str {
        "exec_command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command inside the workspace root. Use `tty: true` to keep the process alive and continue it with `write_stdin`."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Run shell commands in the workspace, optionally as an interactive session.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "workdir": {
                    "type": "string",
                    "description": "Optional working directory for command execution. Must be relative to the workspace root, or an absolute path inside the workspace root."
                },
                "shell": {
                    "type": "string",
                    "description": "Optional shell binary path. Defaults to `/bin/sh`."
                },
                "login": {
                    "type": "boolean",
                    "description": "Whether to start the shell in login mode."
                },
                "tty": {
                    "type": "boolean",
                    "description": "When true, keep the process alive for follow-up `write_stdin` calls."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional time to wait before collecting initial output."
                }
            },
            "required": ["cmd"]
        })
    }

    /// Marks shell execution as high risk because it can mutate the workspace.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: Duration::from_secs(30),
        }
    }

    /// Executes one shell command through the shared unified-exec runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: ExecCommandArgs =
            invocation.parse_function_arguments("exec-command-parse-args")?;
        self.runtime.exec_command(args, self.name()).await
    }
}

/// Exposes the codex-style `write_stdin` tool for live shell sessions.
pub struct WriteStdinTool {
    runtime: Arc<UnifiedExecRuntime>,
}

impl WriteStdinTool {
    /// Creates a `write_stdin` tool bound to the shared unified-exec runtime.
    pub fn new(runtime: Arc<UnifiedExecRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl ToolHandler for WriteStdinTool {
    fn name(&self) -> &'static str {
        "write_stdin"
    }

    fn description(&self) -> &'static str {
        "Write characters to a running `exec_command` session and fetch recent output."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Continue an interactive shell session started by `exec_command`.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session identifier returned by `exec_command` when `tty: true`."
                },
                "chars": {
                    "type": "string",
                    "description": "Optional text to write to the process stdin. Pass an empty string to poll output."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional time to wait before reading recent output."
                }
            },
            "required": ["session_id"]
        })
    }

    /// Marks stdin writes as high risk because they continue an already approved process.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: Duration::from_secs(30),
        }
    }

    /// Continues an existing unified-exec session and returns fresh output.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: WriteStdinArgs = invocation.parse_function_arguments("write-stdin-parse-args")?;
        self.runtime.write_stdin(args, self.name()).await
    }
}

/// Builds a shell command process configured for the local runtime.
fn build_shell_command(shell: &str, login: bool, workdir: &Path, cmd: &str) -> Result<Command> {
    let mut command = Command::new(shell);
    if login {
        command.arg("-lc");
    } else {
        command.arg("-c");
    }
    command.arg(cmd);
    command.current_dir(workdir);
    Ok(command)
}

/// Extracts apply-patch content only for the codex-supported heredoc shell forms.
fn intercept_apply_patch_command(command: &str) -> Option<InterceptedApplyPatchCommand> {
    let trimmed = command.trim();
    let (relative_workdir, apply_patch_command) = split_intercept_prefix(trimmed)?;
    let prefix = "apply_patch <<";

    let after_prefix = apply_patch_command[prefix.len()..].trim_start();
    let delimiter = after_prefix.lines().next()?.trim();
    if delimiter.is_empty() {
        return None;
    }

    let delimiter = delimiter.trim_matches('\'').trim_matches('"');
    if delimiter.is_empty() {
        return None;
    }

    let body_start = apply_patch_command.find('\n')? + 1;
    let body = &apply_patch_command[body_start..];
    let terminator = format!("\n{delimiter}");
    let end = body.find(&terminator)?;
    let trailing = body[end + terminator.len()..].trim();
    if !trailing.is_empty() {
        return None;
    }
    Some(InterceptedApplyPatchCommand {
        relative_workdir,
        patch: body[..end].to_string(),
    })
}

/// Splits a supported optional `cd <dir> &&` prefix from an apply_patch shell command.
fn split_intercept_prefix(command: &str) -> Option<(Option<String>, &str)> {
    let prefix = "apply_patch <<";
    if command.starts_with(prefix) {
        return Some((None, command));
    }
    if !command.starts_with("cd ") {
        return None;
    }

    let after_cd = command["cd ".len()..].trim_start();
    let (workdir, remainder) = parse_shell_word(after_cd)?;
    let remainder = remainder.trim_start();
    if !remainder.starts_with("&&") {
        return None;
    }
    let apply_patch_command = remainder["&&".len()..].trim_start();
    if !apply_patch_command.starts_with(prefix) {
        return None;
    }

    Some((Some(workdir), apply_patch_command))
}

/// Parses one shell word, supporting the quoted path forms codex accepts for `cd`.
fn parse_shell_word(input: &str) -> Option<(String, &str)> {
    let first = input.chars().next()?;
    if first == '\'' || first == '"' {
        let closing = input[1..].find(first)? + 1;
        let word = input[1..closing].to_string();
        let remainder = &input[closing + 1..];
        return Some((word, remainder));
    }

    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    let word = input[..end].to_string();
    let remainder = &input[end..];
    if word.is_empty() {
        return None;
    }
    Some((word, remainder))
}

/// Starts a background task that continuously appends stream bytes to a shared buffer.
fn start_stream_pump<R>(mut reader: R, stream: Arc<SessionStream>)
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => stream.push_bytes(&buffer[..count]).await,
                Err(_) => break,
            }
        }
    });
}

/// Formats stdout, stderr, and session state into a human-readable tool text body.
fn render_shell_text(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    running: bool,
    session_id: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(session_id) = session_id {
        parts.push(format!("session_id: {session_id}"));
    }
    if let Some(exit_code) = exit_code {
        parts.push(format!("exit_code: {exit_code}"));
    }
    if running {
        parts.push("running: true".to_string());
    }
    if !stdout.is_empty() {
        parts.push(format!("stdout:\n{stdout}"));
    }
    if !stderr.is_empty() {
        parts.push(format!("stderr:\n{stderr}"));
    }
    if parts.is_empty() {
        "command produced no output".to_string()
    } else {
        parts.join("\n")
    }
}

/// Returns the default shell binary used when the caller does not specify one.
const fn default_shell() -> &'static str {
    "/bin/sh"
}

#[cfg(test)]
mod tests {
    use super::intercept_apply_patch_command;

    /// Verifies the shell interception parser understands a plain apply_patch heredoc.
    #[test]
    fn intercept_apply_patch_command_matches_plain_heredoc() {
        let command = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF\n";
        let parsed = intercept_apply_patch_command(command).expect("plain heredoc should match");
        assert!(parsed.patch.contains("*** Add File: test.txt"));
        assert!(parsed.relative_workdir.is_none());
    }

    /// Verifies the parser accepts the codex-style `cd sub && apply_patch <<...` form.
    #[test]
    fn intercept_apply_patch_command_matches_cd_and_heredoc() {
        let command = "cd sub && apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF\n";
        let parsed = intercept_apply_patch_command(command).expect("cd + heredoc should match");
        assert!(parsed.patch.contains("*** Add File: test.txt"));
        assert_eq!(parsed.relative_workdir.as_deref(), Some("sub"));
    }

    /// Verifies the parser ignores scripts that append extra shell commands after the heredoc.
    #[test]
    fn intercept_apply_patch_command_rejects_trailing_commands() {
        let command = "cd sub && apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF\n&& echo done";
        assert!(intercept_apply_patch_command(command).is_none());
    }

    /// Verifies the parser ignores scripts with unrelated leading commands before `cd &&`.
    #[test]
    fn intercept_apply_patch_command_rejects_leading_commands() {
        let command = "echo before; cd sub && apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF\n";
        assert!(intercept_apply_patch_command(command).is_none());
    }
}

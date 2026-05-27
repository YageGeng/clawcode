//! Shell command execution tools.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use async_stream::stream;
use async_trait::async_trait;
use futures::{StreamExt, stream::Stream};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{
    LocalTerminalBackend, RunningTerminal, TerminalBackend,
    TerminalCreateParams, TerminalEnvVariable, TerminalOutputSnapshot, Tool,
    ToolContext,
};

const OUTPUT_MAX_LEN: usize = 4096;
const DEFAULT_YIELD_TIME_MS: u64 = 10_000;
const DEFAULT_POLL_YIELD_TIME_MS: u64 = 5_000;
const POLL_INTERVAL_MS: u64 = 50;

/// Shared runtime for shell-like tools.
pub struct ShellRuntime {
    backend: Arc<dyn TerminalBackend>,
    processes: Mutex<HashMap<i32, StoredProcess>>,
    next_process_id: AtomicI32,
}

impl ShellRuntime {
    /// Create a shell runtime backed by the given terminal backend.
    #[must_use]
    pub fn new(backend: Arc<dyn TerminalBackend>) -> Self {
        Self {
            backend,
            processes: Mutex::new(HashMap::new()),
            next_process_id: AtomicI32::new(1000),
        }
    }

    /// Spawn a command and return its logical process id plus terminal handle.
    async fn spawn(
        &self,
        args: &ShellArgs,
        ctx: &ToolContext,
    ) -> Result<(i32, Arc<dyn RunningTerminal>), String> {
        let process_id = self.next_process_id.fetch_add(1, Ordering::Relaxed);
        let params = TerminalCreateParams::builder()
            .session_id(ctx.session_id.clone())
            .command(args.shell.clone())
            .args(vec!["-c".to_string(), args.command.clone()])
            .env(args.env.clone())
            .cwd(args.cwd.clone())
            .tty(args.tty)
            .stdin_writable(true)
            .build();
        let handle = self
            .backend
            .create(params)
            .await
            .map_err(|error| format!("terminal create failed: {error}"))?;
        Ok((process_id, handle.into()))
    }

    /// Store an ongoing process for later `write_stdin` calls.
    async fn store_process(
        &self,
        process_id: i32,
        handle: Arc<dyn RunningTerminal>,
        stdout_offset: usize,
        stderr_offset: usize,
    ) {
        let mut processes = self.processes.lock().await;
        processes.insert(
            process_id,
            StoredProcess {
                handle,
                stdout_offset,
                stderr_offset,
            },
        );
    }

    /// Load a stored process and its current output offsets.
    async fn load_process(&self, process_id: i32) -> Option<StoredProcess> {
        self.processes.lock().await.get(&process_id).cloned()
    }

    /// Update output offsets or remove the process after it exits.
    async fn finish_poll(
        &self,
        process_id: i32,
        exited: bool,
        stdout_offset: usize,
        stderr_offset: usize,
    ) {
        let mut processes = self.processes.lock().await;
        if exited {
            processes.remove(&process_id);
        } else if let Some(process) = processes.get_mut(&process_id) {
            process.stdout_offset = stdout_offset;
            process.stderr_offset = stderr_offset;
        }
    }
}

/// Stored process state for an ongoing shell command.
#[derive(Clone)]
struct StoredProcess {
    handle: Arc<dyn RunningTerminal>,
    stdout_offset: usize,
    stderr_offset: usize,
}

/// Executes arbitrary shell commands via a [`TerminalBackend`].
pub struct ShellCommand {
    runtime: Arc<ShellRuntime>,
    tool_name: &'static str,
}

impl ShellCommand {
    /// Create a shell tool with the default local runtime.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalTerminalBackend::new()))
    }

    /// Create a shell tool with a custom terminal backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn TerminalBackend>) -> Self {
        Self::with_runtime(Arc::new(ShellRuntime::new(backend)))
    }

    /// Create a shell tool with a shared shell runtime.
    #[must_use]
    pub fn with_runtime(runtime: Arc<ShellRuntime>) -> Self {
        Self {
            runtime,
            tool_name: "shell",
        }
    }

    /// Create a Codex-compatible `exec_command` tool with a shared shell runtime.
    #[must_use]
    pub fn exec_command(runtime: Arc<ShellRuntime>) -> Self {
        Self {
            runtime,
            tool_name: "exec_command",
        }
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
        self.tool_name
    }

    fn description(&self) -> &str {
        "Execute a shell command. Long-running commands return a process id that can be polled with write_stdin."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cmd": {
                    "type": "string",
                    "description": "Alias for command, matching Codex exec_command"
                },
                "shell": {
                    "type": ["string", "null"],
                    "description": "Shell binary to launch; defaults to /bin/sh"
                },
                "cwd": {
                    "type": ["string", "null"],
                    "description": "Optional working directory for the command"
                },
                "workdir": {
                    "type": ["string", "null"],
                    "description": "Alias for cwd, matching Codex exec_command"
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
                },
                "tty": {
                    "type": "boolean",
                    "description": "Whether to allocate a pseudo-terminal for the command"
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Maximum time to wait for the command to finish before returning a running process id; output deltas may stream during this window"
                },
                "timeout_ms": {
                    "type": "number",
                    "description": "Maximum time to wait before killing the command"
                },
                "max_output_tokens": {
                    "type": "number",
                    "description": "Maximum approximate output tokens to return"
                }
            }
        })
    }

    fn capability(&self) -> protocol::ToolCapability {
        protocol::ToolCapability {
            supports_streaming: true,
        }
    }

    fn needs_approval(
        &self,
        _: &serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let mut text = String::new();
        let mut stream = self.execute_streaming(arguments, ctx).await?;
        while let Some(item) = stream.next().await {
            if let protocol::ToolStreamItem::Final { content, .. } = item {
                text = content;
            }
        }
        Ok(text)
    }

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<
        Pin<Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>>,
        String,
    > {
        let args = ShellArgs::parse(arguments, ctx)?;
        let command =
            vec![args.shell.clone(), "-c".to_string(), args.command.clone()];
        let (process_id, handle) = self.runtime.spawn(&args, ctx).await?;
        let runtime = Arc::clone(&self.runtime);
        let stream = stream! {
            // Emit lifecycle start before polling so ACP clients can attach
            // following deltas to a visible command item immediately.
            yield protocol::ToolStreamItem::Begin(protocol::TurnItem::ExecCommand(
                protocol::ExecCommandItem::builder()
                    .id(String::new())
                    .command(command.clone())
                    .cwd(args.cwd.clone())
                    .status(protocol::ExecCommandStatus::InProgress)
                    .build(),
            ));

            let deadline = Instant::now() + Duration::from_millis(args.yield_time_ms);
            let mut collector = OutputCollector::new();
            let timeout_at = args
                .timeout_ms
                .map(|ms| collector.started_at + Duration::from_millis(ms));

            loop {
                let (snapshot, deltas) = match collector.snapshot_with_deltas(&*handle).await {
                    Ok(result) => result,
                    Err(error) => {
                        yield protocol::ToolStreamItem::Final {
                            content: error,
                            is_error: true,
                        };
                        return;
                    }
                };
                for item in deltas {
                    yield item;
                }

                if snapshot.exit_status.is_some() {
                    let is_error = snapshot.is_error();
                    let (model_text, end_item) = build_shell_result(
                        &command,
                        &args.cwd,
                        snapshot,
                        collector.elapsed_ms(),
                        args.output_limit(),
                    );
                    yield end_item;
                    yield protocol::ToolStreamItem::Final {
                        content: model_text,
                        is_error,
                    };
                    return;
                }

                if timeout_at.is_some_and(|time| Instant::now() >= time) {
                    // A hard timeout is terminal for this invocation, so kill
                    // the process and report whatever output was captured.
                    if let Err(error) = handle.kill().await {
                        yield protocol::ToolStreamItem::Final {
                            content: format!("terminal kill failed: {error}"),
                            is_error: true,
                        };
                        return;
                    }
                    let (snapshot, deltas) = match collector.snapshot_with_deltas(&*handle).await {
                        Ok(result) => result,
                        Err(error) => {
                            yield protocol::ToolStreamItem::Final {
                                content: error,
                                is_error: true,
                            };
                            return;
                        }
                    };
                    for item in deltas {
                        yield item;
                    }
                    let is_error = snapshot.is_error();
                    let (model_text, end_item) = build_shell_result(
                        &command,
                        &args.cwd,
                        snapshot,
                        collector.elapsed_ms(),
                        args.output_limit(),
                    );
                    yield end_item;
                    yield protocol::ToolStreamItem::Final {
                        content: model_text,
                        is_error,
                    };
                    return;
                }

                if Instant::now() >= deadline {
                    // The command is still alive after the soft yield deadline;
                    // keep the handle and offsets so write_stdin can resume.
                    runtime
                        .store_process(
                            process_id,
                            Arc::clone(&handle),
                            collector.stdout_offset,
                            collector.stderr_offset,
                        )
                        .await;
                    let model_text = ShellYieldResult {
                        process_id,
                        snapshot: &snapshot,
                        output_limit: args.output_limit(),
                    }
                    .model_text();
                    yield protocol::ToolStreamItem::Final {
                        content: model_text,
                        is_error: false,
                    };
                    return;
                }

                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Writes to or polls an existing shell process.
pub struct WriteStdin {
    runtime: Arc<ShellRuntime>,
}

impl WriteStdin {
    /// Create a `write_stdin` tool using the shared shell runtime.
    #[must_use]
    pub fn new(runtime: Arc<ShellRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for WriteStdin {
    fn name(&self) -> &str {
        "write_stdin"
    }

    fn description(&self) -> &str {
        "Write characters to an existing shell process and return recent output."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "number",
                    "description": "Process id returned by shell"
                },
                "process_id": {
                    "type": "number",
                    "description": "Alias for session_id"
                },
                "chars": {
                    "type": "string",
                    "description": "Bytes to write to stdin; empty string polls output"
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Maximum time to wait for the process to produce more output or exit before returning its current status"
                },
                "max_output_tokens": {
                    "type": "number",
                    "description": "Maximum approximate output tokens to return"
                }
            },
            "required": ["session_id"]
        })
    }

    fn capability(&self) -> protocol::ToolCapability {
        protocol::ToolCapability {
            supports_streaming: true,
        }
    }

    fn needs_approval(
        &self,
        _: &serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let mut text = String::new();
        let mut stream = self.execute_streaming(arguments, ctx).await?;
        while let Some(item) = stream.next().await {
            if let protocol::ToolStreamItem::Final { content, .. } = item {
                text = content;
            }
        }
        Ok(text)
    }

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        _ctx: &crate::ToolContext,
    ) -> Result<
        Pin<Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>>,
        String,
    > {
        let args = WriteStdinArgs::parse(arguments)?;
        let Some(stored) = self.runtime.load_process(args.process_id).await
        else {
            return Err(format!(
                "unknown shell process id: {}",
                args.process_id
            ));
        };

        if !args.chars.is_empty() {
            stored
                .handle
                .write_stdin(args.chars.as_bytes())
                .await
                .map_err(|error| format!("stdin write failed: {error}"))?;
        }

        let runtime = Arc::clone(&self.runtime);
        let stream = stream! {
            let deadline = Instant::now() + Duration::from_millis(args.yield_time_ms);
            let mut collector =
                OutputCollector::with_offsets(stored.stdout_offset, stored.stderr_offset);

            loop {
                let (snapshot, deltas) = match collector.snapshot_with_deltas(&*stored.handle).await {
                    Ok(result) => result,
                    Err(error) => {
                        yield protocol::ToolStreamItem::Final {
                            content: error,
                            is_error: true,
                        };
                        return;
                    }
                };
                for item in deltas {
                    yield item;
                }

                if snapshot.exit_status.is_some() {
                    runtime
                        .finish_poll(
                            args.process_id,
                            true,
                            collector.stdout_offset,
                            collector.stderr_offset,
                        )
                        .await;
                    let is_error = snapshot.is_error();
                    let text = build_shell_result(
                        &["write_stdin".to_string(), args.process_id.to_string()],
                        Path::new("."),
                        snapshot,
                        collector.elapsed_ms(),
                        args.output_limit(),
                    )
                    .0;
                    yield protocol::ToolStreamItem::Final {
                        content: text,
                        is_error,
                    };
                    return;
                }

                if Instant::now() >= deadline {
                    // Preserve output offsets across polls so repeated
                    // write_stdin calls only emit newly observed bytes.
                    runtime
                        .finish_poll(
                            args.process_id,
                            false,
                            collector.stdout_offset,
                            collector.stderr_offset,
                        )
                        .await;
                    let text = ShellYieldResult {
                        process_id: args.process_id,
                        snapshot: &snapshot,
                        output_limit: args.output_limit(),
                    }
                    .model_text();
                    yield protocol::ToolStreamItem::Final {
                        content: text,
                        is_error: false,
                    };
                    return;
                }

                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Parsed shell command arguments.
#[derive(typed_builder::TypedBuilder)]
struct ShellArgs {
    /// Shell command text passed to the selected shell with `-c`.
    command: String,
    /// Shell binary used to execute the command.
    shell: String,
    /// Working directory for the command.
    cwd: PathBuf,
    /// Environment overrides for the command.
    env: Vec<TerminalEnvVariable>,
    /// Whether to allocate a pseudo-terminal.
    tty: bool,
    /// Maximum initial wait before yielding output.
    yield_time_ms: u64,
    /// Optional hard timeout for the command.
    #[builder(default, setter(strip_option))]
    timeout_ms: Option<u64>,
    /// Optional approximate output-token budget.
    #[builder(default, setter(strip_option))]
    max_output_tokens: Option<usize>,
}

impl ShellArgs {
    /// Parse tool arguments into a typed shell request.
    fn parse(
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<Self, String> {
        ShellArgsInput::parse(arguments)?.into_shell_args(ctx)
    }

    /// Return the output byte budget derived from max_output_tokens.
    fn output_limit(&self) -> usize {
        self.max_output_tokens
            .map_or(OUTPUT_MAX_LEN, |tokens| tokens.saturating_mul(4))
    }
}

/// Deserialized shell command request with Codex-compatible aliases.
#[derive(Debug, Deserialize, typed_builder::TypedBuilder)]
struct ShellArgsInput {
    /// Preferred command field.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    command: Option<String>,
    /// Codex-compatible command alias.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    cmd: Option<String>,
    /// Optional shell binary.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    shell: Option<String>,
    /// Preferred working directory field.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    cwd: Option<PathBuf>,
    /// Codex-compatible working directory alias.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    workdir: Option<PathBuf>,
    /// Optional environment overrides.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    env: Option<ShellEnvInput>,
    /// Whether to allocate a pseudo-terminal.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    tty: Option<bool>,
    /// Maximum initial wait before yielding output.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    yield_time_ms: Option<u64>,
    /// Optional hard timeout for the command.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    timeout_ms: Option<u64>,
    /// Optional approximate output-token budget.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    max_output_tokens: Option<usize>,
}

impl ShellArgsInput {
    /// Deserialize raw JSON into shell input fields.
    fn parse(arguments: serde_json::Value) -> Result<Self, String> {
        serde_path_to_error::deserialize(arguments).map_err(|error| {
            format!("invalid shell arguments at {}: {error}", error.path())
        })
    }

    /// Convert deserialized input into normalized shell execution arguments.
    fn into_shell_args(self, ctx: &ToolContext) -> Result<ShellArgs, String> {
        let command = self
            .command
            .or(self.cmd)
            .ok_or("missing 'command' argument")?;
        let cwd = self.cwd.or(self.workdir).unwrap_or_else(|| ctx.cwd.clone());
        let shell = self
            .shell
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        let env = self.env.map(Into::into).unwrap_or_default();
        let tty = self.tty.unwrap_or(false);
        let yield_time_ms = self.yield_time_ms.unwrap_or(DEFAULT_YIELD_TIME_MS);

        let mut args = ShellArgs::builder()
            .command(command)
            .shell(shell)
            .cwd(cwd)
            .env(env)
            .tty(tty)
            .yield_time_ms(yield_time_ms)
            .build();

        // `strip_option` builder setters intentionally accept bare values, while
        // deserialization already gives us normalized optional fields.
        args.timeout_ms = self.timeout_ms;
        args.max_output_tokens = self.max_output_tokens;

        Ok(args)
    }
}

/// Deserialized shell environment, accepting strict and legacy shapes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ShellEnvInput {
    /// Strict schema shape used by the tool definition.
    Entries(Vec<ShellEnvEntry>),
    /// Legacy object shape kept for compatibility.
    Object(HashMap<String, String>),
}

impl From<ShellEnvInput> for Vec<TerminalEnvVariable> {
    /// Convert deserialized environment input into backend variables.
    fn from(value: ShellEnvInput) -> Self {
        match value {
            ShellEnvInput::Entries(entries) => {
                entries.into_iter().map(Into::into).collect()
            }
            ShellEnvInput::Object(values) => values
                .into_iter()
                .map(|(name, value)| {
                    TerminalEnvVariable::builder()
                        .name(name)
                        .value(value)
                        .build()
                })
                .collect(),
        }
    }
}

/// Deserialized strict environment entry.
#[derive(Debug, Deserialize)]
struct ShellEnvEntry {
    /// Environment variable name.
    name: String,
    /// Environment variable value.
    value: String,
}

impl From<ShellEnvEntry> for TerminalEnvVariable {
    /// Convert a strict environment entry into a backend variable.
    fn from(value: ShellEnvEntry) -> Self {
        TerminalEnvVariable::builder()
            .name(value.name)
            .value(value.value)
            .build()
    }
}

/// Parsed `write_stdin` arguments.
#[derive(typed_builder::TypedBuilder)]
struct WriteStdinArgs {
    /// Logical process id returned by `shell`.
    process_id: i32,
    /// Bytes to write to stdin.
    chars: String,
    /// Maximum wait before yielding output.
    yield_time_ms: u64,
    /// Optional approximate output-token budget.
    #[builder(default, setter(strip_option))]
    max_output_tokens: Option<usize>,
}

impl WriteStdinArgs {
    /// Parse tool arguments into a typed stdin-write request.
    fn parse(arguments: serde_json::Value) -> Result<Self, String> {
        WriteStdinArgsInput::parse(arguments)?.into_write_stdin_args()
    }

    /// Build stdin arguments from normalized input fields.
    fn from_input(
        process_id: i32,
        chars: String,
        yield_time_ms: u64,
        max_output_tokens: Option<usize>,
    ) -> Self {
        let mut args = Self::builder()
            .process_id(process_id)
            .chars(chars)
            .yield_time_ms(yield_time_ms)
            .build();
        // Preserve the builder API's `strip_option` contract while accepting
        // already-normalized optional input from JSON parsing.
        args.max_output_tokens = max_output_tokens;
        args
    }

    /// Return the output byte budget derived from max_output_tokens.
    fn output_limit(&self) -> usize {
        self.max_output_tokens
            .map_or(OUTPUT_MAX_LEN, |tokens| tokens.saturating_mul(4))
    }
}

/// Deserialized write_stdin request with Codex-compatible aliases.
#[derive(Debug, Deserialize, typed_builder::TypedBuilder)]
struct WriteStdinArgsInput {
    /// Preferred logical process id field.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    session_id: Option<i64>,
    /// Codex-compatible logical process id alias.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    process_id: Option<i64>,
    /// Bytes to write to stdin.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    chars: Option<String>,
    /// Maximum wait before yielding output.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    yield_time_ms: Option<u64>,
    /// Optional approximate output-token budget.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    max_output_tokens: Option<usize>,
}

impl WriteStdinArgsInput {
    /// Deserialize raw JSON into stdin-write input fields.
    fn parse(arguments: serde_json::Value) -> Result<Self, String> {
        serde_path_to_error::deserialize(arguments).map_err(|error| {
            format!(
                "invalid write_stdin arguments at {}: {error}",
                error.path()
            )
        })
    }

    /// Convert deserialized input into normalized stdin-write arguments.
    fn into_write_stdin_args(self) -> Result<WriteStdinArgs, String> {
        let process_id = self
            .session_id
            .or(self.process_id)
            .ok_or("missing 'session_id' argument")?;
        let process_id = i32::try_from(process_id).map_err(|error| {
            format!("session_id must fit in a 32-bit integer: {error}")
        })?;

        Ok(WriteStdinArgs::from_input(
            process_id,
            self.chars.unwrap_or_default(),
            self.yield_time_ms.unwrap_or(DEFAULT_POLL_YIELD_TIME_MS),
            self.max_output_tokens,
        ))
    }
}

/// Stateful output collector for a process poll.
struct OutputCollector {
    started_at: Instant,
    stdout_offset: usize,
    stderr_offset: usize,
}

impl OutputCollector {
    /// Create a collector starting at zero output offsets.
    fn new() -> Self {
        Self::with_offsets(0, 0)
    }

    /// Create a collector starting at known output offsets.
    fn with_offsets(stdout_offset: usize, stderr_offset: usize) -> Self {
        Self {
            started_at: Instant::now(),
            stdout_offset,
            stderr_offset,
        }
    }

    /// Return the elapsed collection time in milliseconds.
    fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Return the current output snapshot plus stream deltas for newly observed bytes.
    async fn snapshot_with_deltas(
        &mut self,
        handle: &dyn RunningTerminal,
    ) -> Result<(TerminalOutputSnapshot, Vec<protocol::ToolStreamItem>), String>
    {
        let snapshot = handle
            .output()
            .await
            .map_err(|error| format!("terminal output failed: {error}"))?;
        let deltas = self.new_output_items(&snapshot);
        Ok((snapshot, deltas))
    }

    /// Build stream delta items for output that has not yet been emitted.
    fn new_output_items(
        &mut self,
        snapshot: &TerminalOutputSnapshot,
    ) -> Vec<protocol::ToolStreamItem> {
        let mut items = Vec::new();
        if snapshot.stdout.len() > self.stdout_offset {
            if let Some(chunk) =
                snapshot.stdout.as_bytes().get(self.stdout_offset..)
            {
                items.push(protocol::ToolStreamItem::Delta {
                    stream: protocol::ExecOutputStream::Stdout,
                    chunk: chunk.to_vec(),
                });
            }
            self.stdout_offset = snapshot.stdout.len();
        }
        if snapshot.stderr.len() > self.stderr_offset {
            if let Some(chunk) =
                snapshot.stderr.as_bytes().get(self.stderr_offset..)
            {
                items.push(protocol::ToolStreamItem::Delta {
                    stream: protocol::ExecOutputStream::Stderr,
                    chunk: chunk.to_vec(),
                });
            }
            self.stderr_offset = snapshot.stderr.len();
        }
        items
    }
}

/// Model-facing result for a shell command that yielded while still running.
struct ShellYieldResult<'a> {
    process_id: i32,
    snapshot: &'a TerminalOutputSnapshot,
    output_limit: usize,
}

impl ShellYieldResult<'_> {
    /// Render the model-facing text for a command that can be polled later.
    fn model_text(&self) -> String {
        format!(
            "process_id: {}\nstatus: running\nstdout:\n{}\nstderr:\n{}\n\nUse write_stdin with {{\"session_id\": {}, \"chars\": \"\"}} to poll for more output.",
            self.process_id,
            truncate(&self.snapshot.stdout, self.output_limit / 2),
            truncate(&self.snapshot.stderr, self.output_limit / 2),
            self.process_id,
        )
    }
}

/// Build the model-facing text and `End` lifecycle item for a completed shell command.
fn build_shell_result(
    command: &[String],
    cwd: &Path,
    snapshot: TerminalOutputSnapshot,
    duration_ms: u64,
    output_limit: usize,
) -> (String, protocol::ToolStreamItem) {
    let status = if snapshot.is_error() {
        protocol::ExecCommandStatus::Failed
    } else {
        protocol::ExecCommandStatus::Completed
    };

    let exit_code = snapshot.exit_status.as_ref().map_or(-1, |es| es.exit_code);

    let model_text = format!(
        "exit code: {}\nstdout:\n{}\nstderr:\n{}",
        exit_code,
        truncate(&snapshot.stdout, output_limit / 2),
        truncate(&snapshot.stderr, output_limit / 2),
    );

    let end_item =
        protocol::ToolStreamItem::End(protocol::TurnItem::ExecCommand(
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

/// Truncate command output to a display budget.
fn truncate(s: &str, limit: usize) -> &str {
    if s.len() > limit {
        let boundary = s.floor_char_boundary(limit);
        match s.get(..boundary) {
            Some(prefix) => prefix,
            None => s,
        }
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    /// Build a test tool context rooted at the current directory.
    fn test_ctx() -> ToolContext {
        ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(Path::new(".").to_path_buf())
            .agent_path(protocol::AgentPath::root())
            .approval_mode(protocol::ApprovalMode::default())
            .build()
    }

    /// Drain a stream and return the last model-facing text item.
    async fn final_text_and_items(
        mut stream: Pin<
            Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>,
        >,
    ) -> (String, Vec<protocol::ToolStreamItem>) {
        let mut text = String::new();
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            if let protocol::ToolStreamItem::Final { content, .. } = &item {
                text = content.clone();
            }
            items.push(item);
        }
        (text, items)
    }

    /// Extract the process id from a yielded shell result.
    fn yielded_process_id(text: &str) -> i32 {
        text.lines()
            .find_map(|line| line.strip_prefix("process_id: "))
            .expect("yielded process id")
            .parse()
            .expect("numeric process id")
    }

    #[tokio::test]
    async fn shell_echo_hello() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &test_ctx())
            .await;
        assert!(result.is_ok());
        let output = result.expect("shell output");
        assert!(output.contains("hello"));
        assert!(output.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn shell_yields_before_long_command_exits() {
        let tool = ShellCommand::new();
        let started = Instant::now();
        let stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "printf ready; sleep 2; printf done",
                    "yield_time_ms": 100
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");

        assert!(started.elapsed() < Duration::from_millis(900));
        let (text, items) = final_text_and_items(stream).await;
        assert!(text.contains("status: running"));
        assert!(text.contains("ready"));

        let process_id = yielded_process_id(&text);
        let write = WriteStdin::new(Arc::clone(&tool.runtime));
        let poll_stream = write
            .execute_streaming(
                serde_json::json!({
                    "session_id": process_id,
                    "chars": "",
                    "yield_time_ms": 2500
                }),
                &test_ctx(),
            )
            .await
            .expect("poll process");
        let (poll_text, _poll_items) = final_text_and_items(poll_stream).await;
        assert!(poll_text.contains("done"));

        assert!(items.iter().any(|item| matches!(
            item,
            protocol::ToolStreamItem::Delta { .. }
        )));
    }

    #[tokio::test]
    async fn shell_streams_delta_before_yield_deadline() {
        let tool = ShellCommand::new();
        let started = Instant::now();
        let mut stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "echo ready; sleep 2; echo done",
                    "yield_time_ms": 5000
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");

        assert!(matches!(
            stream.next().await,
            Some(protocol::ToolStreamItem::Begin(_))
        ));

        let item =
            tokio::time::timeout(Duration::from_millis(900), stream.next())
                .await
                .expect("first delta should arrive before the yield deadline")
                .expect("stream item");
        assert!(started.elapsed() < Duration::from_millis(900));
        assert!(matches!(
            item,
            protocol::ToolStreamItem::Delta { chunk, .. }
                if String::from_utf8_lossy(&chunk).contains("ready")
        ));
    }

    #[tokio::test]
    async fn shell_timeout_kills_long_command() {
        let tool = ShellCommand::new();
        let stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "sleep 2",
                    "yield_time_ms": 1000,
                    "timeout_ms": 100
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");
        let (text, _items) = final_text_and_items(stream).await;

        assert!(text.contains("exit code: -1"));
    }

    #[tokio::test]
    async fn shell_tty_makes_stdout_a_terminal() {
        let tool = ShellCommand::new();
        let stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "[ -t 1 ] && echo tty || echo pipe",
                    "tty": true,
                    "yield_time_ms": 1000
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");
        let (text, _items) = final_text_and_items(stream).await;

        assert!(text.contains("tty"));
    }

    #[tokio::test]
    async fn write_stdin_sends_input_to_tty_process() {
        let tool = ShellCommand::new();
        let stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "read line; echo got:$line",
                    "tty": true,
                    "yield_time_ms": 100
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");
        let (text, _items) = final_text_and_items(stream).await;
        let process_id = yielded_process_id(&text);

        let write = WriteStdin::new(Arc::clone(&tool.runtime));
        let stream = write
            .execute_streaming(
                serde_json::json!({
                    "session_id": process_id,
                    "chars": "hello\n",
                    "yield_time_ms": 1000
                }),
                &test_ctx(),
            )
            .await
            .expect("write stdin");
        let (text, _items) = final_text_and_items(stream).await;

        assert!(text.contains("got:hello"));
    }

    #[tokio::test]
    async fn write_stdin_sends_input_to_pipe_process() {
        let tool = ShellCommand::new();
        let stream = tool
            .execute_streaming(
                serde_json::json!({
                    "command": "read line; echo pipe:$line",
                    "yield_time_ms": 100
                }),
                &test_ctx(),
            )
            .await
            .expect("execute_streaming");
        let (text, _items) = final_text_and_items(stream).await;
        let process_id = yielded_process_id(&text);

        let write = WriteStdin::new(Arc::clone(&tool.runtime));
        let stream = write
            .execute_streaming(
                serde_json::json!({
                    "session_id": process_id,
                    "chars": "hello\n",
                    "yield_time_ms": 1000
                }),
                &test_ctx(),
            )
            .await
            .expect("write stdin");
        let (text, _items) = final_text_and_items(stream).await;

        assert!(text.contains("pipe:hello"));
    }

    #[tokio::test]
    async fn shell_missing_command() {
        let tool = ShellCommand::new();
        let result = tool.execute(serde_json::json!({}), &test_ctx()).await;
        result.expect_err("missing command should fail");
    }

    #[tokio::test]
    async fn shell_needs_approval() {
        let tool = ShellCommand::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"command": "ls"}),
            &test_ctx()
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
        let args = ShellArgs::parse(
            serde_json::json!({
                "command": "printenv FOO",
                "shell": "/bin/sh",
                "env": [{"name": "FOO", "value": "bar"}]
            }),
            &test_ctx(),
        )
        .expect("parse args");

        assert_eq!(args.command, "printenv FOO");
        assert_eq!(args.shell, "/bin/sh");
        assert_eq!(args.env.len(), 1);
        assert_eq!(args.env[0].name, "FOO");
        assert_eq!(args.env[0].value, "bar");
    }

    #[test]
    fn parse_args_accepts_legacy_env_object() {
        let args = ShellArgs::parse(
            serde_json::json!({
                "command": "printenv FOO",
                "env": {"FOO": "bar"}
            }),
            &test_ctx(),
        )
        .expect("parse args");

        assert_eq!(args.env.len(), 1);
        assert_eq!(args.env[0].name, "FOO");
        assert_eq!(args.env[0].value, "bar");
    }

    #[test]
    fn parse_args_defaults_empty_shell_to_bin_sh() {
        let args = ShellArgs::parse(
            serde_json::json!({
                "command": "echo hello",
                "shell": ""
            }),
            &test_ctx(),
        )
        .expect("parse args");

        assert_eq!(args.shell, "/bin/sh");
    }

    /// Verifies Codex-compatible shell argument aliases are normalized.
    #[test]
    fn parse_args_accepts_command_and_workdir_aliases() {
        let args = ShellArgs::parse(
            serde_json::json!({
                "cmd": "pwd",
                "workdir": "/tmp"
            }),
            &test_ctx(),
        )
        .expect("parse args");

        assert_eq!(args.command, "pwd");
        assert_eq!(args.cwd, PathBuf::from("/tmp"));
    }

    /// Verifies shell argument deserialization errors include the offending field path.
    #[test]
    fn parse_args_reports_deserialization_error_path() {
        let error = ShellArgs::parse(
            serde_json::json!({
                "command": "echo hello",
                "yield_time_ms": "slow"
            }),
            &test_ctx(),
        )
        .err()
        .expect("invalid yield_time_ms should fail");

        assert!(error.contains("yield_time_ms"));
    }

    /// Verifies Codex-compatible write_stdin process id alias is normalized.
    #[test]
    fn parse_write_stdin_args_accepts_process_id_alias() {
        let args = WriteStdinArgs::parse(serde_json::json!({
            "process_id": 42,
            "chars": ""
        }))
        .expect("parse write_stdin args");

        assert_eq!(args.process_id, 42);
        assert_eq!(args.chars, "");
    }

    /// Verifies write_stdin deserialization errors include the offending field path.
    #[test]
    fn parse_write_stdin_args_reports_deserialization_error_path() {
        let error = WriteStdinArgs::parse(serde_json::json!({
            "session_id": "not-a-number"
        }))
        .err()
        .expect("invalid session_id should fail");

        assert!(error.contains("session_id"));
    }

    #[tokio::test]
    async fn shell_parameter_selects_shell_binary() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({
                    "command": "echo custom-shell-ok",
                    "shell": "/bin/sh"
                }),
                &test_ctx(),
            )
            .await
            .expect("shell output");

        assert!(result.contains("custom-shell-ok"));
    }

    #[test]
    fn truncate_utf8_boundary_does_not_panic() {
        let s = format!("{}{}", "a".repeat(2047), "你好世界");
        let result = truncate(&s, OUTPUT_MAX_LEN / 2);
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
        assert_eq!(truncate("hello", OUTPUT_MAX_LEN), "hello");
    }
}

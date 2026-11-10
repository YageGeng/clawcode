# Unified Exec 进程管理实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 codex 式的 `exec_command` + `write_stdin` 取代 `bash` 工具，后台进程由会话统一管理，并在 ACP/WebUI 中持续展示。

**Architecture:** `tools` crate 新增 `exec/` 模块，负责进程启动（PTY/管道）、输出缓冲、进程表与结果格式；Kernel 为每个会话持有一个 `ExecProcessManager`，注册两个 Kernel 工具，负责生命周期、事件、持久化与上下文注入；protocol/ACP/WebUI 增加 exec 事件、terminal 映射、扩展方法与后台终端列表。

**Tech Stack:** Rust（tokio、portable-pty 0.9、nix、typed-builder、serde）、ACP v2、React/TypeScript（WebUI）。

**Spec:** [Unified Exec 进程管理设计规格](../specs/2026-11-09-unified-exec-design.md)

## Global Constraints

- 不执行 `git commit`、不创建 worktree。每个任务结束时汇报改动与验证结果；commit 需用户明确允许，届时先跑 pre-commit（`fmt`、`clippy`、`rig-boundary`）并套用 `.gitmessage`。
- 新增和修改的函数必须有英文函数级注释；非平凡逻辑写英文注释。
- 超过 3 个字段的结构体用 `typed-builder` 并通过 builder 构造；`Option` 字段加 `#[builder(default)]`。
- 克隆 `Arc` 字段写 `Arc::clone(&x)`。
- 日志用 `tracing::info!("...", ...)` 全限定路径和正文格式，不用结构化字段；不记录命令输出、`chars`、环境变量。错误返回前打 `tracing::warn!`。
- 测试只放在 crate 的 `tests/` 目录或 `#[cfg(test)] mod tests`；前端不写单元测试；WebUI e2e 跑生产后端。
- 依赖在根 `[workspace.dependencies]` 声明，子 crate 用 `{ workspace = true }`，按语义分组、组内字母序。
- 共享上下文的函数挂在持有上下文的结构体上；转换用 `From`/`TryFrom`；不写一个参数、参数是项目类型的自由 helper。
- 工具名常量：`exec_command`、`write_stdin`。环境变量 `CLAWCODE_CI=1`。
- 数值：`yield_time_ms` 默认 10000、范围 250–30000（Windows 下限 10000）；`write_stdin` 写入默认 250、上限 30000，空轮询 5000 到 `background_terminal_max_timeout_ms`（默认 300000）；`max_output_tokens` 默认 10000、上限 262144；缓冲 1 MiB；进程上限 64、保护最近 8 个；PTY 24×80；输出快照节流 100 ms、尾部 2000 行或 50 KB；关闭时等待退出记录最多 2 秒；snapshot 生成限时 10 秒、文件保留 3 天。
- 不考虑历史会话迁移与兼容。

## Review Focus

1. **非 UTF-8 与 ANSI 输出**：进程输出 `\xff`、被切断的多字节字符或转义序列时，按有损解码处理，不能 panic，也不能丢掉后续输出。测试归任务 5。
2. **后台子进程占住管道**：`(sleep 30 &); echo done` 这类命令，父进程退出后要在有限时间内结束读取并报告退出，不能等到孙进程结束。测试归任务 5。
3. **大量输出**：几 MB 的输出，内存要保持在 1 MiB 缓冲之内，结果带截断标记和完整输出文件，事件按节流发送。测试归任务 6。
4. **并发轮询与重复轮询**：同一个 session 被并行的两个 `write_stdin` 轮询，或在取走结果后再次轮询时，只能得到一次输出和退出码，其余返回 `Unknown process id`，不能 panic。测试归任务 6。
5. **相对或不存在的 `workdir`**：相对路径按本轮 cwd 解析；不存在时返回错误，并且不分配编号。测试归任务 6。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/config/src/tools.rs` | `ExecConfig`、`ExecEnvPolicy`、`EnvInherit` |
| `crates/protocol/src/exec.rs`（新） | 工具名常量、`ExecExit`、`ExecProcessInfo` |
| `crates/protocol/src/{tool,event,session,identity}.rs` | `ToolResultDetails::Exec`、`ExecOutput`/`ExecExited` 事件、`SessionReplayItem::ExecExited`、三个扩展方法 |
| `crates/tools/src/exec/mod.rs`（新） | 常量、`ExecSettings`、`ExecEventSink`、`ExecError`、导出 |
| `crates/tools/src/exec/output.rs`（新） | `HeadTailBuffer`、`OutputBuffers`、`Captured` |
| `crates/tools/src/exec/request.rs`（新） | `ExecCommandArgs`、`WriteStdinArgs`、工具定义 |
| `crates/tools/src/exec/response.rs`（新） | `ExecResponse`：模型文本、截断、details |
| `crates/tools/src/exec/env.rs`（新） | `ExecEnvironment` |
| `crates/tools/src/exec/shell.rs`（新） | `UserShell` |
| `crates/tools/src/exec/process.rs`（新） | `SpawnSpec`、`ExecProcess`（PTY/管道） |
| `crates/tools/src/exec/manager.rs`（新） | `ExecProcessManager`、进程表、淘汰、流式与退出监视任务 |
| `crates/tools/src/exec/snapshot.rs`（新，第三期） | `ShellSnapshot` |
| `crates/tools/src/exec/sessions.rs`（新，第三期） | `CommandSessions` |
| `crates/kernel/src/runtime/exec/mod.rs`（新） | 注册、`SessionExecSink`、Kernel 查询与终止接口 |
| `crates/kernel/src/runtime/exec/tools.rs`（新） | `ExecCommandTool`、`WriteStdinTool` |
| `crates/kernel/src/runtime/exec/sessions.rs`（新，第三期） | `CommandSessionsNote` |
| `crates/acp/src/mapping.rs`、`server.rs`、`extension.rs`、`tool_call.rs` | exec 映射、重放、扩展方法 |
| `web/src/features/composer/BackgroundTerminals.tsx`（新） | 后台终端列表 |
| `crates/app/tests/web/exec.e2e.mjs`（新） | WebUI e2e |

---

## 第一期：引擎、工具、生命周期、替换 bash

### 任务 1：依赖与 `[tools.exec]` 配置

**Files:**
- Modify: `Cargo.toml`（`# pty` 组加 `portable-pty = "0.9"`）
- Modify: `crates/tools/Cargo.toml`（workspace crates 组加 `config`；`# pty` 组加 `portable-pty`；`# misc` 组加 `fastrand`）
- Modify: `crates/config/src/tools.rs`、`crates/config/src/lib.rs`
- Modify: `crates/app/src/lib.rs`（`ToolsConfig` 不再是 `Copy`，修正按值使用处）
- Test: `crates/config/tests/loading.rs`

**Interfaces:**
- Produces:
  - `config::ExecConfig { background_terminal_max_timeout_ms: u64, shell_snapshot: bool, command_sessions: bool, env: ExecEnvPolicy }`
  - `config::ExecEnvPolicy { inherit: EnvInherit, ignore_default_excludes: bool, exclude: Vec<String>, include_only: Vec<String>, set: BTreeMap<String, String> }`
  - `config::EnvInherit { All, Core, None }`（serde `lowercase`）
  - `ToolsConfig.exec: ExecConfig`
  - 都带 `#[serde(default)]`，`Default` 通过 builder 实现；默认值见 Global Constraints，`ignore_default_excludes` 默认 `true`。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn tools_exec_defaults_match_codex() {
    let value = serde_json::to_value(AppConfig::default()).expect("serialize");
    let exec = &value["tools"]["exec"];
    assert_eq!(exec["background_terminal_max_timeout_ms"], 300_000);
    assert_eq!(exec["shell_snapshot"], true);
    assert_eq!(exec["command_sessions"], true);
    assert_eq!(exec["env"]["inherit"], "all");
    assert_eq!(exec["env"]["ignore_default_excludes"], true);
}

#[test]
fn tools_exec_loads_env_policy() {
    let config: AppConfig = toml::from_str(r#"
[tools.exec.env]
inherit = "core"
ignore_default_excludes = false
exclude = ["*PROXY*"]
set = { FOO = "1" }
"#).expect("parse");
    assert_eq!(config.tools.exec.env.inherit, config::EnvInherit::Core);
    assert!(!config.tools.exec.env.ignore_default_excludes);
    assert_eq!(config.tools.exec.env.exclude, vec!["*PROXY*".to_string()]);
    assert_eq!(config.tools.exec.env.set.get("FOO").map(String::as_str), Some("1"));
}
```

- [ ] **Step 2：运行确认失败**：`cargo test -p config --test loading tools_exec`，预期编译失败（`exec` 字段不存在）。
- [ ] **Step 3：实现配置类型**，并在 `crates/config/src/lib.rs` 导出。
- [ ] **Step 4：运行确认通过**：`cargo test -p config --test loading tools_exec`，预期 PASS；`cargo check --workspace` 通过。

### 任务 2：protocol 类型

**Files:**
- Create: `crates/protocol/src/exec.rs`
- Modify: `crates/protocol/src/tool.rs`（新增 `ToolResultDetails::Exec`，**暂不删除** `Bash`）
- Modify: `crates/protocol/src/event.rs`、`crates/protocol/src/session.rs`、`crates/protocol/src/lib.rs`
- Modify: `crates/acp/src/mapping.rs`（新事件暂时走 `extension_update()`）、`crates/acp/src/server.rs`（`SessionReplayItem::ExecExited` 转成 `AgentEventPayload::ExecExited`）
- Test: `crates/protocol/tests/exec.rs`

**Interfaces:**
- Produces:
  - `protocol::EXEC_COMMAND_TOOL = "exec_command"`、`protocol::WRITE_STDIN_TOOL = "write_stdin"`
  - `protocol::ExecExit`（typed-builder，serde 默认 snake_case）：`tool_call_id: ToolCallId, turn_id: TurnId, process_id: i32, exit_code: Option<i32>, signal: Option<String>, output: String, truncation: Option<TruncationDetails>, full_output_path: Option<String>, wall_time_ms: u64, lost: bool`（`Option` 与 `lost` 加 `#[builder(default)]`、`#[serde(default)]`）
  - `protocol::ExecProcessInfo`（typed-builder）：`process_id: i32, command: String, cwd: PathBuf, tool_call_id: ToolCallId`
  - `ToolResultDetails::Exec { process_id: Option<i32>, exit_code: Option<i32>, signal: Option<String>, chunk_id: String, wall_time_ms: u64, original_token_count: usize, full_output_path: Option<String>, terminal_call_id: ToolCallId }`
  - `AgentEventPayload::ExecOutput { tool_call_id: ToolCallId, process_id: i32, output: String, truncation: Option<TruncationDetails> }`
  - `AgentEventPayload::ExecExited { exit: ExecExit }`
  - `SessionReplayItem::ExecExited { exit: ExecExit, timestamp_ms: TimestampMs }`

- [ ] **Step 1：写失败测试**：`exec_details_serialize_with_type_tag`，断言 `json["type"] == "exec"`、`json["process_id"] == 48213`、`json["terminal_call_id"] == "call-1"`；`exec_exited_event_round_trips`，断言 `json["event"] == "exec_exited"`，反序列化结果与原值相等，`lost` 缺省为 `false`。
- [ ] **Step 2：运行确认失败**：`cargo test -p protocol --test exec`，预期编译失败。
- [ ] **Step 3：实现类型与 ACP 临时分支。**
- [ ] **Step 4：运行确认通过**：`cargo test -p protocol --test exec` PASS；`cargo check --workspace --all-targets` 通过，用编译器找出所有穷尽 `match` 并补齐分支。

### 任务 3：输出缓冲、参数与结果格式

**Files:**
- Create: `crates/tools/src/exec/{mod,output,request,response}.rs`
- Modify: `crates/tools/src/lib.rs`（`mod exec; pub use exec::{...}`）
- Test: `crates/tools/tests/exec_output.rs`

**Interfaces:**
- Produces:
  - `exec::output::HeadTailBuffer::new(max_bytes: usize) -> Self`、`push(&mut self, chunk: &[u8])`、`take(&mut self) -> Captured`、`text(&self) -> String`。容量前一半保留开头、后一半保留结尾。
  - `Captured { bytes: Vec<u8>, omitted_bytes: usize }`，`Captured::text(&self) -> String`：有损 UTF-8 解码，在头尾之间插入 `... N bytes omitted ...`。
  - `OutputBuffers { pending: HeadTailBuffer, transcript: HeadTailBuffer }`，`push_chunk(&mut self, &[u8])` 同时写入两份。
  - `ExecCommandArgs { cmd, workdir: Option<PathBuf>, tty: bool, yield_time_ms: Option<u64>, max_output_tokens: Option<usize>, shell: Option<PathBuf>, login: bool }`：`TryFrom<&ToolCall>`（`login` 缺省为 `true`）、`yield_time(&self) -> Duration`、`max_output_tokens(&self) -> usize`、`definition() -> ToolDefinition`。
  - `WriteStdinArgs { session_id: i32, chars: String, yield_time_ms: Option<u64>, max_output_tokens: Option<usize> }`：`TryFrom<&ToolCall>`、`yield_time(&self, max_background: Duration) -> Duration`、`definition() -> ToolDefinition`。
  - `ExecResponse`（typed-builder）：`chunk_id: String, wall_time: Duration, process_id: Option<i32>, exit_code: Option<i32>, signal: Option<String>, output: Captured, max_output_tokens: usize, terminal_call_id: ToolCallId, full_output_path: Option<PathBuf>`；方法 `model_text(&self) -> String`、`details(&self) -> ToolResultDetails`、`is_error(&self) -> bool`（有退出码且不为 0，或有信号时为 `true`）。
  - 叶子算法 `fn truncate_middle(text: &str, max_tokens: usize) -> Option<(String, usize)>`：预算为 `max_tokens * 4` 字节，按字符边界保留头尾各一半，中间换成 `…{removed_tokens} tokens truncated…`；未超出预算返回 `None`。
  - 工具描述和参数描述逐字照抄 codex 的 `shell_spec.rs`，去掉 `environment_id` 和审批参数。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn head_tail_buffer_keeps_both_ends_and_counts_omitted() {
    let mut buffer = HeadTailBuffer::new(16);
    buffer.push(b"0123456789012345678901234567890123456789");
    let captured = buffer.take();
    assert_eq!(captured.omitted_bytes, 24);
    let text = captured.text();
    assert!(text.starts_with("01234567"));
    assert!(text.ends_with("23456789"));
    assert!(text.contains("... 24 bytes omitted ..."));
}

#[test]
fn response_header_for_running_process() {
    let text = response(Some(48213), None, "hi").model_text();
    assert!(text.starts_with("Chunk ID: "));
    assert!(text.contains("\nProcess running with session ID 48213\n"));
    assert!(text.contains("\nOriginal token count: 1\nOutput:\nhi"));
}
```

再写：
- `response_header_for_exited_process`：包含 `Process exited with code 3`，不包含 `session ID`。
- `response_truncates_middle_by_token_budget`：`max_output_tokens = 10`、400 字节输出，以 `Warning: truncated output (original token count: 100)` 开头，包含 `tokens truncated…`。
- `exec_args_clamp_yield_time`：在 `#[cfg(not(windows))]` 下，1 → 250 ms，99999 → 30000 ms，缺省 → 10000 ms。
- `write_args_clamp_yield_time`：空轮询 100 → 5000，999999 → `max_background`；非空缺省 → 250，99999 → 30000。
- `exec_args_require_cmd`：缺 `cmd` 时返回 `ToolError::InvalidArguments`。

`response(...)` 是测试文件内的构造函数。
- [ ] **Step 2：运行确认失败**：`cargo test -p tools --test exec_output`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p tools --test exec_output` PASS。

### 任务 4：环境变量与 shell 选择

**Files:**
- Create: `crates/tools/src/exec/{env,shell}.rs`
- Test: `crates/tools/tests/exec_env.rs`

**Interfaces:**
- Produces:
  - `ExecEnvironment<'a> { policy: &'a config::ExecEnvPolicy }`，`ExecEnvironment::new(policy) -> Self`，`build(&self, parent: impl IntoIterator<Item = (String, String)>, session_id: &SessionId, turn_id: &TurnId) -> BTreeMap<String, String>`。顺序：inherit → 默认排除（仅在 `!ignore_default_excludes` 时）→ exclude → set → include_only → 强制覆盖 → session/turn ID（`ProductIdentity::SESSION_ID_ENV`、`TURN_ID_ENV`）。
  - `core` 变量和强制覆盖值照抄 spec 4.6。模式只支持 `*`，不区分大小写。
  - `UserShell { path: PathBuf, kind: ShellKind }`，`ShellKind { Bash, Zsh, Sh, Other }`；`UserShell::detect(env_shell: Option<&str>) -> Self`（`$SHELL` → `/bin/bash` → `/bin/sh`）；`From<PathBuf> for UserShell`（按文件名判断 kind）；`argv(&self, cmd: &str, login: bool) -> Vec<String>`。

- [ ] **Step 1：写失败测试**：
  - `env_inherit_core_keeps_only_core_vars`：parent 含 `PATH`、`FOO`，`core` 下只剩 `PATH` 和强制、ID 变量。
  - `env_default_excludes_apply_only_when_not_ignored`：默认保留 `GITHUB_TOKEN`；`ignore_default_excludes = false` 时去掉 `GITHUB_TOKEN`、`AWS_SECRET_ACCESS_KEY`，保留 `PATH`。
  - `env_include_only_runs_after_set`：`set FOO=1` 加上 `include_only ["PATH"]`，结果里没有 `FOO`。
  - `env_patterns_are_case_insensitive_globs`：`exclude ["*proxy*"]` 去掉 `HTTP_PROXY`。
  - `forced_env_overrides_parent`：parent 里 `PAGER=less` 最终为 `cat`；`TERM == "dumb"`，`CLAWCODE_CI == "1"`，两个 ID 变量存在。
  - `shell_argv_uses_login_flag`：`["/bin/zsh", "-lc", "echo hi"]`，`login = false` 时为 `-c`。
- [ ] **Step 2：运行确认失败**：`cargo test -p tools --test exec_env`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p tools --test exec_env` PASS。

### 任务 5：进程启动（PTY 与管道）

**Files:**
- Create: `crates/tools/src/exec/process.rs`
- Test: `crates/tools/tests/exec_process.rs`（文件首行 `#![cfg(unix)]`）

**Interfaces:**
- Consumes: 任务 3 的 `OutputBuffers`。
- Produces:
  - `SpawnSpec`（typed-builder）：`argv: Vec<String>, cwd: PathBuf, env: BTreeMap<String, String>, tty: bool`。
  - `ExecProcess::spawn(spec: SpawnSpec) -> Result<ExecProcess, ExecError>`。
  - `ExecProcess` 方法：
    - `async fn write(&self, bytes: &[u8]) -> Result<(), ExecError>`：非 tty 返回 `ExecError::StdinClosed`；
    - `fn interrupt(&self) -> Result<(), ExecError>`：对进程组发 SIGINT；
    - `fn terminate(&self)`：对进程组发 SIGKILL，Windows 用 `taskkill /F /T`；
    - `fn state(&self) -> ProcessState`；
    - `fn subscribe_state(&self) -> watch::Receiver<ProcessState>`；
    - `fn buffers(&self) -> &Arc<Mutex<OutputBuffers>>`；
    - `fn output_notify(&self) -> &Arc<Notify>`；
    - `fn output_closed(&self) -> bool`；
    - `fn interaction_lock(&self) -> Arc<tokio::sync::Mutex<()>>`；
    - `fn is_tty(&self) -> bool`；
    - `impl Drop` 调用 `terminate()`。
  - `ProcessState { exited: bool, exit_code: Option<i32>, signal: Option<String> }`。
  - `ExecError`（在 `exec/mod.rs`，thiserror），消息照抄 codex：
    - `CreateProcess(String)` → `Failed to create unified exec process: {0}`
    - `ProcessFailed(String)` → `Unified exec process failed: {0}`
    - `UnknownProcessId(i32)` → `Unknown process id {0}`
    - `WriteToStdin` → `failed to write to stdin`
    - `StdinClosed` → `stdin is closed for this session; rerun exec_command with tty=true to keep stdin open`
    - `InvalidWorkdir(PathBuf)` → `Working directory does not exist: {0}`
    - `Cancelled`
  - `ExecError::into_tool_error(self, tool: &str) -> ToolError`：`Cancelled` → `ToolError::Cancelled`，其余 → `ToolError::Execution { tool, message: self.to_string() }`。不实现 `From`，因为错误要带上调用它的工具名。
- 管道模式在 `pre_exec` 旁加注释：`// shortcut: on macOS a non-tty child that never writes survives a parent SIGKILL as an orphan; record pids and reap them at startup if that becomes a problem.`
- 管道：stdin 为 `Stdio::null()`；`pre_exec` 里调用 `nix::unistd::setsid()`，`EPERM` 时退回 `setpgid(0,0)`；Linux 上加 `prctl(PR_SET_PDEATHSIG, SIGTERM)`；stdout、stderr 两个 reader 写入同一个 `OutputBuffers`；进程退出后最多再读 500 ms，然后关闭输出。
- PTY：`portable_pty::native_pty_system().openpty(PtySize { rows: 24, cols: 80, .. })`；子进程本身就是进程组组长（portable-pty 在子进程里已调用 `setsid` 与 `TIOCSCTTY`），进程组 ID 等于 `child.process_id()`；读、写、等待各用 `spawn_blocking`。
- 信号名照现有 `BashExecutor::signal_name` 的方式取得。

- [ ] **Step 1：写失败测试**：
  - `pipe_process_reports_exit_code_and_output`：`printf hi; exit 3`。
  - `pipe_process_has_no_controlling_terminal`：`read x </dev/tty; echo rc=$?`，2 秒内结束，输出含 `rc=1`。
  - `terminate_kills_the_whole_process_group`：`sleep 300 & echo $! > <tmp>/pid; wait`，`terminate()` 后 2 秒内 `kill(pid, 0)` 返回 `ESRCH`。
  - `pty_process_echoes_written_input`：tty 跑 `cat`，`write(b"hello\n")` 后 2 秒内输出含 `hello`。
  - `pty_is_a_terminal`：tty 跑 `[ -t 0 ] && echo yes`，输出含 `yes`。
  - `pipe_process_rejects_stdin_writes`：返回 `Err(ExecError::StdinClosed)`。
  - `interrupt_sends_sigint_to_group`：`trap 'echo got-int; exit 130' INT; sleep 30 & wait`，输出含 `got-int`，`exit_code == Some(130)`。
  - Review Focus 1，`invalid_utf8_output_is_lossy_not_fatal`：`printf '\xff\xfe ok'`，文本含 `ok` 和 `\u{FFFD}`。
  - Review Focus 2，`background_child_holding_pipes_does_not_block_exit`：`(sleep 30 &); echo done`，2 秒内 `state().exited`，输出含 `done`。
- [ ] **Step 2：运行确认失败**：`cargo test -p tools --test exec_process`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p tools --test exec_process` PASS。

### 任务 6：进程管理器

**Files:**
- Create: `crates/tools/src/exec/manager.rs`
- Modify: `crates/tools/src/exec/mod.rs`（`ExecSettings`、`ExecEventSink`、`ExecOutputUpdate`）
- Test: `crates/tools/tests/exec_manager.rs`

**Interfaces:**
- Consumes: 任务 3、4、5。
- Produces:
  - `ExecSettings { config: config::ExecConfig, snapshot_dir: PathBuf }`。
  - `#[async_trait] trait ExecEventSink: Send + Sync { async fn output(&self, update: ExecOutputUpdate); async fn exited(&self, exit: protocol::ExecExit); }`。
  - `ExecOutputUpdate`（typed-builder）：`tool_call_id: ToolCallId, turn_id: TurnId, process_id: i32, output: String, truncation: Option<TruncationDetails>`。
  - `ExecStart`（typed-builder）：`args: ExecCommandArgs, cwd: PathBuf, tool_call_id: ToolCallId, turn_id: TurnId, session_id: SessionId, cancellation: CancellationToken, sink: Arc<dyn ExecEventSink>`。
  - `ExecWrite { args: WriteStdinArgs, cancellation: CancellationToken }`。
  - `ExecProcessManager` 方法：
    - `new(settings: Arc<ExecSettings>) -> Self`
    - `async fn exec_command(&self, start: ExecStart) -> Result<ExecResponse, ExecError>`
    - `async fn write_stdin(&self, write: ExecWrite) -> Result<ExecResponse, ExecError>`
    - `async fn list(&self) -> Vec<ExecProcessInfo>`：只列运行中的，按编号排序
    - `async fn terminate(&self, process_id: i32) -> bool`
    - `async fn terminate_all(&self) -> usize`：终止后最多等 2 秒，直到每个进程的退出监视任务完成上报
  - 叶子算法 `fn prune_candidate(meta: &[(i32, Instant, bool)]) -> Option<i32>`：最近使用的 8 个不选，其余按时间从旧到新，优先已退出的。
- 编号：`fastrand::i32(1000..100_000)`，跳过已分配的。Chunk ID：6 位十六进制。
- 每个进程有两个后台任务：
  - **流式任务**：每 100 ms 检查 transcript 是否变化，变化就调用 `sink.output`，输出取 `truncate_tail(.., DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)`；
  - **退出监视任务**：等进程退出且输出关闭，再等首次 `exec_command` 的收集结束（记录上的“首次等待中”标记清除）。如果首次调用的结果已经报告了退出，就不调用 `sink.exited`；否则（转入后台，或首次等待被取消）调用一次 `sink.exited`。最后通知 `reported`。
- **写入时进程已退出**：tty 写入失败但 `state().exited` 为真时，忽略写入错误，照常收集剩余输出与退出码。
- **完整输出文件**：模型文本被截断，或退出快照被截断时，把对应文本写入 `tempfile`，前缀沿用 `OutputAccumulator` 的做法，并保留文件。
- **`write_stdin` 拿到退出时**：在交互锁内，用 `Arc::ptr_eq` 确认还是同一个进程，然后从表中删除，后续请求返回 `UnknownProcessId`。
- **日志**：启动、转入后台、退出、淘汰、`terminate_all` 数量，按 spec 第 11 节。

- [ ] **Step 1：写失败测试**（测试内定义 `RecordingSink`，记录 `output` 与 `exited` 调用）：
  - `short_command_returns_exit_code_without_session_id`：`echo hi`，`process_id == None`，`exit_code == Some(0)`，`model_text` 含 `hi`。
  - `long_command_yields_session_id_and_keeps_running`：`sleep 1; echo late`，yield 250，`process_id.is_some()`；`write_stdin` 空轮询 yield 5000，输出含 `late`、`exit_code == Some(0)`、`process_id == None`；再次轮询返回 `Err(UnknownProcessId(_))`。
  - `exited_background_process_waits_for_collection`：`sleep 0.3; echo x`，yield 250，等 1 秒后 `list()` 为空，但 `write_stdin` 仍拿到 `x` 和退出码。
  - `cancelled_wait_keeps_process`：100 ms 后取消，返回 `Err(Cancelled)`，`list()` 里有这个进程。
  - `sink_receives_output_snapshots_and_one_exit`：`for i in 1 2 3; do echo tick; sleep 0.2; done`，yield 250，结果带 session ID；至少一次 `output` 含 `tick`；正好一次 `exited`，`tool_call_id` 与 `ExecStart` 一致，`exit_code == Some(0)`。
  - `quick_exit_reports_through_result_only`：`echo hi` 的结果带退出码，sink 收到 0 次 `exited`。
  - `cancelled_process_reports_exit_later`：首次等待被取消的 `sleep 0.3`，之后 sink 收到一次 `exited`。
  - `write_after_tty_exit_still_collects`：tty 跑 `echo bye`，yield 250 后（进程已退出、尚未取结果）写入 `"x\n"`，结果含 `bye` 与退出码，不返回错误。
  - `write_stdin_ctrl_c_interrupts_non_tty`：`sleep 30` 后写 `"\u{3}"`，轮询拿到退出（信号 `SIGINT` 或退出码 130）。
  - `write_stdin_text_to_non_tty_is_rejected`：错误文本等于 `StdinClosed` 的消息。
  - `prune_candidate_protects_eight_most_recent_and_prefers_exited`：10 条 meta，最旧的两条中一条已退出，返回已退出那条；全部运行时返回最旧一条；只有 8 条时返回 `None`。
  - `terminate_all_reports_every_exit`：3 个 `sleep 30`（yield 250，均转入后台），返回 3，sink 收到 3 次 `exited`。
  - Review Focus 3，`flood_output_stays_bounded`：`head -c 3000000 /dev/zero | tr '\0' a`，`model_text` 含 `tokens truncated`，`full_output_path.is_some()`，`output` 回调次数 ≤ 运行毫秒数 ÷ 100 + 2。
  - Review Focus 4，`concurrent_polls_return_output_once`：后台 `sleep 0.3; echo once` 并发两个轮询，合起来 `once` 只出现一次，退出码只出现一次，另一个返回 `Ok` 但无退出码，或返回 `UnknownProcessId`。
  - Review Focus 5，`relative_workdir_resolves_and_missing_is_error`：`workdir: "sub"` 时 `pwd` 以 `/sub` 结尾；`workdir: "missing"` 返回 `InvalidWorkdir`，且 `list()` 为空。
- [ ] **Step 2：运行确认失败**：`cargo test -p tools --test exec_manager`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p tools --test exec_manager` PASS。

### 任务 7：Kernel 接线与生命周期

**Files:**
- Create: `crates/kernel/src/runtime/exec/{mod,tools}.rs`
- Modify: `crates/kernel/src/runtime.rs`：`KernelFactory`/`Kernel` 加 `#[builder(default)] exec: Option<Arc<tools::ExecSettings>>`；`Session` 加 `#[builder(default)] exec: Option<Arc<ExecProcessManager>>`；`mod exec;`
- Modify: `crates/kernel/src/runtime/session/assembly.rs`：`kernel.exec` 为 `Some` 时构造 manager，并在 `register_todo_write` 后调用 `crate::runtime::exec::register(&runtime, kernel)?`
- Modify: `crates/kernel/src/runtime/tools/state.rs`：`RESERVED_TOOL_NAMES` 加入两个工具名（`[&str; 4]`）
- Modify: `crates/kernel/src/runtime/session/close.rs`：`acquire_close()` 返回后调用 `terminate_all()`，并打 `info` 记录数量
- Modify: `crates/app/src/lib.rs`：`enable_shell` 为真时构造 `ExecSettings { config: snapshot.tools.exec.clone(), snapshot_dir: data_root.join("shell_snapshots") }` 传给 `.exec(Some(..))`
- Modify: `crates/app/src/main.rs`：两种 transport 停止后调用 `application.kernel.terminate_exec_processes().await`
- Modify: `crates/kernel/tests/subagent/fixture.rs`：`with_factories` 传入 `exec`（`shell_snapshot = false`，`snapshot_dir = root/shell_snapshots`）
- Test: `crates/kernel/tests/exec/main.rs`（`#[path = "../subagent/fixture.rs"] mod fixture;`）

**Interfaces:**
- Consumes: 任务 6。
- Produces（`impl Kernel`，写在 `runtime/exec/mod.rs`）：
  - `pub async fn exec_processes(&self, session_id: &SessionId) -> Result<Vec<ExecProcessInfo>, KernelError>`
  - `pub async fn terminate_exec_process(&self, session_id: &SessionId, process_id: i32) -> Result<bool, KernelError>`
  - `pub async fn clean_exec_processes(&self, session_id: &SessionId) -> Result<usize, KernelError>`
  - `pub async fn terminate_exec_processes(&self) -> usize`
  - 会话没有 manager 时分别返回空列表、`false`、`0`。
- `SessionExecSink { session: Weak<Session>, kernel: Kernel, session_id: SessionId }` 实现 `ExecEventSink`：用 `kernel.observer_emitter(..)` 取 emitter，以 `ExecExit.turn_id` 或启动该进程的 turn 发 `ExecOutput`/`ExecExited`。会话已释放时只打 `debug`。本任务不写持久化，留给任务 10。
- `ExecCommandTool { session: Weak<Session>, sink: Arc<SessionExecSink> }`、`WriteStdinTool { session: Weak<Session> }`：
  - 实现 `AgentTool`：`definition()` 用任务 3 的定义；`validate()` 只做 `TryFrom`；
  - `execute()` 返回 `ToolResult { blocks: [Text(model_text)], is_error, details: Some(details()) }`；
  - `prompt_contribution` 的 snippet 分别为 `Run shell commands; long-running ones keep running and return a session ID` 与 `Poll or send input to a running exec_command session`。

- [ ] **Step 1：写失败测试**：
  - `exec_command_runs_and_reports_exit`：route 先发 `exec_command {"cmd":"echo hi"}` 再给出答案，工具结果文本含 `Process exited with code 0` 与 `hi`。
  - `background_process_survives_turn_and_is_pollable`：第一轮 `sleep 1; echo later`、yield 250，结果含 `Process running with session ID`；第二轮 route 从上一条工具结果解析编号，调用 `write_stdin`，结果含 `later`。
  - `interrupt_keeps_background_process`：`sleep 30`、yield 30000，取消该 run 后 `exec_processes` 里有它；`clean_exec_processes` 返回 1。
  - `close_session_terminates_processes`：`echo $$ > <tmp>/pid; exec sleep 300`，`close_session` 后该 pid 已退出。
  - `background_exit_emits_exec_exited_event`：后台进程退出后，`sinks.payloads(session)` 里有 `ExecExited`，`exit.tool_call_id` 等于发起的调用；`echo hi` 这种首次等待内退出的调用没有 `ExecExited`。
  - `subagent_sessions_have_separate_process_tables`：父会话的进程不出现在子会话的 `exec_processes` 里。
  - `extension_cannot_register_exec_command`：同名扩展工具被拒绝，模型看到的是 Kernel 版本。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test exec`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test exec` PASS；`cargo check --workspace --all-targets` 通过。

### 任务 8：删除 bash、名字迁移与 plan 门控

**Files:**
- Delete: `crates/tools/src/builtin/bash.rs`
- Modify: `crates/tools/src/builtin/mod.rs`（删除 shell 组和 `shell_enabled`）、`crates/app/src/lib.rs`
- Modify: `crates/protocol/src/tool.rs`（删除 `ToolResultDetails::Bash`）
- Modify: `crates/acp/src/mapping.rs`（删除 bash 的 terminal 分支和 `bash_tool_end`；exec 暂走通用工具映射，第二期替换）、`crates/acp/src/projection.rs`（删除第 962 行附近按 `ToolResultDetails::Bash` 设置 `terminal:{id}` 快照键的分支）、`crates/acp/src/tool_call.rs`（两个工具名映射为 `Execute`）
- Modify: `crates/prompt/src/system.rs`（`has_bash` 改为判断 `exec_command`，文案为 `Use exec_command for file operations like ls, rg, find`）
- Modify: `extensions/src/available/command_guard.rs`（`exec_command` 的 `cmd`；`write_stdin` 的 `chars` 按 `\n` 拆行，每行套用同一个 `rejects` 检查）
- Modify: `crates/kernel/src/runtime/plan/gate.rs`（按名字的判断移到 `kernel_owned` 放行之前）
- Modify: `web/src/features/conversation/ToolCallCard.tsx`（`tool.title === "bash"` 改为 `"exec_command"`）
- Tests to migrate（`"bash"` 改为 `"exec_command"`、`"command"` 改为 `"cmd"`，断言改为新输出格式）：
  - `crates/kernel/tests/protocol_alignment/{plan_mode,tools,message_end,usage,queue}.rs`
  - `crates/kernel/tests/{tracing,extensions_host}.rs`
  - `crates/acp/tests/{mapping,v2_alignment}.rs`
  - `crates/prompt/tests/system_prompt.rs`
  - `crates/tools/tests/{builtins,registry}.rs`
  - `extensions/tests/command_guard.rs`
  - 任务 7 让测试夹具对所有会话注册 exec 工具，所以凡是断言工具列表或工具定义的测试（`protocol_alignment/tools.rs`、`crates/tools/tests/builtins.rs` 等）都要去掉 `bash`，并在 Kernel 工具列表中加入 `exec_command`、`write_stdin`。用 `cargo test --workspace` 的失败列表找全。
- `crates/tools/tests/bash.rs`：删除工具层测试（参数校验、文本格式）。执行器层的语义（退出码、信号、超时、取消、截断与落盘、部分更新、继承的 stdio）改为直接调用 `BashExecutor::execute`，保留在同一文件。

**Interfaces:**
- `PlanModeGate::check` 新规则：
  - `exec_command` 的 `cmd` 走 `ShellCommandLine::read_only`；
  - `write_stdin` 只放行 `chars` 为空或等于 `"\u{3}"`，否则返回 `Plan mode is active: write_stdin may only poll or interrupt.`。

- [ ] **Step 1：写失败测试**：
  - `crates/kernel/tests/protocol_alignment/plan_mode.rs` 新增 `plan_mode_limits_write_stdin_to_poll_and_interrupt`：写 `"y\n"` 时结果为错误且含上面的提示；空轮询照常执行。
  - 同文件已有的 bash 白名单测试改为 `exec_command`：`ls` 放行，`rm` 被拒。
  - `extensions/tests/command_guard.rs` 改为对 `exec_command {"cmd": "rm -rf /"}` 断言 Block；新增 `write_stdin_lines_are_guarded`：`write_stdin {"session_id": 1000, "chars": "ls\nrm -rf /\n"}` 被 Block，`{"chars": "y\n"}` 放行。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test protocol_alignment plan_mode`。
- [ ] **Step 3：实现删除与迁移。**
- [ ] **Step 4：运行确认通过**：`cargo test --workspace` 全部 PASS；`rg -n '"bash"' crates extensions web/src` 只剩用户 `!` 命令相关的位置（`BashExecution`、`UserBash`）；`npm --prefix web run check` 通过。

### 任务 9：`/ps` 与 `/stop`

**Files:**
- Modify: `crates/kernel/src/runtime/command/builtin.rs`
- Test: `crates/kernel/tests/slash_commands.rs`

**Interfaces:**
- Consumes: 任务 7 的 `exec_processes`、`clean_exec_processes`。
- `BuiltinSlashCommand` 新增 `Ps`（`list background terminals`）和 `Stop`（`stop all background terminals`）；`definitions()` 返回 5 个；`resolve("clean")` 返回 `Stop`，但定义里不列出 `clean`。
- `/ps` 输出：没有进程时为 `No background terminals.`，否则第一行为 `Background terminals:`，之后每行 `  {id}  {command}  ({cwd})`，command 截到 80 个字符。
- `/stop` 输出：`Stopped N background terminals`。

- [ ] **Step 1：写失败测试**：
  - `ps_lists_running_background_terminals`：启动 `sleep 30` 后执行 `/ps`，输出含编号和 `sleep 30`。
  - `stop_terminates_all_and_clean_is_an_alias`：`/clean` 输出 `Stopped 1 background terminals`，之后 `/ps` 输出 `No background terminals.`。
  - 更新已有的定义数量断言。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test slash_commands`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test slash_commands` PASS。

**第一期验收**：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check` 全部通过。用 `target/debug/clawcode serve` 手工确认：模型启动 `sleep 20; echo done` 后 turn 能结束，`/ps` 能列出它，`/stop` 能终止它。

---

## 第二期：卡片持续刷新与持久化

### 任务 10：`exec_exit` 持久化、`lost` 补记与重放项

**Files:**
- Modify: `crates/kernel/src/runtime/session/transcript.rs`：`append_store_only_entry` 由 `pub(super)` 放宽为 `pub(in crate::runtime)`
- Modify: `crates/kernel/src/runtime/exec/mod.rs`：`SessionExecSink::exited` 先 `append_store_only_entry`，再发事件
- Modify: `crates/kernel/src/runtime/session/replay.rs`：`session_replay` 遇到 `exec_exit` 条目时产出 `SessionReplayItem::ExecExited`
- Modify: `crates/kernel/src/runtime/session/assembly.rs`：注册 exec 后调用 `runtime.reconcile_lost_exec()?`
- Test: `crates/kernel/tests/exec/main.rs`

**Interfaces:**
- `ExecExitEntry(ExecExit)` 实现：
  - `From<&ExecExitEntry> for serde_json::Value`，payload 为 `{"extensionId": ProductIdentity::SLUG, "customType": "exec_exit", "data": <ExecExit>}`；
  - `TryFrom<&SessionEntry> for ExecExitEntry`，类型不匹配时返回 `Err(())`。
- `Session::reconcile_lost_exec(&self) -> Result<(), KernelError>`（写在 `runtime/exec/mod.rs` 的 `impl Session`）：扫描当前分支，找出 `ToolResultDetails::Exec { process_id: Some(_) }` 且之后没有同 `terminal_call_id` 的 `exec_exit` 的调用，各追加一条 `lost: true`、`output: ""` 的条目。

- [ ] **Step 1：写失败测试**：
  - `exec_exit_is_persisted_and_replayed`：后台进程退出后，会话文件里有 `customType == "exec_exit"`；`kernel.session_replay` 里有 `ExecExited`，`exit_code == Some(0)`。
  - `reopened_session_marks_unfinished_processes_lost`：后台 `sleep 300` 运行中关闭会话，然后从会话 JSONL 中删掉所有 `customType == "exec_exit"` 的行，模拟崩溃时没写出退出记录（直接改文件的方式同 `plan_mode.rs` 的 `append_custom_entry`）；`resume_session` 后，重放中该调用有 `lost == true` 的 `ExecExited`；再关闭、打开一次，不会重复追加。
  - `exec_exit_entries_stay_out_of_model_context`：之后一次模型请求的消息里不含 `exec_exit`。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test exec exec_exit`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test exec` PASS。

### 任务 11：ACP terminal 映射

**Files:**
- Modify: `crates/acp/src/mapping.rs`、`crates/acp/src/terminal.rs`（`ExecResultBody`）、`crates/acp/src/projection.rs`（`ExecOutput` 的 `snapshot_of` 为 `terminal:{tool_call_id}`）
- Test: `crates/acp/tests/exec_mapping.rs`

**Interfaces:**
- 规则照 spec 6.3，包括“顺序无关”。
- 新增 `AcpEventMapper::exec_tool_end(..)`，供 `ToolExecutionEnd` 与 `MessageEnd(ToolResult)` 共用：
  - 结果为 `process_id: Some` 时：`ToolCallUpdate` 不带 status，`content = [terminal(terminal_call_id)]`，带 `raw_output`，不写 terminal；
  - 结果为已退出时：`TerminalUpdate` 写入正文并带 `exit_status`（用 details 的 `exit_code`、`signal`），status 按 `ToolStatusOf`；有 `full_output_path` 时追加 `Full output: {path}`。
- 新增 `ExecResultBody<'a>(&'a [ContentBlock])` 并实现 `From<ExecResultBody<'_>> for String`，写在 `crates/acp/src/terminal.rs`，与 `TerminalText` 同一写法：返回文本块中第一个 `\nOutput:\n` 之后的部分；文本以 `\nOutput:` 结尾时返回空串；找不到时返回全文。
- `ExecExited` 映射为两条更新：
  - `TerminalUpdate`：最终输出 + `exit_status`；
  - `ToolCallUpdate`：status 为 `exit_code == Some(0) && !lost` 时 `completed`，否则 `failed`；`lost` 时追加 `Content` 文本 `进程已不在本会话中运行（服务重启或会话分叉）`；有 `full_output_path` 时追加 `Full output: {path}`。
- `write_stdin` 卡片：`ToolExecutionStart` 时 content 为文本 `向 session {id} 发送：{chars}` 或 `等待 session {id} 输出`（`chars` 里的控制字符按 `escape_debug` 显示）；结束时只更新 status 和 `raw_output`。

- [ ] **Step 1：写失败测试**（直接驱动 `AcpEventMapper`，构造方式同 `crates/acp/tests/mapping.rs`）：
  - `exec_start_opens_terminal_with_cmd`：输出 `TerminalUpdate`，`terminal_id == call id`，`command == cmd`。
  - `exec_output_replaces_terminal_output`。
  - `exited_result_closes_terminal_with_body`：结果文本为 `Chunk ID: a\nWall time: 0.1 seconds\nProcess exited with code 0\nOriginal token count: 1\nOutput:\nhi`，terminal 输出为 `hi`，`exit_status.exit_code == 0`，status 为 `completed`。
  - `background_result_sets_no_status`：`ToolCallUpdate.status.is_none()`，没有 `TerminalUpdate`。
  - `exec_exited_closes_terminal_and_sets_status`：退出码 0 为 `completed`，`lost` 为 `failed` 且含提示文本。
  - `exit_before_result_keeps_final_status`：依次映射 `ExecExited(0)` 和“转入后台”的结果，最终状态仍为 `completed`。
  - `write_stdin_card_shows_sent_chars_without_output`。
  - `replayed_exec_exit_maps_like_live`：由 `SessionReplayItem::ExecExited` 产生的更新与实时映射相同。
- [ ] **Step 2：运行确认失败**：`cargo test -p acp --test exec_mapping`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p acp` PASS。

### 任务 12：WebUI 卡片持续刷新与 e2e

**Files:**
- Modify: `web/src/features/conversation/ToolCallCard.tsx`：exec 卡片在 `in_progress` 时默认展开，turn 结束后继续显示 terminal
- Modify: `web/src/workspace/updateRouter.ts` 或 `sessionState.ts`：确认 run 结束后到达的 `terminal_update` 与 `tool_call_update` 不会被丢弃，若会被丢弃就修正
- Create: `crates/app/tests/web/exec.e2e.mjs`：仿照 `plan_mode.e2e.mjs`，跑生产 app 与 `claw.toml` 配置的 provider

e2e 场景 `card`：
1. 提示模型“用 exec_command 运行 `for i in 1 2 3 4 5; do echo tick-$i; sleep 2; done`，yield_time_ms 设为 1000，然后直接结束回复，不要轮询”。
2. 断言 turn 结束后（输入框可用），该卡片 terminal 的文本在 15 秒内出现 `tick-5`。
3. 断言卡片最终状态为完成，并显示退出码 0。

- [ ] **Step 1：写 e2e 脚本。**
- [ ] **Step 2：运行确认失败**：先跑 `cargo build -p app`，再跑 `node crates/app/tests/web/exec.e2e.mjs card`，预期失败。
- [ ] **Step 3：实现前端改动。**
- [ ] **Step 4：运行确认通过**：`npm --prefix web run check` 通过；`node crates/app/tests/web/exec.e2e.mjs card` PASS。

---

## 第三期：shell snapshot、`<command_sessions>`、扩展方法

### 任务 13：shell snapshot

**Files:**
- Create: `crates/tools/src/exec/snapshot.rs`
- Modify: `crates/tools/src/exec/shell.rs`：`argv` 增加 `snapshot: Option<&Path>, exports: &BTreeMap<String, String>` 两个参数
- Modify: `crates/tools/src/exec/manager.rs`：
  - 第一次 `exec_command` 时，若 `config.shell_snapshot` 为真且 shell 是 bash、zsh 或 sh，就 `tokio::spawn` 一次 `ShellSnapshot::capture`，写到 `snapshot_dir/<session_id>.sh`；生成状态存在 `tokio::sync::OnceCell<Option<PathBuf>>` 里，失败存 `None`。
  - 之后的 `exec_command` 在 `login` 为真且 snapshot 已就绪时改写命令；生成中或失败时用普通 `-lc`。
  - 新增 `async fn remove_snapshot(&self)`，删除已生成的文件。
- Modify: `crates/kernel/src/runtime/session/close.rs`：`terminate_all()` 之后调用 `exec.remove_snapshot().await`
- Modify: `crates/app/src/main.rs`：启动 transport 前 `tokio::spawn` 一次 `ShellSnapshot::cleanup_stale(&data_root.join("shell_snapshots"), 3 天)`
- Test: `crates/tools/tests/exec_snapshot.rs`（`#![cfg(unix)]`）

**Interfaces:**
- `ShellSnapshot { shell: UserShell, path: PathBuf }`
  - `async fn capture(&self, cwd: &Path, timeout: Duration) -> Result<(), ExecError>`：以 `-lic` 运行采集脚本，写文件，再用 `<shell> -c '. <path>'` 校验；`ShellKind::Other` 返回 `Err`。
  - `async fn cleanup_stale(dir: &Path, max_age: Duration)`。
- 采集内容：
  - bash：`declare -f`、`alias -p`、`shopt -p`、`set +o`、`export -p`
  - zsh：`functions`、`alias -L`、`setopt` 的结果加上 `setopt` 前缀、`export -p`
  - sh：`alias`、`set +o`、`export -p`
  - 脚本开头写 `unalias -a 2>/dev/null || true`。
- 改写结果：`[shell, "-c", ". '<path>'; export K='V' ...; exec '<shell>' -c '<cmd>'"]`。单引号按 `'\''` 转义；`exports` 为强制覆盖变量加上 session/turn ID。

- [ ] **Step 1：写失败测试**：
  - `snapshot_restores_functions_and_aliases`：临时 `HOME` 里放一个 `.bashrc`，定义函数 `greet() { echo hi-from-rc; }` 和 `alias ll='echo ll-alias'`；以 bash 生成 snapshot 后，改写后的 `greet; ll` 输出两行。
  - `snapshot_does_not_override_forced_env`：rc 里 `export PAGER=less`，改写后 `echo $PAGER` 输出 `cat`。
  - `argv_without_snapshot_is_plain_login`：没有 snapshot 时 argv 为 `-lc`。
  - `cleanup_removes_only_stale_files`：把一个文件的 mtime 设为 4 天前，另一个为新文件，只删掉旧的。
  - `capture_times_out`：rc 里 `sleep 30`，`timeout = 1s` 时返回 `Err`。
  - `snapshot_is_generated_on_first_exec_only`：manager 构造后、`exec_command` 前，`snapshot_dir` 为空；第一次 `exec_command` 后 10 秒内出现 `<session_id>.sh`；`remove_snapshot()` 后文件消失。
- [ ] **Step 2：运行确认失败**：`cargo test -p tools --test exec_snapshot`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p tools --test exec_snapshot` PASS；`cargo test -p kernel --test exec` PASS。

### 任务 14：`<command_sessions>` 注入

**Files:**
- Create: `crates/tools/src/exec/sessions.rs`、`crates/kernel/src/runtime/exec/sessions.rs`
- Modify: `crates/tools/src/exec/manager.rs`：`async fn sessions(&self) -> CommandSessions`
- Modify: `crates/kernel/src/runtime/session/transcript.rs`：`append_command_sessions(&self, entry_id: EntryId, note: &CommandSessionsNote) -> Result<(), KernelError>`，写法同 `append_mode_note`
- Modify: `crates/kernel/src/runtime/run/turn.rs`：在 `model_request` 读取 history 之前调用 `self.session.refresh_command_sessions(kernel, turn_id)`
- Modify: `crates/kernel/src/runtime/session/replay.rs`（`history_from_store` 的 `Custom` 分支）、`transcript.rs:436` 与 `replay.rs:419` 附近的压缩后处理：表非空时重新声明
- Test: `crates/kernel/tests/exec/main.rs`

**Interfaces:**
- `CommandSessions`（serde、`PartialEq`）：`sessions: BTreeMap<i32, CommandSessionState { exited: bool, in_use: bool }>`，`omitted: usize`；`fn render(&self) -> String` 按 spec 第 8 节原文输出；`Default` 为空。
- `CommandSessionsNote { sessions: CommandSessions, turn_id: TurnId }`：payload 为 `{"extensionId": SLUG, "customType": "command_sessions", "data": {...}}`；`from_entry(&SessionEntry) -> Option<Self>`；`message(&self, id: &str, timestamp) -> Result<AgentMessage, KernelError>`，生成隐藏的 `MessageContent::Extension`，`customType: "command_sessions"`。
- `Session::refresh_command_sessions`：
  - `config.command_sessions` 为假或没有 manager 时直接返回；
  - 上下文中最后一条 note 的 `sessions`，没有时视为 `Default`，与当前快照相同时直接返回；
  - 否则追加一条。

- [ ] **Step 1：写失败测试**：
  - `no_note_without_background_processes`：普通对话的请求里没有 `command_sessions` 消息。
  - `note_appears_when_process_backgrounds`：转入后台后，下一次请求的最后一条 Extension 消息含 `{id}: running`。
  - `note_not_repeated_when_unchanged`：再一轮请求只含一条 note。
  - `note_shows_exited_then_none`：进程退出后为 `exited; poll to collect`；`write_stdin` 取走后为 `None.`。
  - `note_replays_identically`：关闭再打开会话后，`history_from_store` 生成的上下文与关闭前一致。
  - `note_restated_after_compaction_when_nonempty`：压缩后上下文里仍有当前快照。
  - `reopen_declares_none`：有后台进程且已注入 note 后关闭会话（进程被终止），再打开，下一次请求追加 `None.`。
  - `config_false_disables_note`。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test exec note`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel` PASS。

### 任务 15：ACP 扩展方法

**Files:**
- Modify: `crates/protocol/src/identity.rs`：`ExecList`（`_clawcode/session/exec/list`，查询类）、`ExecTerminate`（`_clawcode/session/exec/terminate`）、`ExecClean`（`_clawcode/session/exec/clean`）；补全 `as_str`、`is_query`、`parse`
- Modify: `crates/acp/src/extension.rs`：新增 `AcpExecTerminateParameters { session_id, process_id }`（serde camelCase）；三个分支分别调用任务 7 的 Kernel 接口，返回 `{"processes": [...]}`、`{"terminated": bool}`、`{"terminated": n}`
- Modify: `crates/acp/src/server.rs`：注册三个方法
- Test: `crates/acp/tests/extension.rs`

- [ ] **Step 1：写失败测试**：
  - `exec_list_returns_running_processes`：`processes[0].processId` 与 `command` 正确。
  - `exec_terminate_stops_one_process`：返回 `terminated == true`，再次 list 为空。
  - `exec_clean_stops_all`：返回 `terminated == 2`。
  - `exec_methods_parse_and_classify`：`parse` 能往返，`ExecList.is_query()` 为真，另外两个为假。
- [ ] **Step 2：运行确认失败**：`cargo test -p acp --test extension exec`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`cargo test -p acp` PASS。

### 任务 16：WebUI 后台终端列表与 e2e

**Files:**
- Modify: `web/src/acp/extensions.ts`：新增 `execList`、`execTerminate`、`execClean`
- Modify: `web/src/workspace/controller.ts`：新增 `listExecProcesses(sessionId)`、`terminateExecProcess(sessionId, processId)`、`cleanExecProcesses(sessionId)`
- Modify: `web/src/workspace/selectors.ts`：`backgroundTerminalCount(session)` 统计 `title === "exec_command"` 的卡片中，`terminalId === toolCallId` 的 terminal 实体已存在、且没有 `exitStatus` 的数量。不看卡片状态，因此首次等待被 Esc 打断、显示 `cancelled` 的进程也会计入
- Create: `web/src/features/composer/BackgroundTerminals.tsx`：显示“N 个后台终端”。点开时调用 `listExecProcesses` 取列表，每项显示编号、命令和“终止”按钮，底部是“全部停止”；终止后重新取列表
- Modify: `web/src/features/composer/Composer.tsx`：放在 `PendingQueue` 旁边，数量为 0 时不渲染
- Modify: `crates/app/tests/web/exec.e2e.mjs`：新增场景 `badge`

e2e 场景 `badge`：
1. 让模型分别用 `exec_command` 启动 `sleep 300` 两次，`yield_time_ms` 设为 1000。
2. 断言输入框附近显示 `2 个后台终端`。
3. 点第一项的“终止”后显示 `1 个后台终端`。
4. 点“全部停止”后标记消失，两张卡片都变为失败并显示信号。

- [ ] **Step 1：写 e2e 场景。**
- [ ] **Step 2：运行确认失败**：`node crates/app/tests/web/exec.e2e.mjs badge`。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行确认通过**：`npm --prefix web run check` 通过；`node crates/app/tests/web/exec.e2e.mjs badge` 与 `card` PASS。

**第三期验收**：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`、`npm --prefix web run check` 全部通过，两个 e2e 场景通过。

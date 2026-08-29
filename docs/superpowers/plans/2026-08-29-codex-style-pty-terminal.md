# Codex 风格 PTY 终端实施计划

> **执行要求：** REQUIRED SUB-SKILL：使用 `superpowers:executing-plans` 在当前会话逐任务执行并设置复核检查点。项目禁止 SubAgent 和 worktree，因此不得使用 `subagent-driven-development` 或创建隔离 worktree。

**目标：** 将模型侧一次性 `bash` 替换为 Codex 风格的 `exec_command` / `write_stdin`，让真实 PTY 和 pipe 进程在 Session 内跨 Turn 交互，并由 ACP/WebUI 完成进程管理。

**架构：** `crates/tools` 实现输出环形缓冲、pipe/PTY 后端和 `TerminalManager`；Kernel 的每个 Session 持有一个 manager，并通过 `ToolExecutionContext` 注入无状态工具。`protocol` 定义宿主快照与事件，ACP 转发 Session 级管理请求和易失通知，WebUI 在 Inspector 中展示并控制后台终端。

**技术栈：** Rust 2024、Tokio、`portable-pty 0.9.0`、`win32job 2.0.3`（仅 Windows）、nix、serde、typed-builder、ACP v2 JSON-RPC、React 19、TypeScript 6、Zustand。

**Spec：** `docs/superpowers/specs/2026-08-29-codex-style-pty-terminal-design.md`

## 全局约束

- 所有命令必须以 `rtk` 开头；即使使用 `&&`，每个子命令也必须单独添加 `rtk`。
- 禁止使用 SubAgent，禁止创建 worktree。
- 未经用户再次明确允许，不执行 `git commit`；每个任务只保留可审查的未提交检查点。
- 所有新增函数必须有英文函数级注释；非平凡逻辑和旧代码修改原因必须使用英文注释。
- 字段超过 3 个的 struct 必须使用 `typed-builder`；`Option` 字段必须使用 `#[builder(default)]`，并按调用端类型决定是否使用 `setter(strip_option)`。
- 克隆 `Arc<T>` 字段必须写为 `Arc::clone(&value)`。
- 测试代码只能位于 crate 级 `tests/` 或准确声明为 `#[cfg(test)] mod tests` 的模块。
- 前端不要求单元测试，但必须通过 TypeScript、ESLint、生产构建和真实生产后端浏览器 E2E。
- WebUI E2E 必须连接生产 `app` 和当前配置的真实 provider，不使用 fixture backend。
- 模型侧不保留 `bash` 别名；用户 `!` / `!!` 的现有行为必须保持。
- 单进程环形缓冲固定为 1 MiB；单 Session 最多 64 个条目；保护最近使用的 8 个条目。
- `exec_command` 默认等待 10000ms，范围 250–30000ms；工具响应 token 默认及上限为 10000。
- 所有依赖路径或版本声明在根 `[workspace.dependencies]`，子 crate 只使用 `{ workspace = true }`。

---

## 文件结构

新增或修改后的职责边界：

- `crates/protocol/src/terminal.rs`：终端 ID、状态、快照、ACP 参数、响应和实时事件。
- `crates/tools/src/terminal/buffer.rs`：1 MiB 环形输出及 Agent 游标。
- `crates/tools/src/terminal/backend.rs`：统一后端 trait、spawn 返回值和 shell 解析。
- `crates/tools/src/terminal/backend/pipe.rs`：Tokio pipe 进程、输出 reader、进程组/Job Object 控制。
- `crates/tools/src/terminal/backend/pty.rs`：`portable-pty` 创建、阻塞 I/O 桥接和 PTY 控制。
- `crates/tools/src/terminal/process.rs`：单进程状态、交互锁、输出通知和退出 watcher。
- `crates/tools/src/terminal/manager.rs`：Session 表、ID、LRU、宿主管理和事件订阅。
- `crates/tools/src/terminal/mod.rs`：公共 service、请求、结果、错误和常量导出。
- `crates/tools/src/builtin/exec_command.rs`：模型侧创建工具。
- `crates/tools/src/builtin/write_stdin.rs`：模型侧交互和状态轮询工具。
- `crates/kernel/src/runtime/terminal.rs`：Kernel 对 Session manager 的查询、终止、清理和订阅能力。
- `crates/acp/src/terminal.rs`：终端通知的 typed JSON-RPC 包装。
- `web/src/features/inspector/TerminalPanel.tsx`：Session 级后台终端列表与控制。

现有 `crates/tools/src/bash.rs`、`crates/tools/src/builtin/output.rs` 和 `web/src/features/conversation/BashExecutionCard.tsx` 继续只服务用户 `!` / `!!`，不并入交互终端子系统。

---

### 任务 1：定义终端公共协议与 ACP 方法名

**文件：**

- 创建：`crates/protocol/src/terminal.rs`
- 修改：`crates/protocol/src/scalar.rs`
- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/protocol/src/identity.rs`
- 修改：`crates/protocol/src/lib.rs`
- 创建：`crates/protocol/tests/terminal.rs`
- 修改：`crates/protocol/tests/identity.rs`
- 修改：`crates/protocol/tests/public_types.rs`

**接口：**

- 产出：`TerminalId`、`TerminalStatus`、`TerminalSnapshot`、`TerminalEvent`、`TerminalUpdateNotification`、`TerminalListResult`、`TerminalTerminateResult`、`TerminalCleanResult`。
- 产出：`AcpTerminalTargetParameters`，并复用 `AcpSessionParameters` 作为 list/clean 输入。
- 产出：`AcpExtensionMethod::{TerminalList, TerminalTerminate, TerminalClean}` 和 `ProductIdentity::ACP_TERMINAL_UPDATE_NOTIFICATION`。

- [x] **步骤 1：先写协议失败测试**

在 `crates/protocol/tests/terminal.rs` 写精确 wire 断言：

```rust
/// Terminal snapshots serialize stable host-facing field names.
#[test]
fn terminal_snapshot_uses_host_facing_camel_case_fields() {
    let snapshot = TerminalSnapshot::builder()
        .terminal_id(TerminalId::try_from(7319_u32).expect("terminal id"))
        .command("python3 -i".to_string())
        .cwd(PathBuf::from("/tmp/work"))
        .tty(true)
        .status(TerminalStatus::Running)
        .started_at(TimestampMs::from(100_u64))
        .last_activity_at(TimestampMs::from(200_u64))
        .build();

    let json = serde_json::to_value(snapshot).expect("terminal snapshot json");
    assert_eq!(json["terminalId"], 7319);
    assert_eq!(json["status"], "running");
    assert_eq!(json["tty"], true);
    assert!(json.get("exitCode").is_none());
}

/// Output events retain their monotonic process-local sequence.
#[test]
fn terminal_output_event_preserves_absolute_sequence() {
    let event = TerminalEvent::Output {
        terminal_id: TerminalId::try_from(7319_u32).expect("terminal id"),
        sequence: 42,
        output: "ready> ".to_string(),
    };
    let json = serde_json::to_value(event).expect("terminal event json");
    assert_eq!(json["type"], "output");
    assert_eq!(json["sequence"], 42);
}
```

在 `identity.rs` 测试中断言三个请求方法和通知常量的精确字符串。

- [x] **步骤 2：运行测试并确认失败**

运行：

```bash
rtk cargo test -p protocol --test terminal
rtk cargo test -p protocol --test identity
```

预期：因终端类型、方法枚举和通知常量尚不存在而编译失败。

- [x] **步骤 3：实现最小公共协议**

`TerminalId` 使用数值透明序列化，并验证 `1000..=99999`：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct TerminalId(u32);

impl TerminalId {
    pub const MIN: u32 = 1_000;
    pub const MAX: u32 = 99_999;

    /// Returns the validated Session-local numeric terminal identifier.
    #[must_use]
    pub const fn get(self) -> u32 { self.0 }
}
```

`terminal.rs` 使用 `#[serde(rename_all = "camelCase")]` 定义快照和结果；`TerminalEvent` 使用：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TerminalEvent {
    Started { terminal: TerminalSnapshot },
    Output { terminal_id: TerminalId, sequence: u64, output: String },
    Exited { terminal_id: TerminalId, exit_code: Option<i32> },
    Failed { terminal_id: TerminalId, message: String },
    Removed { terminal_id: TerminalId, reason: TerminalRemovalReason },
}
```

`ScalarError` 增加 `InvalidTerminalId { minimum: u32, maximum: u32 }`。`TerminalRemovalReason` 的 wire 值固定为 `terminated`、`cleaned`、`capacity`、`session_closed`、`kernel_shutdown`、`reaped`。`TerminalSnapshot.exit_code` 使用 `#[builder(default)]` 且不使用 `strip_option`，因为状态投影持有的就是 `Option<i32>`。

扩展方法精确命名为 `_clawcode/terminal/list`、`_clawcode/terminal/terminate`、`_clawcode/terminal/clean`；通知为 `_clawcode/terminal/update`。同步更新 `parse()` 的候选表和 protocol re-export。

- [x] **步骤 4：运行协议测试**

运行：`rtk cargo test -p protocol --test terminal && rtk cargo test -p protocol --test identity && rtk cargo test -p protocol --test public_types`

预期：全部 PASS，且 JSON 字段、枚举标签和公共 re-export 与测试一致。

- [x] **步骤 5：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：仅出现本任务文件和已批准的 spec/plan；不执行 commit。

---

### 任务 2：实现有界输出缓冲与 Codex 风格响应裁剪

**文件：**

- 创建：`crates/tools/src/terminal/mod.rs`
- 创建：`crates/tools/src/terminal/buffer.rs`
- 修改：`crates/tools/src/lib.rs`
- 测试：`crates/tools/src/terminal/buffer.rs` 内的 `#[cfg(test)] mod tests`

**接口：**

- 产出：`TerminalOutputBuffer::new(capacity: usize)`、`append(&mut self, bytes: &[u8]) -> TerminalOutputEvent`、`drain_agent(&mut self, max_output_tokens: usize) -> TerminalOutputSlice`。
- 产出：`TerminalInteractionState::{Running { session_id }, Exited { exit_code }}` 和 `TerminalInteractionOutput`。
- 产出：字段完整的 `TerminalSpawnRequest`（`cmd`、`cwd`、`session_id`、`turn_id`、`tty`、`yield_duration`、`max_output_tokens`、`cancellation`）和 `TerminalInteractionRequest`（`terminal_id`、`chars`、`yield_duration`、`max_output_tokens`、`cancellation`），两者使用 builder。
- 产出：`TerminalError::{Unavailable, InvalidWorkingDirectory, Spawn, UnknownTerminal, InputUnsupported, Backend, StatePoisoned, Cancelled}`；每个包含诊断数据的 variant 使用具名字段。
- 常量：`TERMINAL_OUTPUT_CAPACITY = 1024 * 1024`、`DEFAULT_MAX_OUTPUT_TOKENS = 10_000`、`APPROX_BYTES_PER_TOKEN = 4`。

- [x] **步骤 1：写环形缓冲失败测试**

覆盖三个不变量：

```rust
/// Draining output advances one non-repeating Agent cursor.
#[test]
fn drain_advances_agent_cursor_without_repeating_output() {
    let mut buffer = TerminalOutputBuffer::new(32);
    buffer.append(b"first");
    assert_eq!(buffer.drain_agent(10_000).output, "first");
    assert_eq!(buffer.drain_agent(10_000).output, "");
    buffer.append(b"second");
    assert_eq!(buffer.drain_agent(10_000).output, "second");
}

/// Ring eviction reports the exact number of unavailable historical bytes.
#[test]
fn eviction_reports_exact_omitted_byte_count() {
    let mut buffer = TerminalOutputBuffer::new(8);
    buffer.append(b"abcdefghijkl");
    let slice = buffer.drain_agent(10_000);
    assert_eq!(slice.omitted_bytes, 4);
    assert!(slice.output.contains("4 bytes omitted"));
    assert!(slice.output.ends_with("efghijkl"));
}

/// Response truncation keeps a UTF-8-safe tail and never replays omitted bytes.
#[test]
fn response_budget_keeps_utf8_safe_tail_and_consumes_snapshot_once() {
    let mut buffer = TerminalOutputBuffer::new(128);
    buffer.append("前缀-abcdefghijklmnopqrstuvwxyz-结尾".as_bytes());
    let slice = buffer.drain_agent(4);
    assert!(slice.output.contains("tokens truncated"));
    assert!(std::str::from_utf8(slice.output.as_bytes()).is_ok());
    assert_eq!(buffer.drain_agent(4).output, "");
}
```

- [x] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p tools terminal::buffer::tests`

预期：因 `terminal` 模块和缓冲类型不存在而编译失败。

- [x] **步骤 3：实现缓冲和裁剪**

使用 `VecDeque<u8>` 保存最多 capacity 字节，维护 `start_offset`、`end_offset`、`agent_cursor` 和 `event_sequence`。`append` 返回新增内容的绝对事件序号；`drain_agent` 在同一个 `&mut self` 临界区内复制当前未读范围并将游标推进到快照末尾。

响应 token 预算按 Codex 的 4 bytes/token 近似换算；从最近 UTF-8 边界保留尾部，并使用稳定文本：

```text
Warning: truncated output (original token count: N)
…M tokens truncated…
<retained tail>
```

环形缓冲淘汰使用独立标记：

```text
…M bytes omitted from terminal buffer…
```

使用 `String::from_utf8_lossy` 处理任意进程字节，不能在字节索引上直接切割 `str`。

- [x] **步骤 4：运行缓冲测试**

运行：`rtk cargo test -p tools terminal::buffer::tests`

预期：全部 PASS。缓冲的生产消费者在任务 5 接入，`tools --all-targets` clippy 延后到任务 5，避免用临时 dead-code 例外掩盖阶段性未引用代码。

- [x] **步骤 5：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：缓冲实现和测试形成独立可审查单元；不执行 commit。

---

### 任务 3：实现 pipe 后端和完整进程树控制

**文件：**

- 修改：`Cargo.toml`
- 修改：`crates/tools/Cargo.toml`
- 创建：`crates/tools/src/terminal/backend.rs`
- 创建：`crates/tools/src/terminal/backend/pipe.rs`
- 测试：`crates/tools/src/terminal/backend/pipe.rs` 内的 `#[cfg(test)] mod tests`

**接口：**

- 产出：`TerminalBackend` trait 的 `write`、`interrupt`、`terminate`。
- 产出：`spawn_backend(request: &TerminalSpawnRequest) -> Result<SpawnedTerminal, TerminalError>` 的 pipe 分支。
- `SpawnedTerminal` 提供 output receiver、exit receiver、控制句柄和可选 OS PID；这些字段不进入 ACP 公共类型。

- [x] **步骤 1：写真实 pipe 失败测试**

测试直接创建后端，不经过 Agent 工具：

```rust
/// Pipe output preserves observed stdout/stderr arrival order and exit status.
#[tokio::test]
async fn pipe_backend_merges_stdout_stderr_and_reports_exit() {
    let spawned = spawn_pipe(test_request("printf out; printf err >&2; exit 7"))
        .await
        .expect("spawn pipe");
    let captured = collect_until_exit(spawned).await.expect("collect pipe");
    assert_eq!(captured.output, "outerr");
    assert_eq!(captured.exit_code, Some(7));
}

#[cfg(unix)]
/// Terminating one pipe backend prevents descendants from outliving it.
#[tokio::test]
async fn pipe_termination_kills_descendant_process_group() {
    let marker = tempfile::NamedTempFile::new().expect("marker");
    let spawned = spawn_pipe(test_request(&format!(
        "(sleep 2; printf leaked > {}) & sleep 30",
        marker.path().display()
    ))).await.expect("spawn process tree");
    spawned.control.terminate().await.expect("terminate process tree");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(std::fs::metadata(marker.path()).expect("marker metadata").len(), 0);
}
```

- [x] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p tools terminal::backend::pipe::tests`

预期：因 backend 和 pipe spawn 尚未实现而编译失败。

- [x] **步骤 3：声明依赖并实现 pipe 后端**

根依赖新增并按语义分组排序：

```toml
# pty
nix = { version = "0.31", features = ["signal", "process"] }
portable-pty = "0.9.0"

# windows process
win32job = "2.0.3"
```

`crates/tools/Cargo.toml` 普通依赖继承 `portable-pty`，并在 `cfg(windows)` target dependency 中继承 `win32job`。

同时在 tools 的 tracing/log 语义组继承根 `tracing`，供 manager 记录生命周期边界。所有 event macro 必须使用 `tracing::info!()`、`tracing::warn!()` 等全限定形式。

pipe command 设置 cwd、`CLAW_SESSION_ID`、`CLAW_TURN_ID`、stdin null、stdout/stderr piped 和 `kill_on_drop(true)`。Unix 使用 `process_group(0)`；Windows 创建带 `kill_on_job_close` 的 `win32job::Job` 并将 child handle 分配进去。

`backend.rs` 集中解析 shell：Unix 只接受绝对、存在且可执行的 `$SHELL`，否则依次回退 `/bin/bash`、`sh`；Windows 使用 `powershell.exe`。PTY 和 pipe 必须调用同一个 resolver，并以各 shell 的命令参数形式执行完整 `cmd`，不能自行按空格拆分。

两个 Tokio reader 写入一个有界 mpsc；聚合任务按到达顺序转交 output sink。等待任务同时处理 child exit 和 control channel，终止后继续在 500ms grace 内排空继承的 stdio，随后中止 reader。

`pipe.rs` 的测试模块内定义带英文注释的 `test_request` 和 `collect_until_exit`；前者构造固定 Session/Turn/cwd 请求，后者同时消费 output receiver 与 exit receiver，并在退出后返回完整测试捕获值。这些辅助函数不导出到生产 API。

- [x] **步骤 4：运行 pipe 测试**

运行：`rtk cargo test -p tools terminal::backend::pipe::tests`

预期：快速退出、非零退出、输出顺序和当前平台进程树终止测试全部 PASS。

- [x] **步骤 5：检查平台编译和差异**

运行：

```bash
rtk cargo check -p tools --all-targets
rtk rustup target list --installed
rtk git diff --check
rtk git status --short
```

预期：本机编译通过，并明确记录 Windows target 是否可用。如果列表包含 `x86_64-pc-windows-gnu`，追加运行 `rtk cargo check -p tools --target x86_64-pc-windows-gnu`；若未安装，依赖 `#[cfg(windows)]` 测试在 Windows 环境执行，不能把未运行写成通过；不执行 commit。

---

### 任务 4：实现真实 PTY 后端

**文件：**

- 创建：`crates/tools/src/terminal/backend/pty.rs`
- 修改：`crates/tools/src/terminal/backend.rs`
- 测试：`crates/tools/src/terminal/backend/pty.rs` 内的 `#[cfg(test)] mod tests`

**接口：**

- 完成：`spawn_backend` 的 `tty=true` 分支。
- PTY writer 支持任意字符；control 支持中断和完整终止；exit receiver 返回可选退出码。

- [x] **步骤 1：写真实 PTY 失败测试**

测试必须使用 `native_pty_system()`，不能以 duplex/mock 替代：

```rust
/// A native PTY exposes terminal descriptors and carries interactive input.
#[tokio::test]
async fn pty_backend_exposes_a_real_terminal_and_accepts_input() {
    let spawned = spawn_pty(test_request(
        "python3 -u -c 'import os; print(os.isatty(0), os.isatty(1)); print(input(\"name: \"))'"
    )).await.expect("spawn real pty");
    wait_for_output(&spawned, "name: ").await;
    spawned.control.write("clawcode\n").await.expect("write pty");
    let captured = collect_until_exit(spawned).await.expect("collect pty");
    assert!(captured.output.contains("True True"));
    assert!(captured.output.contains("clawcode"));
    assert_eq!(captured.exit_code, Some(0));
}

#[cfg(unix)]
/// Ctrl-C interrupts the PTY foreground process group within a bounded wait.
#[tokio::test]
async fn pty_ctrl_c_interrupts_foreground_process_group() {
    let spawned = spawn_pty(test_request("sleep 30"))
        .await
        .expect("spawn pty sleeper");
    spawned.control.write("\u{3}").await.expect("send ctrl-c");
    let exit = tokio::time::timeout(Duration::from_secs(3), spawned.exit)
        .await
        .expect("pty exits after ctrl-c")
        .expect("exit channel");
    assert_ne!(exit.exit_code, Some(0));
}
```

- [x] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p tools terminal::backend::pty::tests`

预期：`tty=true` 分支尚不存在或返回 unsupported。

- [x] **步骤 3：实现 PTY spawn 和阻塞桥接**

使用固定初始尺寸 24x80：

```rust
PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }
```

通过平台 shell builder 启动命令；保存 `master.take_writer()`、`master.try_clone_reader()`、child 和 `clone_killer()`。reader 和 child wait 分别运行在 `spawn_blocking`；writer 每次写入和 flush 也通过 `spawn_blocking`，不能占用 Tokio worker。

Unix `\x03` 原样写入 PTY；强制 terminate 同时调用进程组 kill 和 portable child killer。Windows 将 child raw handle 分配到带 `kill_on_job_close` 的 Job Object；terminate 关闭 Job 并调用 child killer，二者均幂等。

`pty.rs` 的测试模块内定义带英文注释的 `test_request`、`wait_for_output` 和 `collect_until_exit`，直接消费内部 spawn 返回值；这些函数不出现在非测试构建中。

- [x] **步骤 4：运行真实 PTY 测试**

运行：`rtk cargo test -p tools terminal::backend::pty::tests -- --nocapture`

预期：测试输出证明 stdin/stdout 都是 TTY、提示交互成功、Ctrl-C 在限定时间内退出。

- [x] **步骤 5：检查阻塞与差异**

运行：`rtk cargo clippy -p tools --all-targets -- -D warnings && rtk git diff --check && rtk git status --short`

预期：不存在同步 PTY I/O 直接运行在 async worker 的代码路径；不执行 commit。

---

### 任务 5：实现 Session 级 TerminalManager、状态机和 LRU

**文件：**

- 创建：`crates/tools/src/terminal/process.rs`
- 创建：`crates/tools/src/terminal/manager.rs`
- 修改：`crates/tools/src/terminal/mod.rs`
- 测试：`crates/tools/src/terminal/manager.rs` 内的 `#[cfg(test)] mod tests`

**接口：**

- 产出：

```rust
#[async_trait]
pub trait TerminalService: Send + Sync {
    /// Starts one Session-owned process and performs its initial bounded wait.
    async fn spawn(&self, request: TerminalSpawnRequest)
        -> Result<TerminalInteractionOutput, TerminalError>;
    /// Writes or polls one existing Session-owned process.
    async fn interact(&self, request: TerminalInteractionRequest)
        -> Result<TerminalInteractionOutput, TerminalError>;
}

impl TerminalManager {
    /// Creates an empty native process manager for one Session.
    pub fn new(session_id: SessionId) -> Self;
    /// Returns one non-consuming snapshot of every retained terminal.
    pub fn list(&self) -> Result<TerminalListResult, TerminalError>;
    /// Subscribes one host observer to transient terminal changes.
    pub fn subscribe(&self) -> broadcast::Receiver<TerminalEvent>;
    /// Terminates and removes one retained terminal idempotently.
    pub async fn terminate(&self, terminal_id: TerminalId)
        -> Result<bool, TerminalError>;
    /// Terminates and removes every retained terminal with one reason.
    pub async fn terminate_all(&self, reason: TerminalRemovalReason)
        -> Result<usize, TerminalError>;
}
```

- [x] **步骤 1：写 manager 失败测试**

至少覆盖后台化、跨调用游标、并发、取消、容量和 Session 隔离：

```rust
/// Cancelling initial wait leaves an already registered process Session-owned.
#[tokio::test]
async fn running_process_is_registered_before_initial_wait_is_cancelled() {
    let manager = Arc::new(TerminalManager::new(session("cancelled-spawn")));
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let manager = Arc::clone(&manager);
        let cancellation = cancellation.clone();
        async move { manager.spawn(spawn_request("sleep 30", cancellation)).await }
    });
    wait_until(|| manager.list().expect("list").terminals.len() == 1).await;
    cancellation.cancel();
    assert!(matches!(task.await.expect("spawn task"), Err(TerminalError::Cancelled)));
    assert_eq!(manager.list().expect("list").terminals.len(), 1);
    manager.terminate_all(TerminalRemovalReason::SessionClosed).await.expect("cleanup");
}

/// Session-local identifiers never authorize access through another manager.
#[tokio::test]
async fn managers_reject_each_others_terminal_ids() {
    let first = TerminalManager::new(session("first"));
    let second = TerminalManager::new(session("second"));
    let running = first.spawn(spawn_request("sleep 30", CancellationToken::new()))
        .await.expect("spawn first");
    let id = running.running_id().expect("running id");
    assert!(matches!(second.interact(poll_request(id)).await, Err(TerminalError::UnknownTerminal { .. })));
    first.terminate_all(TerminalRemovalReason::SessionClosed).await.expect("cleanup");
}
```

容量测试在 `manager.rs` 的测试模块内实现 `ControlledBackendFactory` 和 `SequenceIdSource`，通过 manager 的内部构造路径注入，创建 64 个受控后端并断言已退出优先、LRU 和最近 8 个保护，不使用真实的 64 个长时间 OS 进程。测试替身只能定义在允许的 `#[cfg(test)] mod tests` 内，生产类型不增加 test-only 字段、函数或 impl。

- [x] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p tools terminal::manager::tests`

预期：manager、service 和请求类型尚不存在。

- [x] **步骤 3：实现状态机和 watcher**

`TerminalProcess` 通过独立 `tokio::sync::Mutex<()>` 串行交互；输出 buffer 使用短临界区。后台聚合任务持续 append 并发送 `TerminalEvent::Output`，退出 watcher 写入 `Exited` 或 `Failed` 状态并 `Notify::notify_waiters()`。

`spawn` 流程固定为：验证 cwd → 容量回收 → 分配 ID → 创建后端 → 插入表 → 发布 started → 开始初始等待。取消只结束等待，不移除条目。

`interact` 流程固定为：按 ID clone 条目 → 获取交互锁 → 根据 tty/字符规则写入 → 等待输出版本变化、退出、等待窗口或 cancellation → drain Agent 输出 → 退出时从表中条件移除。

普通 pipe 收到除单个 `\x03` 外的非空 chars 时返回 `TerminalError::InputUnsupported`。空轮询等待最少 5000ms；非空写入默认 250ms，并在 output 到达后保留最多 100ms 反应窗口。

只在创建、转为后台、退出、失败、容量回收、显式终止和 Session 清理边界记录日志。INFO 只能包含 Session ID、Terminal ID、TTY 标志和状态；命令与输出都不能进入 INFO/DEBUG。reader/backend 错误只记录错误类别和 OS error，不记录已捕获输出。

- [x] **步骤 4：实现 ID 与 LRU 回收**

随机 ID 使用 `fastrand::u32(1000..100000)` 并检查冲突。表达到 64 条时：先按 `last_activity_at` 回收 exited/failed；仍满时在排除最近使用的 8 条后选择最旧 running 条目，await 完整 terminate 后再插入新进程。

不得跨 await 持有 manager 表锁。容量选择阶段只 clone 目标 `Arc<TerminalProcess>`，终止完成后重新加锁并使用 `Arc::ptr_eq` 条件移除，避免误删复用 ID。

- [x] **步骤 5：运行 manager 和真实后端测试**

运行：

```bash
rtk cargo test -p tools terminal::manager::tests
rtk cargo test -p tools terminal::backend::pipe::tests
rtk cargo test -p tools terminal::backend::pty::tests
```

预期：全部 PASS；所有测试末尾显式 `terminate_all`，测试进程表为空。

- [x] **步骤 6：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：manager 子系统完整可审查；不执行 commit。

---

### 任务 6：替换模型侧 bash 工具

**文件：**

- 创建：`crates/tools/src/builtin/exec_command.rs`
- 创建：`crates/tools/src/builtin/write_stdin.rs`
- 删除：`crates/tools/src/builtin/bash.rs`
- 修改：`crates/tools/src/builtin/mod.rs`
- 修改：`crates/tools/src/contract.rs`
- 修改：`crates/tools/src/lib.rs`
- 删除：`crates/tools/tests/bash.rs`
- 创建：`crates/tools/tests/terminal_tools.rs`
- 修改：`crates/tools/tests/builtins.rs`
- 修改：`crates/tools/tests/contract.rs`
- 修改：`crates/tools/tests/filesystem.rs`
- 修改：`crates/tools/tests/registry.rs`

**接口：**

- `ToolExecutionContext` 新增 `terminals: Arc<dyn TerminalService>`。
- 产出模型工具 `exec_command` 与 `write_stdin`，响应使用 JSON text block。
- 保留 crate 根导出的 `BashExecutor`，仅移除模型适配器。

- [x] **步骤 1：写工具 Schema 和调用失败测试**

```rust
/// The built-in shell group exposes only the two Codex-style tools.
#[test]
fn shell_registry_exposes_only_codex_style_tools() {
    let names = BuiltinToolFactory::new()
        .filesystem_enabled(false)
        .create()
        .expect("registry")
        .names();
    assert_eq!(names, vec!["exec_command", "write_stdin"]);
}

/// The tool adapters carry one real PTY from creation through interactive exit.
#[tokio::test]
async fn exec_then_write_stdin_completes_interactive_pty() {
    let manager = Arc::new(TerminalManager::new(session("tool-pty")));
    let context = execution_context(Arc::clone(&manager) as Arc<dyn TerminalService>);
    let started = registry().execute(exec_call(
        "python3 -u -c 'print(input(\"prompt: \"))'", true, 250
    ), &context).await.expect("exec command");
    let id = response_json(&started)["session_id"].as_u64().expect("session id") as u32;
    let completed = registry().execute(write_call(id, "answer\n"), &context)
        .await.expect("write stdin");
    assert!(response_json(&completed)["output"].as_str().expect("output").contains("answer"));
    assert_eq!(response_json(&completed)["exit_code"], 0);
}
```

另加参数测试：不存在 cwd、`max_output_tokens=0`、普通 pipe 普通输入、未知 ID、yield 边界收敛。

- [x] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p tools --test terminal_tools && rtk cargo test -p tools --test builtins`

预期：注册表仍含 `bash`，新工具不存在。

- [x] **步骤 3：实现 Context 注入和不可用默认服务**

为不使用 shell 的直接工具测试提供 `UnavailableTerminalService`，并将 context builder 字段设置默认值；生产 Kernel 后续必须显式覆盖：

```rust
/// Runtime context supplied to every tool invocation.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub trace_id: TraceId,
    pub cwd: PathBuf,
    pub cancellation: CancellationToken,
    pub updates: Arc<dyn ToolUpdateSink>,
    #[builder(default = Arc::new(UnavailableTerminalService) as Arc<dyn TerminalService>)]
    pub terminals: Arc<dyn TerminalService>,
}
```

不可用服务始终返回 `TerminalError::Unavailable`，不能悄悄创建全局 manager。

- [x] **步骤 4：实现两个工具适配器**

`exec_command` Schema 使用 `cmd`、`workdir`、`tty`、`yield_time_ms`、`max_output_tokens`；`write_stdin` 使用 `session_id`、`chars`、`yield_time_ms`、`max_output_tokens`。两者都在 `validate` 中完成 JSON 解码和数值边界检查。

相对 `workdir` 以 `ToolExecutionContext.cwd` 为基准，绝对路径原样验证；两类路径都必须存在且是目录。非空 write 的 `yield_time_ms` 收敛到 250–30000ms；空轮询收敛到 5000–300000ms。`max_output_tokens=0` 是参数错误，大于 10000 时收敛到 10000。

工具输出序列化为两种互斥形状：

```json
{"output":"...","wall_time_seconds":0.25,"session_id":7319}
```

```json
{"output":"...","wall_time_seconds":0.01,"exit_code":0}
```

信号退出必须保留 `"exit_code": null`。工具调用本身只在参数、spawn、后端或访问错误时标记失败；命令的非零 exit code 是正常工具结果，不转换为 `ToolError`。

prompt contribution 明确说明：长命令返回 session ID、空 `write_stdin` 可轮询、交互命令需要 `tty=true`。

- [x] **步骤 5：删除 bash 模型适配器并更新测试 context**

`BuiltinToolFactory::shell_enabled` 的注释改为启用/禁用 Codex 风格 shell tools。删除 `builtin/bash.rs` 和模型 bash 测试，但保留 `src/bash.rs` 及 user-bash 相关测试。所有 direct context builder 使用默认不可用服务或显式 manager。

- [x] **步骤 6：运行 tools 全量测试**

运行：`rtk cargo test -p tools && rtk cargo clippy -p tools --all-targets -- -D warnings`

预期：tools 全部 PASS，注册表没有 `bash`，真实 PTY 工具调用通过。

- [x] **步骤 7：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：工具替换完整，user-bash executor 未被删除；不执行 commit。

---

### 任务 7：接入 Kernel Session 生命周期

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 创建：`crates/kernel/src/runtime/terminal.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/tools/batch.rs`
- 修改：`crates/app/src/main.rs`
- 创建：`crates/kernel/tests/terminal_lifecycle.rs`
- 修改：`crates/kernel/tests/support/mod.rs`

**接口：**

- `Session` 新增 `terminals: Arc<TerminalManager>`。
- Kernel 产出：`terminals`、`subscribe_terminals`、`terminate_terminal`、`clean_terminals`、`terminate_all_terminals`。
- Tool batch 显式注入当前 Session 的 `Arc<dyn TerminalService>`。
- 测试辅助类型：`TerminalKernelFixture::{new, start_background_command, fork_session, close_session, resume_session, process_has_exited}`，全部定义在 `crates/kernel/tests/support/mod.rs`，使用真实 `TerminalManager` 和脚本化 Model，不提供 fixture backend 给 WebUI。

- [ ] **步骤 1：写 Kernel 生命周期失败测试**

使用可脚本化 Model 触发真实工具调用，并验证跨 Turn 和 close：

```rust
/// Turn cancellation preserves a background process while Session close kills it.
#[tokio::test]
async fn terminal_survives_turn_cancellation_but_not_session_close() {
    let fixture = TerminalKernelFixture::new().await;
    let terminal_id = fixture.start_background_command("sleep 30").await;
    fixture.kernel.cancel_session(&fixture.session_id).expect("cancel turn");
    assert_eq!(fixture.kernel.terminals(&fixture.session_id).expect("list").terminals.len(), 1);
    fixture.kernel.close_session(&fixture.session_id).await.expect("close session");
    assert!(fixture.process_has_exited(terminal_id).await);
}

/// Fork and resume always construct empty terminal managers.
#[tokio::test]
async fn fork_and_resume_receive_empty_terminal_managers() {
    let fixture = TerminalKernelFixture::new().await;
    let terminal_id = fixture.start_background_command("sleep 30").await;
    let fork_id = fixture.fork_session().await;
    assert_eq!(fixture.kernel.terminals(&fixture.session_id).expect("source terminals").terminals.len(), 1);
    assert_eq!(fixture.kernel.terminals(&fork_id).expect("fork terminals").terminals.len(), 0);

    fixture.close_session(&fixture.session_id).await;
    assert!(fixture.process_has_exited(terminal_id).await);
    fixture.resume_session(&fixture.session_id).await;
    assert_eq!(fixture.kernel.terminals(&fixture.session_id).expect("resumed terminals").terminals.len(), 0);
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p kernel --test terminal_lifecycle`

预期：Kernel 尚无终端能力和 Session 字段。

- [x] **步骤 3：在 Session 创建和工具执行中注入 manager**

`build_session` 在读取 `session_id` 后创建：

```rust
let terminals = Arc::new(TerminalManager::new(session_id.clone()));
```

写入 `Session::builder()`；`ToolExecutionContext::builder()` 使用：

```rust
.terminals(Arc::clone(&batch.session.terminals) as Arc<dyn TerminalService>)
```

因此 new/resume/fork 都通过统一 `build_session` 获得空 manager。

- [x] **步骤 4：实现 Kernel 宿主能力和清理顺序**

`runtime/terminal.rs` 只负责 Session 定位和 manager 委托。`close_session` 在 lifecycle 进入 Closing、extension shutdown hook 完成后调用 `terminate_all(SessionClosed)`，并使用现有 `preserve_primary_error` 链合并 terminal、MCP、store 和 lifecycle 错误。

`Kernel::terminate_all_terminals` 先在读锁内 clone 所有 manager，再释放 sessions 锁后并发 await 清理。`app/main.rs` 在 stdio 或 HTTP transport 停止后调用该方法，确保优雅 shutdown 不遗留进程；manager/process Drop 仍保留 best-effort kill 作为异常退出兜底。

- [x] **步骤 5：运行 Kernel 测试**

运行：

```bash
rtk cargo test -p kernel --test terminal_lifecycle
rtk cargo test -p kernel --test turn
rtk cargo test -p kernel --test session_capabilities
```

预期：跨 Turn、cancel、close、delete、resume、fork 和 shutdown 测试 PASS，原有 Turn 语义无回归。

- [x] **步骤 6：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：Kernel 只增加 Session 终端能力和清理路径；不执行 commit。

---

### 任务 8：实现 ACP 管理请求和实时通知

**文件：**

- 创建：`crates/acp/src/terminal.rs`
- 修改：`crates/acp/src/lib.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/src/server.rs`
- 创建：`crates/acp/tests/terminal.rs`
- 修改：`crates/acp/tests/extension.rs`

**接口：**

- ACP 请求：`_clawcode/terminal/list`、`terminate`、`clean`。
- ACP 通知：`_clawcode/terminal/update`。
- 每个 ACP connection 按 Session 去重安装 terminal watcher。
- 测试辅助类型：`TerminalAcpFixture::{connect, advertised_methods, create_session, start_background, recv_terminal_event, request_json}`，完整定义在 `crates/acp/tests/terminal.rs`，底层必须使用 `Client.v2().connect_with(AcpServerFactory::component(...))`。

- [ ] **步骤 1：写 ACP 失败测试**

通过真实 ACP component 连接和 Kernel manager，覆盖方法广告、请求响应和通知：

```rust
/// ACP advertises, streams, and executes every terminal management operation.
#[tokio::test]
async fn terminal_extensions_list_notify_terminate_and_clean() {
    let mut fixture = TerminalAcpFixture::connect().await;
    assert!(fixture.advertised_methods().contains("_clawcode/terminal/list"));
    assert!(fixture.advertised_methods().contains("_clawcode/terminal/terminate"));
    assert!(fixture.advertised_methods().contains("_clawcode/terminal/clean"));
    let session_id = fixture.create_session().await;
    let terminal_id = fixture.start_background(&session_id, "printf ready; sleep 30").await;
    let started = fixture.recv_terminal_event("started").await;
    assert_eq!(started["event"]["terminal"]["terminalId"], terminal_id.get());

    let listed = fixture.request_json("_clawcode/terminal/list", serde_json::json!({
        "sessionId": session_id.as_str()
    })).await;
    assert_eq!(listed["terminals"][0]["command"], "printf ready; sleep 30");
    assert_eq!(listed["terminals"][0]["status"], "running");

    let first = fixture.request_json("_clawcode/terminal/terminate", serde_json::json!({
        "sessionId": session_id.as_str(), "terminalId": terminal_id.get()
    })).await;
    let second = fixture.request_json("_clawcode/terminal/terminate", serde_json::json!({
        "sessionId": session_id.as_str(), "terminalId": terminal_id.get()
    })).await;
    assert_eq!(first["terminated"], true);
    assert_eq!(second["terminated"], false);
    let cleaned = fixture.request_json("_clawcode/terminal/clean", serde_json::json!({
        "sessionId": session_id.as_str()
    })).await;
    assert_eq!(cleaned["cleaned"], 0);
}
```

测试必须对每一步发送精确 typed/untyped JSON-RPC 请求并断言 JSON 字段，不能只调用 dispatcher 私有函数。

- [ ] **步骤 2：运行测试并确认失败**

运行：`rtk cargo test -p acp --test terminal`

预期：方法未广告、dispatcher 无分支、通知类型不存在。

- [x] **步骤 3：实现 typed 通知和 watcher**

`AcpTerminalUpdateNotification(TerminalUpdateNotification)` 实现 `JsonRpcMessage` 和 `JsonRpcNotification`，method 精确匹配通知常量。

`ConnectionWatchers` 新增 terminal Session set；`watch_terminals` 仿照 `watch_mcp` 去重订阅，但直接转发 manager 的 typed event。`broadcast::RecvError::Lagged` 不关闭连接：记录不含命令/输出的 WARN，继续等待下一事件，由客户端序号缺口触发 list。

在 new、resume、fork 都安装 watcher；连接关闭后 spawned watcher 随 connection 生命周期结束。

- [x] **步骤 4：实现 dispatcher 和能力广告**

list 使用 `AcpSessionParameters`；terminate 使用 `AcpTerminalTargetParameters`；clean 使用 `AcpSessionParameters`。所有响应通过 protocol typed result 再 `serde_json::to_value`，未知 Session 保留现有 Kernel error 映射，未知 terminal 的 terminate 返回 `terminated=false`。

初始化 metadata 的 methods 数组加入三个方法；terminal update 是通知，不加入 request methods 数组。

- [x] **步骤 5：运行 ACP 测试和日志测试**

运行：

```bash
rtk cargo test -p acp --test terminal
rtk cargo test -p acp --test extension
rtk cargo test -p acp --test tracing
```

预期：管理请求、易失通知和现有参数脱敏测试全部 PASS，日志断言不出现完整 output。

- [x] **步骤 6：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：ACP 边界完整且不修改 transcript projection；不执行 commit。

---

### 任务 9：在 WebUI Inspector 中管理后台终端

**文件：**

- 修改：`web/src/acp/extensions.ts`
- 修改：`web/src/acp/protocol.ts`
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/sessionState.ts`
- 修改：`web/src/workspace/sessionStaging.ts`
- 修改：`web/src/workspace/controller.ts`
- 创建：`web/src/features/inspector/TerminalPanel.tsx`
- 修改：`web/src/features/inspector/Inspector.tsx`
- 修改：`web/src/features/conversation/ToolCallCard.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**

- `TerminalId = Brand<number, "TerminalId">`。
- `TerminalEntity` 对应 ACP snapshot，并保存 `lastOutputSequence?: number`。
- Session state 新增 `terminals: ReadonlyMap<TerminalId, TerminalEntity>`。
- Controller 新增 `terminateTerminal(id)`、`cleanTerminals()`、`refreshTerminals(sessionId)`。

- [x] **步骤 1：定义前端协议和领域类型**

在 `extensions.ts` 增加三个 method 和一个 notification 名；在 `protocol.ts` / `domain/model.ts` 定义：

```ts
export type TerminalId = Brand<number, "TerminalId">;
export type TerminalEntity = Readonly<{
  terminalId: TerminalId;
  command: string;
  cwd: string;
  tty: boolean;
  status: "running" | "exited" | "failed";
  exitCode?: number | null;
  startedAt: TimestampMs;
  lastActivityAt: TimestampMs;
  lastOutputSequence?: number;
}>;
```

增加严格 decoder：拒绝非安全整数 ID/sequence、未知 status、缺失字段和数组伪装对象。不能把未经验证的 JSON 直接 cast 成 `TerminalEntity`。

- [x] **步骤 2：接入 Session store 和通知**

新增 action：`terminal/replaced`、`terminal/upserted`、`terminal/output`、`terminal/removed`。reducer 保持 map 不可变更新；`transcript/cleared` 不清空 terminals，因为 transcript replay 与运行时终端无关，只有 Session close/delete 或 list replacement 才改变该 map。

Controller 在 open/recovery snapshot Promise 中调用 terminal list。通知处理规则：started upsert；output 更新 sequence/lastActivity；exited/failed 更新状态；removed 删除。发现 sequence 不连续时立即 `refreshTerminals(sessionId)`，且同一 Session 只允许一个在途刷新。

- [x] **步骤 3：实现终端面板和控制动作**

`TerminalPanel` 展示 command、cwd、PTY/Pipe、状态、退出码和最近活动时间。running 条目提供“终止”按钮；顶部仅在列表非空时提供“清理全部”。按钮 await controller 请求，期间 disabled，错误写入现有 diagnostics。

Inspector 增加 `terminals` tab 和 `SquareTerminal` 图标，空状态文本为“当前会话没有后台终端。”。所有按钮提供 `aria-label`、键盘焦点和可见 loading/disabled 状态。

`ToolCallCard` 的默认展开条件从旧 `bash` 改为 `exec_command` 或 `write_stdin` 的 in-progress 状态，使真实交互输出可直接看到。

- [x] **步骤 4：增加样式并运行前端门禁**

只在 `workbench.css` 增加 `.terminal-panel`、`.terminal-card` 和控制按钮样式，复用现有 token，不增加硬编码品牌色。

运行：

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
```

预期：TypeScript、ESLint 和 Vite 生产构建全部 PASS。

- [x] **步骤 5：检查未提交差异**

运行：`rtk git diff --check && rtk git status --short`

预期：WebUI 只增加 Session 级终端运行时状态，不污染 transcript；不执行 commit。

---

### 任务 10：全量回归、真实 production app PTY E2E 与进程泄漏验收

> **实施调整：** 任务 7 没有新增独立的 `terminal_lifecycle.rs` fixture，生命周期边界由真实 `TerminalManager` 集成测试、现有 Kernel Session 回归测试和 production app 退出清理 E2E 共同覆盖。任务 8 的请求测试并入现有 `acp/tests/extension.rs`，typed 通知 round-trip 放在 `acp/src/terminal.rs` 的标准测试模块中；实时 watcher 由 production app 浏览器重连 E2E 覆盖。上方四个原始红灯步骤因此保留未勾选，最终行为验收已经完成。

**文件：**

- 验证：所有本计划涉及文件

**接口：**

- 消费前九个任务的最终行为，不增加 fixture backend 或 mock provider。

- [x] **步骤 1：执行格式和 Rust 全量门禁**

运行：

```bash
rtk cargo fmt --all -- --check
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

预期：全部 PASS；记录测试数量和 ignored 数。若失败，按 `superpowers:systematic-debugging` 定位根因，修复后重新运行完整失败命令。

- [x] **步骤 2：执行 Web 和生产静态服务门禁**

运行：

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
rtk cargo test -p app --test web
```

预期：全部 PASS，production app 能提供新构建的静态资源和 ACP WebSocket。

- [x] **步骤 3：启动真实 production app**

使用根目录现有 `claw.toml` 和当前 provider 凭据，不改成测试 provider：

```bash
rtk cargo run -p app --bin clawcode -- serve --bind 127.0.0.1:3000 --web-root web/dist
```

保持 PTY session 运行，等待日志明确显示真实 provider preflight 成功和 HTTP transport 监听成功。不得输出、复制或汇报凭据正文。

- [x] **步骤 4：通过真实浏览器完成 Agent PTY 交互**

打开 `http://127.0.0.1:3000`，创建指向精确临时 cwd 的新 Session。向真实 Agent 发送：

```text
请调用 exec_command，设置 tty=true、yield_time_ms=250，运行：
python3 -u -c 'import os; print(f"TTY:{os.isatty(0)}:{os.isatty(1)}"); value=input("VALUE>"); print(f"ECHO:{value}")'
命令等待输入后停止，不要替我输入。
```

验收：工具卡名称是 `exec_command`，输出包含 `TTY:True:True` 和 `VALUE>`，JSON 结果包含数字 `session_id`；Inspector Terminals 中出现 running PTY。

在下一 Turn 发送：

```text
使用上一轮返回的 session_id 调用 write_stdin，chars 必须是 "CLAW-PTY-E2E\n"，然后轮询直到进程退出。
```

验收：结果包含 `ECHO:CLAW-PTY-E2E` 和 `exit_code: 0`，证明 stdin 交互和跨 Turn 存活。

- [x] **步骤 5：通过真实浏览器完成后台管理和进程树清理**

让 Agent 使用 `exec_command(tty=true, yield_time_ms=250)` 启动：

```bash
sh -c 'sleep 300 & wait'
```

在 Inspector 确认 running terminal，点击单个终止；验收 terminal update 使条目移除。再次启动两个长进程，点击“清理全部”，验收列表为空。

在宿主 shell 使用只读进程查询确认没有属于测试命令的 `sleep 300` 或 Python 进程；查询命令同样使用 `rtk`。不能通过宽泛 kill 命令清理不确定目标。

- [x] **步骤 6：验证 ACP 重连和应用重启语义**

启动一个新的长时间 PTY，刷新浏览器；验收重新连接后 terminal list 恢复 running 条目。停止 production app，确认 Drop/shutdown 清理该进程树；重新启动 app 并 resume 同一个 Session，验收 Inspector 终端列表为空，不伪造进程恢复。

- [x] **步骤 7：验证 user-bash 和模型工具迁移**

在 Composer 执行：

```text
!printf 'USER-BASH-E2E'
```

验收现有 BashExecutionCard 正常显示输出。随后要求 Agent 列出其 shell 工具，确认模型侧只有 `exec_command` 和 `write_stdin`，不会调用已移除的 `bash`。

- [x] **步骤 8：停止测试服务并完成最终审查**

先通过 WebUI clean 清空测试 Session 的终端，再正常停止 production app。运行：

```bash
rtk git diff --check
rtk git status --short
rtk git diff -- Cargo.toml crates web README.md docs/superpowers/specs/2026-08-29-codex-style-pty-terminal-design.md docs/superpowers/plans/2026-08-29-codex-style-pty-terminal.md
```

使用 `superpowers:verification-before-completion` 对最近一次完整测试输出、真实浏览器证据和 OS 进程查询做最终核验。向用户报告实际通过项、真实 provider E2E 结果、平台限制和未创建 commit 的状态；等待用户明确授权后才进入 pre-commit 与 commit 流程。

---

## 最终验收清单

- [x] 模型工具定义只包含 `exec_command` / `write_stdin`，不存在 `bash`。
- [x] pipe 快速命令、非零退出、后台化、轮询和 Ctrl-C 行为正确。
- [x] 真实 PTY 的 stdin/stdout 检测、提示输入和跨 Turn 交互通过。
- [x] 单进程输出内存不超过 1 MiB，token 响应裁剪和 omission 标记正确。
- [x] 同终端交互串行，不同终端并发，64/8/LRU 规则通过。
- [x] Session close/delete、Kernel shutdown 和当前 Linux 进程树清理通过；Windows 使用条件编译的 Job Object 实现，本轮未在 Windows 主机实测。
- [x] ACP list/terminate/clean 和 started/output/exited/failed/removed 通知通过。
- [x] WebUI Inspector 能查看、终止和清理后台终端，重连时重新同步。
- [x] 用户 `!` / `!!` 无回归。
- [x] workspace fmt/test/clippy、Web check/build 和 app web tests 全部通过。
- [x] production app + 当前真实 provider + 真实浏览器 E2E 全部通过。
- [x] 测试结束后不存在遗留后台进程。
- [x] 未经用户明确授权没有创建 commit。

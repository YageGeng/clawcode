# Tool 流式执行重构实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 重构 Tool trait 引入流式执行机制，新增 `ExecCommandBegin` → `ExecCommandOutputDelta` → `ExecCommandEnd` 事件流，统一 dispatch 路径，删除 `execute_structured`。

**架构：** Tool trait 新增加 `capability()` 和 `execute_streaming()`，删除 `execute_structured()`。`execute_streaming` 返回 `(String, Stream<Item = ToolStreamItem>)`，dispatch 层统一驱动 stream 并转发事件。`ToolStreamItem` 四种变体：`Begin(TurnItem)` → `ItemStarted`，`End(TurnItem)` → `ItemCompleted`，`Delta` → `ExecCommandOutputDelta`，`Text { content, is_error }` → `ToolCallUpdate`。shell 工具改为 spawn 子进程、边读 pipe 边产出 `Delta`。

**技术栈：** tokio (process::Command, sync::mpsc, sync::oneshot), async-trait, futures::stream, tokio-stream, serde_json, typed-builder

**规格文档：** `docs/superpowers/specs/2026-05-18-tool-streaming-refactor-design.md`

---

## 文件清单

| 文件 | 操作 | 用途 |
|---|---|---|
| `crates/protocol/src/item.rs` | 修改 | 新增 `ExecCommandItem`、`ExecCommandStatus`、`TurnItem::ExecCommand` 变体 |
| `crates/protocol/src/event.rs` | 修改 | 新增 `ExecCommandOutputDelta`、`ExecOutputStream`、`ToolStreamItem` |
| `crates/protocol/src/tool.rs` | 修改 | 新增 `ToolCapability` |
| `crates/protocol/src/lib.rs` | 修改 | 导出新增类型 |
| `crates/tools/src/lib.rs` | 修改 | Tool trait 增加 `capability()`、`execute_streaming()`，删除 `execute_structured()` |
| `crates/tools/src/output.rs` | 删除 | `ToolExecutionResult`、`ToolDisplayOutput` 不再需要 |
| `crates/tools/src/builtin/shell.rs` | 修改 | 改为流式执行 |
| `crates/tools/src/builtin/fs/patch.rs` | 修改 | 改为实现 `execute_streaming` |
| `crates/tools/src/builtin/fs/edit.rs` | 修改 | 改为实现 `execute_streaming` |
| `crates/kernel/src/turn.rs` | 修改 | dispatch 层重构，统一调 `execute_streaming` |
| `crates/kernel/src/tool_events.rs` | 删除 | `ToolEmitter` 职责由 `ToolStreamItem` 替代 |
| `crates/kernel/src/lib.rs` | 修改 | 移除 `tool_events` 模块 |

---

### 任务 1：protocol 层新增类型定义

**文件：**
- 修改：`crates/protocol/src/item.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/protocol/src/tool.rs`
- 修改：`crates/protocol/src/lib.rs`

- [ ] **步骤1：在 `item.rs` 新增 `ExecCommandItem` 和 `ExecCommandStatus`**

在 `TurnItem` enum 中新增 `ExecCommand(ExecCommandItem)` 变体。新增以下类型：

```rust
/// shell/exec 命令的生命周期 item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct ExecCommandItem {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub status: ExecCommandStatus,
    #[builder(default, setter(strip_option))]
    pub stdout: Option<String>,
    #[builder(default, setter(strip_option))]
    pub stderr: Option<String>,
    #[builder(default, setter(strip_option))]
    pub exit_code: Option<i32>,
    #[builder(default, setter(strip_option))]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}
```

`ExecCommandItem` 使用 `typed-builder`，需要 4 个以上字段（此处共 8 个），符合项目 CLAUDE.md 中 "Structs with more than 3 fields must use `typed-builder`" 的约束。

- [ ] **步骤2：在 `event.rs` 新增 `ExecCommandOutputDelta`、`ExecOutputStream`、`ToolStreamItem`**

在 `Event` enum 中新增变体：

```rust
ExecCommandOutputDelta {
    session_id: SessionId,
    call_id: String,
    stream: ExecOutputStream,
    chunk: Vec<u8>,
},
```

新增流输出方向枚举：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}
```

新增 `ToolStreamItem`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolStreamItem {
    Begin(TurnItem),
    End(TurnItem),
    Delta {
        stream: ExecOutputStream,
        chunk: Vec<u8>,
    },
    Text {
        content: String,
        is_error: bool,
    },
}
```

`ToolStreamItem` 不需要 `PartialEq` 派生（`Vec<u8>` chunk 不需要比较），不需要 `Serialize`/`Deserialize`（仅内部使用不经过网络），但为未来扩展保留。

- [ ] **步骤3：在 `tool.rs` 新增 `ToolCapability`**

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolCapability {
    pub supports_streaming: bool,
}
```

- [ ] **步骤4：在 `lib.rs` 导出新增类型**

确保 `ExecCommandItem`、`ExecCommandStatus`、`ExecOutputStream`、`ExecCommandOutputDelta`（作为 Event 变体无需单独导出）、`ToolStreamItem`、`ToolCapability` 在 `pub use` 中可访问。

- [ ] **步骤5：编译验证**

`cargo check -p protocol` 确保无编译错误。

---

### 任务 2：Tool trait 重构

**文件：**
- 修改：`crates/tools/src/lib.rs`
- 删除：`crates/tools/src/output.rs`

- [ ] **步骤1：Tool trait 增加 `capability()` 和 `execute_streaming()`**

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    fn capability(&self) -> ToolCapability {
        ToolCapability::default()
    }

    fn needs_approval(&self, arguments: &serde_json::Value, ctx: &ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<String, String>;

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String> {
        match self.execute(arguments, ctx).await {
            Ok(text) => {
                let stream = futures::stream::once(async {
                    ToolStreamItem::Text { content: text.clone(), is_error: false }
                });
                Ok((text, Box::pin(stream)))
            }
            Err(err) => {
                let stream = futures::stream::once(async {
                    ToolStreamItem::Text { content: err.clone(), is_error: true }
                });
                Ok((err, Box::pin(stream)))
            }
        }
    }
}
```

注意：`execute` 签名中的 `ctx` 参数已从 `&Path` 改为 `&ToolContext`（这是当前代码的已有状态）。

需要新增 imports：`use std::pin::Pin;`、`use protocol::{ToolCapability, ToolStreamItem};`、`futures` workspace dependency。

- [ ] **步骤2：删除 `execute_structured()` 方法**

从 `Tool` trait 中删除 `execute_structured()` 默认实现。

- [ ] **步骤3：更新 `ToolRegistry`**

删除 `execute_structured()` 方法，新增 `execute_streaming()` 包装方法：

```rust
pub async fn execute_streaming(
    &self,
    name: &str,
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String> {
    match self.get(name) {
        Some(tool) => tool.execute_streaming(arguments, ctx).await,
        None => Err(format!("unknown tool: {name}")),
    }
}
```

- [ ] **步骤4：删除 `output.rs`**

`ToolExecutionResult` 和 `ToolDisplayOutput` 不再被任何代码引用后删除此文件。从 `lib.rs` 移除 `pub mod output;` 和相关导出。

- [ ] **步骤5：编译验证**

`cargo check -p tools` 确保无编译错误（此时因 kernel 等下游尚未更新，expect 有 broken import 错误，但 tools crate 自身应通过）。

---

### 任务 3：apply_patch / edit 适配 `execute_streaming`

**文件：**
- 修改：`crates/tools/src/builtin/fs/patch.rs`
- 修改：`crates/tools/src/builtin/fs/edit.rs`

- [ ] **步骤1：apply_patch 改为实现 `execute_streaming`**

删除 `execute_structured` override。`execute` 保持原有纯文本返回逻辑不变。新增 `execute_streaming`：

```rust
fn capability(&self) -> ToolCapability {
    ToolCapability { supports_streaming: true }
}

async fn execute_streaming(
    &self,
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String> {
    let begin = ToolStreamItem::Begin(TurnItem::FileChange(
        FileChangeItem::builder()
            .id(String::new())
            .title("Apply patch".into())
            .changes(vec![])
            .status(FileChangeStatus::InProgress)
            .build(),
    ));

    let result = /* 现有 apply 逻辑，返回 model_output + changes */;

    let end = ToolStreamItem::End(TurnItem::FileChange(
        FileChangeItem::builder()
            .id(String::new())
            .title("Apply patch".into())
            .changes(result.changes)
            .status(FileChangeStatus::Completed)
            .model_output(result.model_output.clone())
            .build(),
    ));

    let stream = futures::stream::iter([begin, end]);
    Ok((result.model_output, Box::pin(stream)))
}
```

注意：`execute` 方法仍然保留，供默认 `execute_streaming` 回退使用（虽然 apply_patch 不再需要，但它是 `Tool` trait 的 required method）。

- [ ] **步骤2：edit 改为实现 `execute_streaming`**

与 apply_patch 同理，`title` 为 `"Edit"`，其余结构一致。

- [ ] **步骤3：编译验证**

`cargo check -p tools` 确保 apply_patch/edit 编译通过。

---

### 任务 4：shell 工具流式改造

**文件：**
- 修改：`crates/tools/src/builtin/shell.rs`

- [ ] **步骤1：增加依赖**

在 `Cargo.toml` 中确认 `tokio-stream` 和 `futures` 可用。shell 模块需要：
- `tokio::sync::mpsc` — 传递 `Delta` 从子进程读取循环到 stream
- `tokio::sync::oneshot` — 回传最终执行结果
- `tokio_stream::wrappers::UnboundedReceiverStream` — mpsc receiver 转 stream
- `futures::stream::Stream` / `futures::stream::once` — 组合 stream

- [ ] **步骤2：实现流式 shell 执行**

```rust
fn capability(&self) -> ToolCapability {
    ToolCapability { supports_streaming: true }
}

async fn execute_streaming(
    &self,
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String> {
    let command_str = arguments
        .get("command").and_then(|v| v.as_str())
        .ok_or("missing 'command' argument")?.to_string();
    let work_dir = arguments
        .get("cwd").and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.cwd.clone());

    let command = vec!["/bin/sh".to_string(), "-c".to_string(), command_str.clone()];
    let cwd = work_dir.clone();

    let (delta_tx, delta_rx) = mpsc::unbounded_channel();
    let (result_tx, result_rx) = oneshot::channel();

    // 在 spawn 前构造 Begin item
    let begin = ToolStreamItem::Begin(TurnItem::ExecCommand(
        ExecCommandItem::builder()
            .id(String::new())
            .command(command.clone())
            .cwd(cwd.clone())
            .status(ExecCommandStatus::InProgress)
            .build(),
    ));

    // spawn 子进程读取循环
    tokio::spawn(async move {
        let start = Instant::now();
        let result = run_with_streaming(&command_str, &work_dir, delta_tx).await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let _ = result_tx.send((result, duration_ms));
    });

    let delta_stream = UnboundedReceiverStream::new(delta_rx);
    let stream = futures::stream::once(async { begin }).chain(delta_stream);

    // 等待子进程结束
    let (exec_result, duration_ms) = result_rx
        .await
        .map_err(|_| "internal error: shell task dropped".to_string())?;

    let (model_text, end_item) = build_shell_result(&command, &cwd, exec_result, duration_ms);

    let stream = stream.chain(futures::stream::once(async { end_item }));
    Ok((model_text, Box::pin(stream)))
}
```

- [ ] **步骤3：实现 `run_with_streaming` 辅助函数**

替代现有的 `execute` 中的阻塞 `Command::output()`：

```rust
async fn run_with_streaming(
    command: &str,
    cwd: &Path,
    delta_tx: mpsc::UnboundedSender<ToolStreamItem>,
) -> std::io::Result<ExecResult> {
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c").arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let stdout_handle = tokio::spawn(read_and_emit(stdout, ExecOutputStream::Stdout, delta_tx.clone()));
    let stderr_handle = tokio::spawn(read_and_emit(stderr, ExecOutputStream::Stderr, delta_tx));

    let status = child.wait().await?;
    let stdout = stdout_handle.await??;
    let stderr = stderr_handle.await??;

    Ok(ExecResult { stdout, stderr, exit_code: status.code().unwrap_or(-1) })
}

async fn read_and_emit<R: AsyncRead + Unpin>(
    mut reader: R,
    stream_type: ExecOutputStream,
    tx: mpsc::UnboundedSender<ToolStreamItem>,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 { break; }
        let _ = tx.send(ToolStreamItem::Delta {
            stream: stream_type,
            chunk: tmp[..n].to_vec(),
        });
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(buf)
}
```

- [ ] **步骤4：实现 `build_shell_result`**

构造 `End` item 和截断后的 model output：

```rust
fn build_shell_result(
    command: &[String],
    cwd: &Path,
    result: ExecResult,
    duration_ms: u64,
) -> (String, ToolStreamItem) {
    let stdout_str = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&result.stderr).to_string();

    let status = if result.exit_code == 0 {
        ExecCommandStatus::Completed
    } else {
        ExecCommandStatus::Failed
    };

    let model_text = format!(
        "exit code: {}\nstdout:\n{}\nstderr:\n{}",
        result.exit_code,
        truncate(&stdout_str, OUTPUT_MAX_LEN / 2),
        truncate(&stderr_str, OUTPUT_MAX_LEN / 2),
    );

    let end_item = ToolStreamItem::End(TurnItem::ExecCommand(
        ExecCommandItem::builder()
            .id(String::new())
            .command(command.to_vec())
            .cwd(cwd.to_path_buf())
            .status(status)
            .stdout(Some(stdout_str))
            .stderr(Some(stderr_str))
            .exit_code(Some(result.exit_code))
            .duration_ms(Some(duration_ms))
            .build(),
    ));

    (model_text, end_item)
}
```

- [ ] **步骤5：编译验证**

`cargo check -p tools` 确保 shell 编译通过。

---

### 任务 5：kernel dispatch 层重构

**文件：**
- 修改：`crates/kernel/src/turn.rs`
- 删除：`crates/kernel/src/tool_events.rs`
- 修改：`crates/kernel/src/lib.rs`

- [ ] **步骤1：重写 `dispatch_tool` 函数**

核心改动：
- `supports_streaming == false` → 先发 `ToolCall(InProgress)`，再调 `execute_streaming`
- `supports_streaming == true` → 直接调 `execute_streaming`（Begin/End 替代 ToolCall 事件）
- 统一驱动 stream，按变体转发 Event

```rust
async fn dispatch_tool(
    ctx: &TurnContext,
    tx_event: &mpsc::UnboundedSender<Event>,
    call_id: &str,
    tool_call: &ToolCall,
    tool_ctx: &ToolContext,
) -> Result<(String, bool), KernelError> {
    let tool_name = &tool_call.function.name;
    let arguments = &tool_call.function.arguments;

    // ... 审批逻辑保持不变 ...

    let tool = ctx.tools.get(tool_name)
        .ok_or_else(|| KernelError::Tool(format!("unknown tool: {tool_name}")))?;

    if !tool.capability().supports_streaming {
        let _ = tx_event.send(Event::tool_call(
            ctx.session_id.clone(),
            ctx.agent_path.clone(),
            call_id,
            tool.name(),
            arguments.clone(),
            ToolCallStatus::InProgress,
        ));
    }

    let (text, mut stream) = tool
        .execute_streaming(arguments.clone(), tool_ctx)
        .await
        .map_err(|err| KernelError::Tool(err))?;

    let mut succeeded = true;
    while let Some(item) = stream.next().await {
        match item {
            ToolStreamItem::Begin(turn_item) => {
                let _ = tx_event.send(Event::item_started(
                    ctx.session_id.clone(), ctx.turn_id.clone(), turn_item,
                ));
            }
            ToolStreamItem::End(turn_item) => {
                succeeded = match &turn_item {
                    TurnItem::ExecCommand(item) =>
                        matches!(item.status, ExecCommandStatus::Completed),
                    TurnItem::FileChange(item) =>
                        matches!(item.status, FileChangeStatus::Completed),
                    TurnItem::McpToolCall(item) =>
                        matches!(item.status, McpToolCallStatus::Completed),
                };
                let _ = tx_event.send(Event::item_completed(
                    ctx.session_id.clone(), ctx.turn_id.clone(), turn_item,
                ));
            }
            ToolStreamItem::Delta { stream, chunk } => {
                let _ = tx_event.send(Event::ExecCommandOutputDelta {
                    session_id: ctx.session_id.clone(),
                    call_id: call_id.to_string(),
                    stream,
                    chunk,
                });
            }
            ToolStreamItem::Text { content, is_error } => {
                succeeded = !is_error;
                let _ = tx_event.send(Event::tool_call_update(
                    ctx.session_id.clone(),
                    call_id,
                    Some(content),
                    Some(if is_error { ToolCallStatus::Failed } else { ToolCallStatus::Completed }),
                ));
            }
        }
    }

    Ok((text, succeeded))
}
```

- [ ] **步骤2：删除 `file_change_emitter()` 函数**

该函数及对 `ToolEmitter` 的引用全部删除。

- [ ] **步骤3：删除 `tool_events.rs`**

`ToolEmitter` 和 `ToolEventCtx` 的职责已由 `ToolStreamItem` 替代。从 `lib.rs` 移除 `mod tool_events;`。

- [ ] **步骤4：清理 import**

`turn.rs` 中删除 `ToolEmitter`、`ToolEventCtx`、`ToolDisplayOutput` 的 import，新增 `ToolStreamItem`、`ToolCapability`（从 protocol 导入）、`ExecCommandStatus`、`FileChangeStatus`、`McpToolCallStatus` 的 import（用于 `End` 成功/失败判断）。

- [ ] **步骤5：编译验证**

`cargo check -p kernel` 确保无编译错误。

---

### 任务 6：全局编译与测试

- [ ] **步骤1：全 workspace 编译**

`cargo check` 确保所有 crate 编译通过。

- [ ] **步骤2：修复编译错误**

检查是否有其他 crate（如 `acp`）直接引用了 `execute_structured` 或 `ToolEmitter`，逐一修复。

- [ ] **步骤3：运行测试**

`cargo test` 确保现有测试通过。shell 原有测试（`shell_echo_hello` 等）需要适配新的 `execute_streaming` 调用方式——改为调用 `execute_streaming`，从 stream 中收集 `Text { is_error }` 判断成功/失败。

- [ ] **步骤4：shell 增量输出手动验证**

可选：写一个简单的 shell 流式 smoke test，验证 `Delta` 事件正常产出：

```rust
#[tokio::test]
async fn shell_streaming_produces_delta_events() {
    let tool = ShellCommand::new();
    let (text, mut stream) = tool
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
            ToolStreamItem::Begin(_) => has_begin = true,
            ToolStreamItem::End(_) => has_end = true,
            ToolStreamItem::Delta { .. } => delta_count += 1,
            _ => {}
        }
    }
    assert!(has_begin);
    assert!(has_end);
    assert!(delta_count > 0, "should have at least one delta event");
    assert!(text.contains("hello"));
    assert!(text.contains("world"));
}
```

- [ ] **步骤5：清理**

确认 `crates/tools/src/output.rs` 和 `crates/kernel/src/tool_events.rs` 已删除。搜索全仓 `execute_structured` 和 `ToolEmitter` 引用确认已全部清理。

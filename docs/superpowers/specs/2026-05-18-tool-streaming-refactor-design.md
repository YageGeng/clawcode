# Tool 流式执行重构方案

## 动机

当前 Tool trait 存在以下问题：

1. **shell 命令无增量输出**：`ShellCommand` 使用 `Command::output()` 阻塞等待全部输出，30 秒超时内前端看不到任何反馈。需要引入 `ExecCommandBegin` → `ExecCommandOutputDelta` → `ExecCommandEnd` 事件流。
2. **execute_structured 混淆职责**：`execute_structured` 同时服务两个目的——返回模型文本、返回前端展示数据（`FileChanges`）。display 逻辑应该统一到流式事件中。
3. **Tool trait 无能力发现机制**：dispatch 层无法预先知道一个工具是否会产出流式事件，只能统一调 `execute_structured`。

## 核心设计

### 数据流

```
Tool::execute_streaming()
  │
  └─ 返回 Stream<Item = ToolStreamItem>
       │
       ▼
  dispatch_tool() 驱动 stream
       │
       ├─ Begin(TurnItem)  → Event::ItemStarted
       ├─ End(TurnItem)    → Event::ItemCompleted
       ├─ Delta { stream, chunk }
       │     → Event::ExecCommandOutputDelta
       │
       └─ Text { content, is_error }
              → Event::ToolCallUpdate(Completed/Failed)
             （简单工具的默认实现产出此变体）
```

### 职责边界

| 层 | 职责 |
|---|---|
| **protocol** | 定义 `ToolStreamItem`、`TurnItem::ExecCommand`、`Event::ExecCommandOutputDelta`、`ToolCapability` |
| **tools** | Tool trait 增加 `capability()` 和 `execute_streaming()`，删除 `execute_structured()` |
| **kernel dispatch** | 统一调 `execute_streaming`，非流式工具先发 `ToolCall(InProgress)`，驱动 stream 并转发事件 |
| **shell 实现** | 改为流式：spawn 子进程，边读 stdout/stderr pipe 边产出 `ToolStreamItem::Delta` |

---

## 详细改动

### 1. `crates/protocol/src/item.rs` — 新增 ExecCommandItem

```rust
/// 结构化 turn item 类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnItem {
    FileChange(FileChangeItem),
    McpToolCall(McpToolCallItem),
    ExecCommand(ExecCommandItem),   // 新增
}

/// shell/exec 命令的生命周期 item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct ExecCommandItem {
    /// 对应 ExecCommandBegin/End 的 call_id
    pub id: String,
    /// 命令行参数数组，首元素为程序名
    pub command: Vec<String>,
    /// 工作目录
    pub cwd: PathBuf,
    /// 当前生命周期状态
    pub status: ExecCommandStatus,
    /// 完整 stdout（Completed/Failed 时填充）
    #[builder(default, setter(strip_option))]
    pub stdout: Option<String>,
    /// 完整 stderr（Completed/Failed 时填充）
    #[builder(default, setter(strip_option))]
    pub stderr: Option<String>,
    /// 退出码（Completed/Failed 时填充）
    #[builder(default, setter(strip_option))]
    pub exit_code: Option<i32>,
    /// 执行耗时毫秒（Completed/Failed 时填充）
    #[builder(default, setter(strip_option))]
    pub duration_ms: Option<u64>,
}

/// ExecCommand 生命周期状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    /// 命令已开始执行
    InProgress,
    /// 命令执行成功（exit_code == 0）
    Completed,
    /// 命令执行失败（exit_code != 0 或异常）
    Failed,
    /// 命令被用户拒绝
    Declined,
}

```

### 2. `crates/protocol/src/event.rs` — 新增 delta 事件 + ToolStreamItem

```rust
pub enum Event {
    // ... 现有 variant 不变 ...

    /// shell/exec 命令的增量输出（stdout 或 stderr 片段）
    ExecCommandOutputDelta {
        session_id: SessionId,
        call_id: String,
        stream: ExecOutputStream,
        /// 原始字节，序列化为 base64
        chunk: Vec<u8>,
    },
}

/// 区分增量输出来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}
```

新增 `ToolStreamItem` —— 工具流式产出的统一类型：

```rust
/// 流式工具产出的单项。dispatch 层驱动 stream 并转换为 Event。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolStreamItem {
    /// 生命周期开始 → dispatch 转换为 Event::ItemStarted
    Begin(TurnItem),
    /// 生命周期结束 → dispatch 转换为 Event::ItemCompleted
    End(TurnItem),
    /// 增量输出 → dispatch 转换为 Event::ExecCommandOutputDelta
    Delta {
        stream: ExecOutputStream,
        chunk: Vec<u8>,
    },
    /// 模型文本结果 → dispatch 转换为 Event::ToolCallUpdate(Completed/Failed)
    /// 简单工具的 execute_streaming 默认实现产出此变体
    Text {
        /// 发送给模型的文本内容
        content: String,
        /// true 表示工具执行失败，content 为错误信息
        is_error: bool,
    },
}
```

### 3. `crates/protocol/src/tool.rs` — 新增 ToolCapability

```rust
/// 工具能力描述，用于 dispatch 层选择执行路径
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolCapability {
    /// 是否支持流式执行（产出 ToolStreamItem 流）
    pub supports_streaming: bool,
}
```

### 4. `crates/tools/src/lib.rs` — Tool trait 重构

```rust
use std::pin::Pin;
use futures::stream::Stream;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// 返回工具能力描述。默认不支持流式。
    fn capability(&self) -> ToolCapability {
        ToolCapability::default()
    }

    /// 是否需要用户审批
    fn needs_approval(&self, arguments: &serde_json::Value, ctx: &ToolContext) -> bool {
        true
    }

    /// 简单执行：返回发送给模型的文本。
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<String, String>;

    /// 流式执行：返回模型文本 + 事件流。
    /// 默认实现调用 execute()，将结果包装为 `ToolStreamItem::Text`。
    /// 成功：`Text { content, is_error: false }`；失败：`Text { content: err, is_error: true }`。
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

**删除**：`execute_structured()` 方法及 `ToolExecutionResult`、`ToolDisplayOutput` 类型。

**删除**：`ToolRegistry::execute_structured()` 方法，只保留 `execute()` 和新增 `execute_streaming()`。

### 5. `crates/kernel/src/turn.rs` — dispatch_tool 重构

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

    // ... 审批逻辑不变 ...

    let tool = ctx.tools.get(tool_name)
        .ok_or_else(|| KernelError::Tool(format!("unknown tool: {tool_name}")))?;

    // 非流式工具：先发 ToolCall(InProgress)，再调 execute_streaming（默认产出 Text）
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
                    ctx.session_id.clone(),
                    ctx.turn_id.clone(),
                    turn_item,
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
                    ctx.session_id.clone(),
                    ctx.turn_id.clone(),
                    turn_item,
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
                    Some(if is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    }),
                ));
            }
        }
    }

    Ok((text, succeeded))
}
```

**删除**：`file_change_emitter()` 函数和 `ToolEmitter`（`crates/kernel/src/tool_events.rs`），其职责由 `ToolStreamItem::Begin`/`End` 替代。

### 6. shell 工具流式改造

```rust
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[async_trait]
impl Tool for ShellCommand {
    fn capability(&self) -> ToolCapability {
        ToolCapability { supports_streaming: true }
    }

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String> {
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
        let cwd = work_dir.clone();

        // 用于把 OutputDelta 从子进程读取循环传到 stream
        let (delta_tx, delta_rx) = mpsc::unbounded_channel();

        // 初始 item: Begin
        let begin = ToolStreamItem::Begin(TurnItem::ExecCommand(
            ExecCommandItem::builder()
                .id(String::new()) // call_id 由 dispatch 在 ItemStarted 中注入
                .command(command.clone())
                .cwd(cwd.clone())
                .status(ExecCommandStatus::InProgress)
                .build(),
        ));

        // 最终结果通过 oneshot 回传
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = run_command_with_streaming(
                &command_str, &work_dir, delta_tx,
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let _ = result_tx.send((result, duration_ms));
        });

        let delta_stream = UnboundedReceiverStream::new(delta_rx);
        let stream = futures::stream::once(async { begin })
            .chain(delta_stream);

        // 等子进程结束，构建最终 End item + model_output
        let (exec_result, duration_ms) = result_rx
            .await
            .map_err(|_| "internal error: shell task dropped".to_string())?;

        let (model_text, end_item) = build_shell_result(
            &command, &cwd, exec_result, duration_ms,
        );
        // end_item: ToolStreamItem::End(TurnItem::ExecCommand { status: Completed/Failed, .. })

        // stream 拼上 End item
        let stream = stream
            .chain(futures::stream::once(async { end_item }));

        Ok((model_text, Box::pin(stream)))
    }
}
```

#### 实现注意事项

`execute_streaming` 返回 `(String, Stream)` 存在时序：`String`（模型文本）和 `End` item 都需要等子进程结束。但因为两者都在同一个 `result_rx` 等待之后生成，不存在竞态——`Build shell result` 构造 `model_text` 和 `end_item`，`End` item 作为 stream 的最后一项追加。

dispatch 层正确行为：收到 `End` 时发 `ItemCompleted` 并标记 `succeeded`，函数返回 `(text, succeeded)`。前端通过 `ItemCompleted` 获得完整输出，`text` 发送给模型。

### 7. apply_patch / edit 适配

这两个工具当前 override `execute_structured` 返回 `FileChanges`。改为实现 `execute_streaming`，不实现增量输出，只产出 lifecycle item：

```rust
// apply_patch
async fn execute_streaming(&self, arguments, ctx)
    -> Result<(String, Pin<Box<dyn Stream<Item = ToolStreamItem> + Send>>), String>
{
    let begin = ToolStreamItem::Begin(TurnItem::FileChange(
        FileChangeItem::builder()
            .id(String::new())
            .title("Apply patch".into())
            .changes(vec![])
            .status(FileChangeStatus::InProgress)
            .build(),
    ));

    let result = self.do_apply(arguments, ctx).await?;

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

这样 apply_patch/edit 的 display 行为与之前完全一致，只是从 `execute_structured` 的返回值改成了 stream 中的 `Begin`/`End` item。

---

## 删除清单

| 文件 | 删除内容 |
|---|---|
| `crates/tools/src/output.rs` | `ToolDisplayOutput`、`ToolExecutionResult` 类型 |
| `crates/tools/src/lib.rs` | `Tool::execute_structured()` 方法、`ToolRegistry::execute_structured()` 方法 |
| `crates/kernel/src/tool_events.rs` | `ToolEmitter` 和 `ToolEventCtx` 整个文件（职责由 `ToolStreamItem::Begin`/`End` 替代） |
| `crates/kernel/src/turn.rs` | `file_change_emitter()` 函数 |
| `crates/protocol/src/item.rs` | `TurnItem::is_terminal()`（不再需要，Begin/End 已显式区分阶段） |

---

## 实施顺序

1. **protocol**：新增 `ExecCommandItem`、`ExecCommandStatus`、`ExecOutputStream`、`Event::ExecCommandOutputDelta`、`ToolStreamItem`、`ToolCapability`
2. **tools**：Tool trait 增加 `capability()`、`execute_streaming()`，删除 `execute_structured()`；`ToolExecutionResult`/`ToolDisplayOutput` 标记 deprecated
3. **kernel dispatch**：`dispatch_tool` 增加 `supports_streaming` 分支；删除 `tool_events.rs`
4. **apply_patch / edit**：改为实现 `execute_streaming`
5. **shell**：改为流式执行，产出 `Delta`
6. **清理**：删除 `output.rs` 和 `tool_events.rs`，更新所有 `execute_structured` 调用点

# TurnItem 与 ToolEmitter 机制设计

## 背景

当前 clawcode 的工具执行链路比较直接：`crates/kernel/src/turn.rs` 中的 `dispatch_tool` 负责审批、发送 `Event::ToolCall`、执行工具、再发送 `Event::ToolCallUpdate`。这个模型可以表达普通文本输出，但不适合表达更丰富的工具展示语义，例如文件变更 diff、MCP tool call 结构化结果、terminal、后续的 image/resource 等。

Codex 的实现提供了更清晰的分层：

1. 核心协议使用 `TurnItem` 表达回合内发生的结构化事项。
2. 事项生命周期通过 `ItemStarted` / `ItemCompleted` 表达。
3. 工具执行侧通过 `ToolEmitter` 统一发出生命周期事件。
4. 客户端桥接层再把这些结构化事项映射为 ACP 的 `ToolCall` / `ToolCallUpdate` / `ToolCallContent`。

需要注意的是，Codex 并没有一个通用的 `TurnItem::ToolCall`。它为不同类别保留不同 item：

- `TurnItem::FileChange`：用于 `apply_patch` 一类文件变更。
- `TurnItem::McpToolCall`：用于 MCP tool call。
- shell / exec 仍通过 `ExecCommandBegin` / `ExecCommandEnd` 事件表达。
- dynamic tool 有独立的 `DynamicToolCallRequest` / `DynamicToolCallResponse`。

clawcode 第一版应该借鉴这个方向，而不是提前抽象一个过宽的通用 tool call item。

## 目标

1. 在 `protocol` 层引入最小可用的 `TurnItem` 生命周期事件。
2. 在 `kernel` 层引入 `ToolEmitter`，让工具执行生命周期从 `dispatch_tool` 中解耦。
3. 第一阶段支持 `TurnItem::FileChange`，用于 `apply_patch` 和后续 `edit` 的最终文件状态展示。
4. 预留 `TurnItem::McpToolCall` 的协议模型，使后续 MCP 工具展示可以自然迁移。
5. 保留现有 `Event::ToolCall` / `Event::ToolCallUpdate`，避免一次性重写全部 TUI/ACP 行为。
6. ACP 层负责把 `TurnItem::FileChange` 转成 `ToolCallContent::Diff`。
7. 模型上下文继续接收工具的简短文本结果，不把完整 diff 注入下一轮 LLM 输入。

## 非目标

1. 第一版不新增通用 `TurnItem::ToolCall`。
2. 第一版不迁移 assistant message、reasoning、plan 到 `TurnItem`。
3. 第一版不迁移 shell / exec 到 `TurnItem`。
4. 第一版不改变 `Event::ToolCallUpdate` 的字段结构。
5. 第一版不实现完整 Codex legacy event 兼容层。
6. 第一版不实现 TUI 完整 diff renderer，只保证 ACP diff 内容可以传递到 TUI；TUI 渲染可以单独设计。
7. 第一版不把 MCP 执行链路迁移到 `McpToolCallItem`，只定义协议形态并保留后续接入点。

## 现状约束

### 当前工具执行链路

当前 `dispatch_tool` 的职责过重：

```text
dispatch_tool
  -> 判断审批
  -> 发送 Event::ToolCall(Pending)
  -> 等待审批
  -> 发送 Event::ToolCall(InProgress)
  -> ToolRegistry::execute(...)
  -> 发送 Event::ToolCallUpdate(output_delta, status)
```

这个链路没有工具类别专用的展示结构。任何工具结果都只能落到 `output_delta: Option<String>`。

### 当前 ACP 转换链路

ACP 层把 `Event::ToolCallUpdate.output_delta` 包装为：

```text
ToolCallContent::Content(ContentBlock::Text(...))
```

它已经依赖 ACP schema 的 `ToolCallContent::Diff(Diff)`，但目前没有内部事件能把 diff 传到这里。

### 当前 TUI 链路

TUI 的 `tool_content_text` 已经能匹配 `ToolCallContent::Diff(_)`，但现在只降级为 `"[diff]"` 文本。这说明 rich content 的入口存在，只是没有完整渲染。

## 协议设计

新增模块建议放在：

```text
crates/protocol/src/item.rs
```

并在 `crates/protocol/src/lib.rs` 中导出。

### TurnItem

第一版只引入两个结构化 item：

```rust
pub enum TurnItem {
    FileChange(FileChangeItem),
    McpToolCall(McpToolCallItem),
}
```

不加入 `ToolCallItem`。普通工具仍走现有 `Event::ToolCall` / `Event::ToolCallUpdate`。

### Item 生命周期事件

在 `protocol` 层新增 turn id newtype：

```rust
pub struct TurnId(pub String);
```

`TurnId` 应与 `SessionId` 一样承担协议边界上的类型区分，避免把 session id、turn id、tool call id 都当作裸 `String` 传递。

现有 `TurnContext::turn_id` 也应从 `String` 迁移为 `TurnId`。store 层如果暂时仍以字符串序列化，可以在记录构造处显式调用 `turn_id.to_string()` 或 `String::from(&turn_id)`，不要把裸字符串继续向 kernel/protocol 传播。

在 `crates/protocol/src/event.rs` 中新增：

```rust
Event::ItemStarted {
    session_id: SessionId,
    turn_id: TurnId,
    item: TurnItem,
}

Event::ItemCompleted {
    session_id: SessionId,
    turn_id: TurnId,
    item: TurnItem,
}
```

这两个事件表达结构化 item 的生命周期。`session_id` 标识线程，`turn_id` 标识本次模型回合。clawcode 当前 `TurnContext` 已经有稳定 turn id，item 事件必须携带它，避免恢复、回放、子 agent 转发或并发 turn 诊断时只能按 session 粗粒度归因。它们不替代现有 tool call 事件，第一版只由支持结构化展示的工具发出。

### FileChangeItem

`FileChangeItem` 表达一次文件变更工具调用的最终结果。

```rust
pub struct FileChangeItem {
    pub id: String,
    pub title: String,
    pub changes: Vec<FileChange>,
    pub status: FileChangeStatus,
    pub model_output: Option<String>,
}
```

字段含义：

- `id`：工具调用 id，和现有 tool call id 保持一致。
- `title`：面向 UI 的标题，例如 `Apply patch` 或 `Edit src/main.rs`。
- `changes`：本次工具调用造成的最终文件变更。
- `status`：`InProgress`、`Completed`、`Failed`、`Declined`。
- `model_output`：工具返回给模型的简短文本 summary，只用于回放和诊断，不用于构造 diff。

### FileChange

`FileChange` 使用最终文件状态，而不是 hunk 或 unified diff。

```rust
pub struct FileChange {
    pub path: PathBuf,
    pub old_text: Option<String>,
    pub new_text: String,
}
```

映射规则：

| 操作 | `old_text` | `new_text` |
|---|---|---|
| 新增文件 | `None` | 新文件完整内容 |
| 修改文件 | `Some(修改前内容)` | 修改后完整内容 |
| 删除文件 | `Some(删除前内容)` | 空字符串 |
| move/rename | 第一版使用最终路径作为 `path`，`old_text/new_text` 表达内容变化 |

不采用 Codex 的 `FileChange::Update { unified_diff }`，原因是 clawcode 当前目标是最终状态展示，而 ACP `Diff` 正好以 `old_text/new_text` 为核心。

### McpToolCallItem

第一版只定义协议模型，不接入执行链路。

```rust
pub struct McpToolCallItem {
    pub id: String,
    pub server: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub status: McpToolCallStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}
```

这个结构对齐 Codex 的 `McpToolCallItem` 思路，但先避免引入 Codex 的完整 `CallToolResult` 类型。后续 MCP crate 如果已经有稳定结果类型，可以再把 `result` 收紧为强类型。

## Kernel 设计

新增模块建议放在：

```text
crates/kernel/src/tool_events.rs
```

### ToolEventCtx

`ToolEventCtx` 保存发事件所需的最小上下文：

```rust
pub(crate) struct ToolEventCtx<'a> {
    pub session_id: &'a SessionId,
    pub turn_id: &'a TurnId,
    pub agent_path: &'a AgentPath,
    pub call_id: &'a str,
    pub tx_event: &'a mpsc::UnboundedSender<Event>,
}
```

第一版不引入 Codex 的 session/turn 对象引用。clawcode 当前 kernel 的事件出口就是 `mpsc::UnboundedSender<Event>`，所以 emitter 应该围绕它设计。`turn_id` 从 `TurnContext::turn_id` 传入 emitter，所有 item 生命周期事件都复用同一个 `TurnId`。

### ToolEmitter

第一版 `ToolEmitter` 不做通用 tool 抽象，只为结构化展示工具服务。

```rust
pub(crate) enum ToolEmitter {
    FileChange {
        title: String,
        arguments: serde_json::Value,
    },
}
```

生命周期方法：

```rust
impl ToolEmitter {
    pub(crate) fn begin(&self, ctx: ToolEventCtx<'_>);

    pub(crate) fn complete_file_change(
        &self,
        ctx: ToolEventCtx<'_>,
        changes: Vec<FileChange>,
        model_output: String,
    );

    pub(crate) fn fail_file_change(
        &self,
        ctx: ToolEventCtx<'_>,
        model_output: String,
    );
}
```

`begin` 发送：

```text
Event::ItemStarted {
  turn_id,
  item: TurnItem::FileChange(FileChangeItem {
    status: InProgress,
    changes: [],
    model_output: None,
  })
}
```

`complete_file_change` 发送：

```text
Event::ItemCompleted {
  turn_id,
  item: TurnItem::FileChange(FileChangeItem {
    status: Completed,
    changes,
    model_output: Some(model_output),
  })
}
```

`fail_file_change` 发送：

```text
Event::ItemCompleted {
  turn_id,
  item: TurnItem::FileChange(FileChangeItem {
    status: Failed,
    changes: [],
    model_output: Some(model_output),
  })
}
```

### 与 dispatch_tool 的关系

`dispatch_tool` 保留现有普通事件：

```text
Event::ToolCall
Event::ToolCallUpdate
```

对于支持结构化 item 的工具，额外创建 `ToolEmitter`：

```text
apply_patch/edit
  -> Event::ToolCall(InProgress)
  -> ToolEmitter::FileChange.begin(turn_id)
  -> execute tool
  -> Event::ToolCallUpdate(text summary, status)
  -> ToolEmitter::FileChange.complete_file_change(turn_id, ...)
```

第一版允许 `ToolCallUpdate` 和 `ItemCompleted(FileChange)` 并存。ACP 层需要避免重复展示同一段文本：`ToolCallUpdate` 展示 summary，`ItemCompleted(FileChange)` 展示 diff。

## 工具结果设计

为了让 `ToolEmitter` 能拿到文件变更，工具执行结果需要从纯字符串扩展为结构化结果。

### ToolExecutionResult

在 `crates/tools/src/lib.rs` 新增：

```rust
pub struct ToolExecutionResult {
    pub model_output: String,
    pub display: ToolDisplayOutput,
}

pub enum ToolDisplayOutput {
    None,
    FileChanges(Vec<protocol::FileChange>),
}
```

`Tool` trait 的长期目标：

```rust
async fn execute(
    &self,
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<ToolExecutionResult, String>;
```

为了降低第一阶段改动风险，可以采用兼容迁移：

1. 先新增 `execute_structured` 默认方法，默认调用现有 `execute` 并包装为 `ToolExecutionResult`。
2. `ToolRegistry::execute` 新增结构化版本。
3. `apply_patch` 优先实现结构化版本。
4. 普通工具继续只返回文本。

这样可以避免一次性修改所有内置工具。

### apply_patch 文件变更来源

`apply_patch` 必须展示最终文件状态，不展示 hunk 片段。

推荐在 `crates/tools/src/builtin/fs/patch.rs` 的 prepare 阶段保留每个 hunk 的旧内容和最终内容：

```text
parse patch
prepare hunks
read old content
derive new content
write all prepared hunks
build Vec<FileChange>
return ToolExecutionResult {
  model_output: summary,
  display: FileChanges(changes),
}
```

关键要求：

1. 只有工具执行成功后才返回 file changes。
2. 执行失败时不返回 diff，避免展示未完成状态为成功变更。
3. 多个 hunk 影响同一个文件时，最终只展示一条从执行前到执行后的变更。
4. 删除文件必须在执行前保留旧内容。

### edit 文件变更来源

`edit` 后续接入同一机制：

```text
read old content
apply replacement in memory
write new content
return FileChange { path, old_text: Some(old), new_text }
```

不展示 `oldString` / `newString` 参数本身，只展示文件最终 diff。

## ACP 设计

`crates/acp/src/agent.rs` 新增对 `Event::ItemStarted` 和 `Event::ItemCompleted` 的处理。

### FileChange started

`ItemStarted(FileChange)` 映射为：

```text
SessionUpdate::ToolCall(
  ToolCall {
    id,
    title,
    kind: Edit,
    status: InProgress,
  }
)
```

如果已有同 id 的普通 `Event::ToolCall`，ACP 层可以只发送 `ToolCallUpdate` 更新 kind/title/status，避免重复创建 cell。第一版更简单的做法是：

1. `Event::ToolCall` 仍创建基础 cell。
2. `ItemStarted(FileChange)` 发送 `ToolCallUpdate`，把 title/kind/status 修正为 edit 类展示。

这样不会和当前 TUI `pending_tool_call_cell` 行为冲突。

### FileChange completed

`ItemCompleted(FileChange)` 映射为：

```text
SessionUpdate::ToolCallUpdate(
  ToolCallUpdate {
    status,
    content: Vec<ToolCallContent::Diff>,
    raw_output: model_output,
  }
)
```

每个 `FileChange` 映射为：

```rust
ToolCallContent::Diff(
    Diff::new(change.path, change.new_text)
        .old_text(change.old_text)
)
```

失败时如果没有 `changes`，只更新 status 和 raw output，不发送 diff。

## TUI 影响

第一版 TUI 可以保持现状：

```text
ToolCallContent::Diff(_) -> "[diff]"
```

这能证明协议链路通了。后续单独实现 diff renderer 时，只需要改 TUI 的 `ToolCallCell` 内容模型和渲染逻辑，不需要再改 kernel/tool/ACP 的事件语义。

建议后续 TUI renderer 使用 ACP `Diff` 的 `old_text/new_text` 生成 unified diff，而不是依赖工具 stdout。

## 持久化与回放

第一版不要求把 `ItemStarted/ItemCompleted` 写入会话持久化历史。当前 session 持久化主要关注模型上下文消息，工具展示事件可以暂时作为运行时 UI 事件。

后续如果需要完整 replay，应把 `TurnItem` 作为 display event 写入 store，而不是塞进 LLM message history。届时 `TurnId` 是 display event 与 `TurnContextRecord`、`MessageRecord` 对齐的主关联键，`FileChangeItem.id` 仍然只表示工具调用 id。

## 错误处理

1. `apply_patch` 解析失败：不发送 `FileChangeItem` completed diff，只保留现有 failed `ToolCallUpdate`。
2. `apply_patch` 写入失败：不发送成功 diff；如果未来要展示部分成功变更，需要单独设计 partial status。
3. 文件读取旧内容失败：工具本身应失败，避免展示不完整 diff。
4. ACP diff 转换失败：记录 warn，并降级为文本 summary。
5. TUI 不支持 diff renderer：显示 `"[diff]"`，不阻断工具执行。

## 测试要求

### protocol

1. `TurnId` serde roundtrip，并验证它不会和裸 `String` 调用点混用。
2. `TurnItem::FileChange` serde roundtrip。
3. `ItemStarted` / `ItemCompleted` serde roundtrip，并断言 `turn_id` 保真。
4. `FileChange` 保留 `old_text: None`、`old_text: Some`、`new_text: ""` 三种情况。

### tools

1. `apply_patch` 新增文件返回 `FileChanges`，`old_text = None`。
2. `apply_patch` 修改文件返回最终完整 `old_text/new_text`。
3. `apply_patch` 删除文件返回删除前内容和空 `new_text`。
4. `apply_patch` 多 hunk 修改同一文件时只返回一条最终状态变更。
5. `apply_patch` 失败时不返回 `FileChanges`。

### kernel

1. `dispatch_tool` 对 `apply_patch` 发送 `ItemStarted(FileChange)`，事件中的 `TurnId` 等于当前 `TurnContext::turn_id`。
2. 成功后发送 `ItemCompleted(FileChange)`，事件中的 `TurnId` 等于当前 `TurnContext::turn_id`，且仍发送现有 `ToolCallUpdate` 文本 summary。
3. 普通工具不发送 `ItemStarted/ItemCompleted`。

### ACP

1. `ItemCompleted(FileChange)` 转成 `ToolCallContent::Diff`。
2. 删除文件 diff 使用 `new_text = ""`。
3. 失败状态不发送 diff。

### TUI

1. 当前阶段至少确认收到 `Diff` 不会 panic。
2. 后续 diff renderer spec 中再覆盖具体显示行。

## 分阶段实施

### Phase 1: 协议与事件骨架

新增 `TurnItem`、`FileChangeItem`、`McpToolCallItem`、`ItemStarted`、`ItemCompleted`。

验收标准：

- protocol serde 测试通过。
- 现有事件不受影响。

### Phase 2: 工具结构化结果

新增 `ToolExecutionResult` 和兼容迁移方法。先让普通工具默认返回 `ToolDisplayOutput::None`。

验收标准：

- 所有现有工具测试通过。
- `ToolRegistry` 可以同时提供旧文本执行和新结构化执行。

### Phase 3: apply_patch 接入 FileChange

`apply_patch` 返回最终文件状态变更。

验收标准：

- add/update/delete/multi-hunk 测试覆盖。
- 模型看到的文本 summary 保持兼容。

### Phase 4: ToolEmitter 接入 kernel

新增 `ToolEmitter::FileChange`，在 `dispatch_tool` 中仅对 `apply_patch` 启用。

验收标准：

- `apply_patch` 发送 `ItemStarted/ItemCompleted`。
- 普通工具事件序列不变。

### Phase 5: ACP 转换

ACP 将 `FileChangeItem` 映射为 `ToolCallContent::Diff`。

验收标准：

- ACP 单元测试能观察到 diff content。
- 现有 `ToolCallUpdate` 文本展示不回退。

### Phase 6: edit 接入

`edit` 使用同一 `ToolExecutionResult::FileChanges` 机制。

验收标准：

- `edit` diff 不展示 `oldString/newString` 参数。
- ACP 输出和 `apply_patch` 一致。

## 设计决策

1. **不新增通用 `TurnItem::ToolCall`**  
   Codex 没有这个 item。过早抽象会让 shell、MCP、file change、dynamic tool 的不同语义被迫塞进一个结构，后续会变得模糊。

2. **`FileChange` 保存最终内容，不保存 unified diff**  
   ACP `Diff` 本身使用 `old_text/new_text`。保存最终内容可以避免 UI 重新读取文件，也能准确表达删除文件。

3. **保留 `Event::ToolCallUpdate`**  
   现有 TUI/ACP 都依赖它。第一版新增结构化事件，不破坏旧链路。

4. **`ToolEmitter` 先服务结构化展示，不接管所有工具**  
   这能避免大范围重构。等 `FileChange` 跑通后，再评估 shell/MCP 是否迁移。

5. **MCP item 先定义，后接入**  
   MCP 的结果类型和运行链路比 file change 更复杂。先稳定协议形态，避免阻塞当前 diff 目标。

## 开放问题

1. `ItemStarted(FileChange)` 是否应该由 ACP 创建新的 `ToolCall`，还是只更新已有 `ToolCall`？推荐先更新已有 cell，避免重复展示。
2. move/rename 是否需要显式 `old_path` 字段？第一版不加，后续如果 UI 需要展示 rename，再扩展 `FileChange`。
3. 失败但部分文件已修改时是否展示 partial diff？第一版不展示，避免误导用户；后续可引入 `Partial` 状态。

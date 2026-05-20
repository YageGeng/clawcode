# Tool Arguments 流式处理与 apply_patch 预览设计

## 背景

当前 clawcode 已经支持两类流式能力：

1. Provider 层会把模型生成中的工具调用参数片段转成 `LlmStreamEvent::ToolCallDelta`，kernel 再转发为 `Event::ToolCallDelta`。
2. Tool 执行阶段支持 `Tool::execute_streaming()`，用于在工具真正执行时发出 `ToolStreamItem`，例如 shell 输出 delta 或 `apply_patch` 最终 `FileChangeItem`。

这两类能力发生在不同阶段。`ToolCallDelta` 发生在模型还在生成工具参数时；`execute_streaming()` 发生在完整工具调用已经生成并进入执行阶段后。

Codex 的 `StreamingPatchParser` 解决的是第一类问题：模型正在输出 `apply_patch` 参数时，前端可以提前看到 patch 预览。它不会替代最终 patch 校验和执行，只提供 UI 进度。clawcode 需要复刻这个能力，但本项目的 `apply_patch` 参数是 JSON schema 形式：

```json
{"patchText":"*** Begin Patch\n..."}
```

Codex 上游则更接近 freeform/raw patch 输入。因此本项目除了需要 Codex 风格的 patch line parser，还需要一层 JSON `patchText` 增量提取器。

本设计明确不把 `apply_patch` 改成 Codex 的 freeform/raw patch。原因是 clawcode 当前 provider、tool schema、审批展示、历史持久化和执行入口都以 JSON arguments 为边界。第一版只在 Codex streaming parser 前增加 JSON `patchText` 提取层，以获得相同的 patch 预览能力，同时保持现有工具协议稳定。

## 目标

1. 在 `Tool` trait 中引入统一的 arguments 流式处理 hook：`arguments_consumer()`。
2. 让 kernel 在收到工具参数 delta 时，把 delta 同步交给对应 tool 的 arguments consumer。
3. 第一阶段只为 `apply_patch` 实现 arguments consumer。
4. 将 `apply_patch` 的文件结构从单文件迁移为模块目录：

```text
crates/tools/src/builtin/fs/apply_patch/mod.rs
crates/tools/src/builtin/fs/apply_patch/stream_parser.rs
```

5. 在 `stream_parser.rs` 中实现 Codex 风格的 `StreamingPatchParser`，并新增本项目所需的 `PatchTextDeltaExtractor`。
6. 保留现有 `apply_patch` 最终执行语义：完整参数到达后仍通过现有解析、校验、prepare/write 和最终 `FileChangeItem` 事件展示真实文件状态。

## 非目标

1. 第一版不把 `apply_patch` 改成 freeform tool。
2. 第一版不改变 `apply_patch` 对模型暴露的 JSON schema，仍使用 `patchText` 字段。
3. 第一版不把 patch preview 混入现有 `FileChangeItem`，因为 preview 只有 hunk 信息，不知道文件旧完整内容。
4. 第一版不实现完整 TUI rich diff renderer，只保证协议事件可以表达 patch preview。
5. 第一版不让 preview parse 失败阻断最终工具执行；最终执行仍以完整参数解析和校验为准。
6. 第一版不为 `edit`、MCP 或 shell 实现 arguments consumer，只预留通用接口。

## 现状约束

### Provider 参数 delta

Provider 层已经产出统一事件：

```rust
LlmStreamEvent::ToolCallDelta {
    internal_call_id,
    id,
    content,
}
```

其中 `content` 是 `ToolCallDeltaContent::Name(name)` 或 `ToolCallDeltaContent::Delta(delta)`。kernel 当前只把它转发为 `Event::ToolCallDelta`，不解析工具参数内容。

### Tool 执行流

`Tool::execute_streaming()` 已经用于工具执行阶段。`apply_patch` 当前执行后一次性产出：

```text
Begin(FileChangeItem InProgress)
End(FileChangeItem Completed with final FileChange list)
```

这个结果表示最终文件状态，适合 ACP diff 和最终 UI 展示，但不适合模型输出参数时的 patch preview。

### apply_patch 单文件过大

`crates/tools/src/builtin/fs/patch.rs` 当前同时包含工具定义、最终 patch parser、应用逻辑、辅助匹配函数和测试。引入 streaming parser 后继续堆在同一文件会让边界更差，因此需要先迁移为模块目录。

## 核心设计

### 总体数据流

```text
Provider stream
  -> LlmStreamEvent::ToolCallDelta(Name)
       -> kernel 创建 tool.arguments_consumer()
  -> LlmStreamEvent::ToolCallDelta(Delta)
       -> kernel 保留现有 Event::ToolCallDelta 转发
       -> kernel 调用 ToolArgumentsConsumer::consume_delta()
       -> consumer 产出 ToolArgumentsStreamItem::PatchPreview
       -> kernel 转成 Event::PatchApplyUpdated

Provider emits full ToolCall
  -> kernel 调用 ToolArgumentsConsumer::finish()
  -> kernel flush pending preview event
  -> dispatch_tool()
  -> Tool::execute_streaming()
  -> 最终 FileChangeItem 生命周期事件
```

### 职责边界

| 层 | 职责 |
|---|---|
| `protocol` | 定义 patch preview 事件与数据结构 |
| `tools::Tool` | 暴露可选 `arguments_consumer()` hook |
| `tools::apply_patch::stream_parser` | 提取 JSON `patchText` delta，解析裸 patch 增量，生成 preview changes |
| `kernel::turn` | 管理 call id 到 consumer 的映射，把 provider delta 分发给 consumer，并转发 consumer 产物 |
| `apply_patch` 最终执行 | 保持现有完整解析、校验和落盘逻辑 |

## 协议设计

### PatchPreviewChange

新增 preview 专用类型，建议放在 `crates/protocol/src/item.rs` 或新模块 `crates/protocol/src/patch.rs`。第一版可以放在 `item.rs`，减少模块切分。

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatchPreviewChange {
    Add {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        move_path: Option<PathBuf>,
        old_text: String,
        new_text: String,
    },
}
```

`Update` 的 `old_text` / `new_text` 是从 patch hunk 构造出的局部片段，不是完整文件内容。它只用于参数流预览；最终执行结果仍由 `FileChangeItem` 提供完整文件状态。

### ToolArgumentsStreamItem

新增 arguments consumer 的内部产物：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolArgumentsStreamItem {
    PatchPreview {
        call_id: String,
        changes: Vec<PatchPreviewChange>,
    },
}
```

该类型与 `ToolStreamItem` 分离。`ToolStreamItem` 属于工具执行阶段，`ToolArgumentsStreamItem` 属于工具参数生成阶段。

### Event::PatchApplyUpdated

新增 kernel 到 frontend 的事件：

```rust
Event::PatchApplyUpdated {
    session_id: SessionId,
    call_id: String,
    changes: Vec<PatchPreviewChange>,
}
```

该事件语义是“apply_patch 参数流预览已更新”，不是“文件已经变更”。前端应把它渲染为 pending/preview 状态，并在最终 `FileChangeItem` 完成后用最终 diff 替换或补充展示。

## Tool trait 设计

在 `crates/tools/src/lib.rs` 中新增 trait：

```rust
pub trait ToolArgumentsConsumer: Send {
    /// Consume one provider-emitted argument delta and return preview items.
    fn consume_delta(
        &mut self,
        call_id: &str,
        delta: &str,
    ) -> Vec<protocol::ToolArgumentsStreamItem>;

    /// Flush pending preview state when argument streaming completes.
    fn finish(
        &mut self,
        call_id: &str,
    ) -> Result<Vec<protocol::ToolArgumentsStreamItem>, String>;
}
```

在 `Tool` trait 中新增默认方法：

```rust
fn arguments_consumer(&self) -> Option<Box<dyn ToolArgumentsConsumer>> {
    None
}
```

默认返回 `None`，所以现有工具行为不变。`apply_patch` 返回 `Some(...)`。

## Kernel 接入设计

### Consumer 映射

`turn.rs` 在处理一次模型流时维护：

```rust
HashMap<String, Box<dyn ToolArgumentsConsumer>>
```

key 使用与现有 `Event::ToolCallDelta` 一致的 call id 规则：

```text
if provider id is empty:
  use internal_call_id
else:
  use provider id
```

这样 preview 事件、tool call delta 事件和最终 tool call 事件能用同一个 `call_id` 关联。

### Name delta

收到 `ToolCallDeltaContent::Name(name)` 时：

1. 继续发送现有 `Event::ToolCallDelta`。
2. 通过 `ctx.tools.get(&name)` 查找 tool。
3. 如果 tool 存在且 `arguments_consumer()` 返回 `Some`，把 consumer 插入 map。
4. 如果同一 call id 已有 consumer，保留已有 consumer，避免重复初始化丢失状态。

### Argument delta

收到 `ToolCallDeltaContent::Delta(delta)` 时：

1. 继续发送现有 `Event::ToolCallDelta`。
2. 查找该 call id 的 consumer。
3. 调用 `consume_delta(call_id, &delta)`。
4. 将返回的 `ToolArgumentsStreamItem` 转成 `Event::PatchApplyUpdated`。

如果 consumer 内部解析失败，第一版建议 consumer 自行进入 failed/silent 状态并返回空列表。这样不会干扰最终工具调用。

### 完整 ToolCall 到达

在执行 `dispatch_tool()` 前：

1. 用最终 `tool_call.id` 或 fallback id 找到 consumer。
2. 调用 `finish(call_id)`。
3. 将返回的 pending item 转成事件。
4. 移除该 consumer。
5. 无论 `finish()` 成功或失败，都继续进入 `dispatch_tool()`。

`finish()` 的错误只影响 preview。最终 `apply_patch` 执行阶段会再次解析完整 `patchText`，并返回权威错误。

## apply_patch 模块拆分

### 文件迁移

当前：

```text
crates/tools/src/builtin/fs/patch.rs
```

迁移为：

```text
crates/tools/src/builtin/fs/apply_patch/mod.rs
crates/tools/src/builtin/fs/apply_patch/stream_parser.rs
```

`crates/tools/src/builtin/fs/mod.rs` 中的模块声明同步调整。

### mod.rs 职责

`mod.rs` 保留：

1. `ApplyPatch` tool 定义。
2. `Tool` trait 实现。
3. 最终 `parse_patch()`。
4. `Hunk`、`UpdateChunk`、`PreparedHunk`。
5. prepare/write 和最终 `FileChange` 构造逻辑。
6. 现有最终执行测试。

为了让 `stream_parser.rs` 复用类型，需要把以下类型提升为 `pub(super)` 或 `pub(crate)`：

```rust
pub(super) struct UpdateChunk
pub(super) enum Hunk
```

如果直接复用现有类型导致构造器或字段可见性过宽，则可以在 `stream_parser.rs` 内定义 preview 专用 hunk 类型，再提供转换函数。第一版推荐复用现有 `Hunk` / `UpdateChunk`，避免 parser 类型分叉。

### stream_parser.rs 职责

`stream_parser.rs` 包含：

1. `ApplyPatchArgumentsConsumer`
2. `PatchTextDeltaExtractor`
3. `StreamingPatchParser`
4. preview hunk 到 `PatchPreviewChange` 的转换函数
5. streaming parser 与 extractor 的单元测试

`ApplyPatch` 的 `arguments_consumer()` 返回：

```rust
Some(Box::new(stream_parser::ApplyPatchArgumentsConsumer::new()))
```

## PatchTextDeltaExtractor 设计

### 输入输出

输入是 provider 直接给出的 JSON 参数片段，例如：

```text
{"patchText":"*** Begin Patch\n*** Add File: a.txt\n+hi
```

输出是新增的裸 patch 文本片段，例如：

```text
*** Begin Patch
*** Add File: a.txt
+hi
```

### 状态机

Extractor 不需要完整 JSON parser，但必须正确处理字符串扫描和 escape：

```text
SearchingKey
  -> ReadingKey
  -> WaitingColon
  -> WaitingValueString
  -> ReadingPatchText
  -> Done
```

额外维护：

1. `recent`：用于跨 chunk 搜索 `"patchText"`。
2. `escape`：当前是否处于 JSON string escape。
3. `unicode_escape`：跨 chunk 收集 4 个 hex 字符，并按 JSON 字符串语义解码为对应 Unicode scalar；如果 hex 非法或 surrogate pair 不完整，则进入 `failed`。
4. `failed`：一旦发现结构明显不匹配，后续返回空 delta，不影响最终执行。

### Escape 处理

必须支持：

| JSON escape | 输出 |
|---|---|
| `\n` | newline |
| `\r` | carriage return |
| `\t` | tab |
| `\"` | `"` |
| `\\` | `\` |
| `\uXXXX` | decoded Unicode scalar |

`\/` 可直接输出 `/`。

## StreamingPatchParser 设计

### 基本语义

Parser 参考 Codex 上游实现：

1. `push_delta(delta)` 按字符接收裸 patch 文本。
2. 遇到 `\n` 时处理完整行。
3. 未完成的当前行保存在 `line_buffer`，不提前解析。
4. 每次成功处理后返回当前已解析 hunks 的快照。
5. `finish()` 处理最后一个没有换行的 line，并要求最终状态为 `EndedPatch`。

### 状态

```rust
enum StreamingParserMode {
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile { hunk_line_number: usize },
    EndedPatch,
}
```

### 支持语法

第一版支持与现有 `parse_patch()` 一致的核心语法：

```text
*** Begin Patch
*** Add File: <path>
+line
*** Delete File: <path>
*** Update File: <path>
*** Move to: <path>
@@
@@ <context>
-old
+new
 context
*** End of File
*** End Patch
```

为了避免最终 parser 和 preview parser 行为明显分叉，streaming parser 也应支持：

1. CRLF：处理行尾 `\r\n` 时去掉单个 trailing `\r`。
2. update hunk 的 bare empty line：按空 context line 处理。
3. 首个 update chunk 缺少 `@@` 时允许直接出现 `+`、`-`、` ` 行，与现有最终 parser leniency 对齐。

### preview 转换

`Hunk::Add` 转为：

```rust
PatchPreviewChange::Add { path, content }
```

`Hunk::Delete` 转为：

```rust
PatchPreviewChange::Delete { path }
```

`Hunk::Update` 转为：

```rust
PatchPreviewChange::Update {
    path,
    move_path,
    old_text,
    new_text,
}
```

`old_text` / `new_text` 由 chunks 构造：context line 同时进入两边，delete line 只进入 `old_text`，insert line 只进入 `new_text`。这与 ACP `Diff` 的输入形状一致，和 `codex-acp` 对 apply_patch preview 的处理方式一致。

## ApplyPatchArgumentsConsumer 设计

结构：

```rust
pub struct ApplyPatchArgumentsConsumer {
    extractor: PatchTextDeltaExtractor,
    parser: StreamingPatchParser,
    last_sent_at: Option<Instant>,
    pending: Option<protocol::ToolArgumentsStreamItem>,
    disabled: bool,
}
```

行为：

1. `consume_delta()` 先用 extractor 提取裸 patch delta。
2. 如果 extractor 没有输出，返回空列表。
3. 将裸 patch delta 传给 parser。
4. parser 返回 hunks 后转成 preview changes。
5. 如果 changes 为空，返回空列表。
6. 如果距离上次发送超过 500ms，立即返回 `PatchPreview`。
7. 否则覆盖 `pending`，本次返回空列表。
8. 如果 parser 报错，设置 `disabled = true` 并返回空列表。
9. `finish()` 调用 parser.finish()，然后返回 pending item；如果 parser finish 报错，只返回空列表或错误，由 kernel 记录但不阻断执行。

第一版节流常量：

```rust
const APPLY_PATCH_ARGUMENTS_PREVIEW_INTERVAL: Duration = Duration::from_millis(500);
```

## ACP / TUI 影响

ACP `ToolCallContent::Diff` 的定义是最终文件状态：

```rust
pub struct Diff {
    pub path: PathBuf,
    pub old_text: Option<String>,
    pub new_text: String,
    pub meta: Option<Meta>,
}
```

现有转换链路已经把 `protocol::FileChange` 转成 ACP Diff：

```rust
ToolCallContent::Diff(schema::Diff::new(change.path, change.new_text).old_text(change.old_text))
```

TUI 当前也是基于 ACP Diff 的 `old_text/new_text` 生成 unified diff 样式展示。参数流 preview 虽然只有 patch hunk，不知道旧文件完整内容，也不能保证当前 partial patch 一定可应用，但 `codex-acp` 已经采用了把 preview hunk 转成 ACP Diff 的方案，用局部 `old_text/new_text` 获得结构化 diff 展示。

`codex-acp` 的 apply_patch 路径提供了一个可参考实现：它把 Codex 的 `PatchApplyBeginEvent` / `PatchApplyUpdatedEvent` / `PatchApplyEndEvent` 都映射为 ACP `ToolCall` 或 `ToolCallUpdate`，并将 changes 转成 `ToolCallContent::Diff`。对于 `FileChange::Update { unified_diff }`，它用 `diffy::Patch::from_str()` 解析 unified diff，再为每个 hunk 构造局部 `old_text` / `new_text`；解析失败时才降级为 `ToolCallContent::Content(Text)`。`codex-acp` 没有用 `_meta` 标记 apply_patch preview，它的 `_meta` 主要用于 terminal 扩展，例如 `terminal_info` / `terminal_output`。

第一版采用与 `codex-acp` 一致的 ACP/TUI 策略：

1. `Event::PatchApplyUpdated` 在 ACP bridge 中映射为 `SessionUpdate::ToolCallUpdate`。
2. 该 update 的 `status` 设置为 `InProgress`，`kind` 设置为 `ToolKind::Edit`，`title` 设置为 `Apply patch`。
3. 该 update 的 `content` 优先使用 `ToolCallContent::Diff`，让 TUI 走现有结构化 diff 渲染路径。
4. preview Diff 不额外设置 `_meta`，保持和 `codex-acp` 一致。
5. 如果某个 preview change 无法安全转换成局部 `old_text` / `new_text`，则该 change 降级为 `ToolCallContent::Content(Text)`。
6. 最终 diff 展示仍依赖现有 `ItemCompleted(FileChangeItem)` 到 ACP `ToolCallContent::Diff` 的转换。

这样 TUI 仍然只需要根据 ACP 协议事件显示：参数流阶段显示 preview Diff；工具完成阶段显示最终 Diff。TUI 需要额外遵守 ACP `ToolCallUpdateFields.content` 的 replace 语义：收到带 content 的 apply_patch preview update 时，应替换该 tool call 的现有 diffs，而不是不断 `push_diff` 累加。最终 completed Diff 到达时，也应替换 preview diff。

如果当前前端事件处理对未知 kernel 事件不兼容，则需要同步增加最小分支，避免反序列化或 match 穷尽失败。

## 错误处理

1. `PatchTextDeltaExtractor` 失败：禁用本次 preview，最终工具执行不受影响。
2. `StreamingPatchParser::push_delta()` 失败：禁用本次 preview，最终工具执行不受影响。
3. `StreamingPatchParser::finish()` 失败：kernel 可记录 debug 日志，但继续执行完整 tool call。
4. 完整 `apply_patch` 最终解析失败：保持现有行为，工具执行返回失败给模型。
5. call id 找不到 consumer：忽略 delta，只保留现有 `ToolCallDelta` 转发。

这个策略保证 preview 永远不是权威路径。权威路径只有最终 `ApplyPatch::do_apply()`。

## 测试计划

### stream_parser.rs 单元测试

1. 字符级 split 下，`Add File` 能逐步产出内容。
2. 字符级 split 下，`Update File` 能产出 chunks。
3. 支持 `Delete File`。
4. 支持 `*** Move to`。
5. 支持 `@@ context` 和空 `@@`。
6. 支持 `*** End of File`。
7. 支持 CRLF。
8. 支持 update hunk 中 bare empty line。
9. 缺少 `*** End Patch` 时 `finish()` 返回错误。
10. update hunk 为空时返回错误。

### PatchTextDeltaExtractor 单元测试

1. `patchText` key 跨 chunk。
2. value 起始引号跨 chunk。
3. `\n` escape 转成 newline。
4. `\"` escape 转成 quote。
5. `\\` escape 转成 backslash。
6. `\uXXXX` escape 能跨 chunk 解码。
7. JSON 中存在其他字段时只提取 `patchText`。
8. 完整 value 结束后忽略后续 JSON 内容。

### Tool/kernel 测试

1. `apply_patch` 的 `arguments_consumer()` 返回 `Some`。
2. 非 `apply_patch` 工具默认返回 `None`。
3. kernel 收到 `Name("apply_patch")` 后创建 consumer。
4. kernel 收到参数 delta 后发送 `Event::PatchApplyUpdated`。
5. kernel 在完整 `ToolCall` 执行前调用 `finish()` 并 flush pending preview。
6. preview parser 失败不阻断 `dispatch_tool()`。

### ACP/TUI 转换测试

1. `Event::PatchApplyUpdated` 转成 ACP `SessionUpdate::ToolCallUpdate`。
2. preview update 的 `content` 优先使用 `ToolCallContent::Diff`。
3. preview update 的状态为 `InProgress`，kind 为 `Edit`。
4. preview update 不设置 apply_patch preview `_meta`。
5. 无法转换为局部 old/new 的 preview change 降级为 `ToolCallContent::Content(Text)`。
6. 最终 `ItemCompleted(FileChangeItem)` 仍转成 ACP `ToolCallContent::Diff`。
7. TUI 收到多次 preview Diff update 时替换旧 diff，不重复累加。
8. TUI 收到最终 Diff 时替换 preview diff，并显示结构化最终 diff。

### 回归测试

1. 现有 `apply_patch` 最终执行测试全部保留。
2. `apply_patch_streaming_result_includes_file_changes` 继续验证最终 `FileChangeItem`。
3. provider 参数 delta 原有事件 `Event::ToolCallDelta` 继续发送。

## 实施步骤

1. 新增协议类型：`PatchPreviewChange`、`ToolArgumentsStreamItem`、`Event::PatchApplyUpdated`。
2. 在 `tools` crate 新增 `ToolArgumentsConsumer` trait 和 `Tool::arguments_consumer()` 默认方法。
3. 将 `crates/tools/src/builtin/fs/patch.rs` 迁移为 `apply_patch/mod.rs`，更新模块导出路径。
4. 新增 `apply_patch/stream_parser.rs`，实现 `PatchTextDeltaExtractor`、`StreamingPatchParser` 和 `ApplyPatchArgumentsConsumer`。
5. 让 `ApplyPatch` 实现 `arguments_consumer()`。
6. 在 `kernel::turn` 中维护 arguments consumer map，并处理 `Name`、`Delta`、完整 `ToolCall` 前的 `finish()`。
7. 在 ACP bridge 中把 `Event::PatchApplyUpdated` 映射为 `ToolCallUpdate` preview Diff。
8. 调整 TUI 对 apply_patch Diff update 的合并逻辑，按 content replacement 语义替换旧 diff。
9. 增加协议、parser、kernel、ACP/TUI 转换测试。
10. 运行相关测试和格式化检查。

## 设计取舍

### 为什么不复用 FileChangeItem

`FileChangeItem` 表示最终文件状态，包含 `old_text` 和 `new_text`。参数流阶段只有模型正在输出的 patch hunk，不能保证 patch 可应用，也不能可靠知道旧文件内容。复用 `FileChangeItem` 会让前端误判 preview 是最终 diff。

ACP `ToolCallContent::Diff` 可以用于参数流 preview，但它承载的是局部 old/new 片段，不是完整文件状态。这与 `codex-acp` 一致。TUI 必须按 update content replacement 语义替换旧 preview，并在最终 Diff 到达时替换掉 preview。

### 为什么不改成 freeform apply_patch

freeform 更贴近 Codex 上游，但会改变本项目当前工具 schema，影响 provider 解析、审批显示和测试。当前目标是引入通用 arguments 流式处理能力，不应同时改变工具协议。

因此本项目和 Codex 的差异是有意保留的：Codex 的 `StreamingPatchParser` 直接消费 raw patch delta；clawcode 的 `ApplyPatchArgumentsConsumer` 先用 `PatchTextDeltaExtractor` 从 JSON arguments delta 中提取 `patchText`，再把裸 patch delta 交给 `StreamingPatchParser`。这个额外层只属于 `apply_patch` 工具实现，不泄漏到通用 `ToolArgumentsConsumer` 接口。

### 为什么 preview 失败不阻断执行

preview 是体验增强，不是安全或正确性边界。最终执行仍会完整解析、校验、审批并落盘。如果 preview 错误阻断执行，会让 UI 功能影响核心工具可靠性。

### 为什么把 stream parser 放进 apply_patch 模块

`StreamingPatchParser` 是 `apply_patch` 格式专用 parser，不应放进通用 protocol 或 kernel。通用层只知道 `ToolArgumentsConsumer` 和 `ToolArgumentsStreamItem`，具体 patch 语法属于工具实现。

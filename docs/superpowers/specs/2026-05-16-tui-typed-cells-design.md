# TUI Typed Cells Design

**日期**: 2026-05-16  
**状态**: 待用户审核  
**范围**: `crates/tui/src/ui`

## 1. 背景

上一阶段已经将 TUI 中的大量 helper 从 `ui/render.rs` 拆到 `layout.rs`、`transcript.rs`、`tool_render.rs`、`tool_summary.rs`、`terminal_output.rs`、`status.rs` 等模块。这个拆分降低了文件体积和定位成本，但本质上仍是 procedural rendering：

```text
TranscriptCell enum
  -> transcript.rs match
      -> tool_render.rs helper
      -> tool_summary.rs helper
      -> terminal_output.rs helper
```

也就是说，UI 行为仍然集中在外部 helper 中；`TranscriptCell` 和 `ToolCallView` 主要是数据容器。随着后续增加 selection/focus、tool call 展开、raw/rich transcript、diff/markdown rich rendering，仅继续拆 helper 文件会遇到同样的问题：逻辑会按技术功能分散，而不是按 UI 单元聚合。

Codex TUI 的更深一层结构不是“没有 helper”，而是把 helper 收敛到具体 UI 单元内部。Codex 的 `history_cell` 模块中，`HistoryCell` 是 transcript 的基本显示单元；每种 cell 自己实现 `display_lines(width)`、`raw_lines()`、`desired_height(width)` 等行为。`ChatWidget` 再通过 `Renderable`/`HistoryCell` 组合显示，而不是由一个外部 transcript renderer match 所有类型。

本设计将 clawcode TUI 从“按功能拆 helper”推进到“按 UI cell 建模”。

## 2. 目标

1. 将 transcript 的显示行为下放到具体 cell 类型，而不是由 `transcript.rs` 对 `TranscriptCell` 做大 match。
2. 保持现有 UI 视觉行为不变。
3. 先采用 enum + per-variant typed struct 的轻量方案，不在第一阶段引入 `Box<dyn HistoryCell>`。
4. 将 tool call 的 summary、status、preview、terminal output 归一化收敛到 `ToolCallCell` 相关实现中。
5. 为后续 selection/focus、tool call 展开、raw/rich transcript、diff/markdown renderer 留出清晰接口。
6. 保持 TUI reducer 继续消费 ACP `SessionUpdate`，不改变 ACP/kernel 边界。

## 3. 非目标

1. 不在本阶段引入完整 Codex `HistoryCell` trait object 架构。
2. 不实现 selection/focus。
3. 不实现 tool call 展开/折叠。
4. 不引入 markdown/diff rich renderer。
5. 不改变 ACP schema。
6. 不改变 session persistence 格式。
7. 不重写 app loop、ACP client 或 ACP server。

## 4. 当前结构问题

### 4.1 `TranscriptCell` 是数据 enum

当前结构：

```rust
pub enum TranscriptCell {
    Assistant(String),
    Reasoning(String),
    User(String),
    System(String),
    ToolCall(ToolCallView),
}
```

它只表达“是什么”，不表达“如何显示”。显示逻辑在 `transcript.rs` 中：

```rust
match cell {
    TranscriptCell::Assistant(text) => ...
    TranscriptCell::Reasoning(text) => ...
    TranscriptCell::ToolCall(tool) => append_tool_call_lines(...)
}
```

这导致新增一个 cell 行为时，通常需要同时改 state、transcript renderer、tool renderer、tests。

### 4.2 `ToolCallView` 是数据容器

当前 `ToolCallView` 保存：

1. `call_id`
2. `name`
3. `arguments`
4. `output`
5. `status`

但 tool call 的所有显示行为在外部：

1. `tool_render.rs` 负责 header/status/output preview。
2. `tool_summary.rs` 负责参数摘要。
3. `terminal_output.rs` 负责 output 归一化。

这些 helper 都只服务 tool call cell，但它们没有被 ToolCall 类型拥有。

### 4.3 `transcript.rs` 仍然是中心分发器

`transcript.rs` 目前负责：

1. 遍历 `state.transcript()`。
2. 插入 cell 间空行。
3. match 每个 cell 类型。
4. 调用 tool helper。
5. 计算 scroll offset。

理想情况下，`transcript.rs` 应只负责 transcript 区域和滚动，不负责每种 cell 的内部显示细节。

## 5. Codex 参考结论

Codex 的关键结构：

1. `history_cell/mod.rs` 定义 `HistoryCell` trait。
2. 每种 history cell 自己实现 `display_lines(width)` 和 `raw_lines()`。
3. `HistoryCell` 默认实现 `desired_height(width)`，通过 `Paragraph::line_count` 测量 wrap 后高度。
4. `history_cell/base.rs` 提供可复用 building blocks，例如 `PlainHistoryCell`、`PrefixedWrappedHistoryCell`、`CompositeHistoryCell`。
5. `chatwidget/transcript.rs` 管理 active cell、revision、copy history，而不是管理每种 cell 的渲染细节。
6. `chatwidget/rendering.rs` 通过 `Renderable` 组合 active cell 和 bottom pane。

对 clawcode 的直接启发：

1. 不要继续创建更多泛化 helper 文件。
2. 应该让 UI 单元拥有自己的显示行为。
3. 在当前规模下，先保留 enum，避免过早引入 trait object。
4. 先建立 `display_lines(width)` / `raw_lines()` / `desired_height(width)` 这类行为接口，后续需要动态 cell 时再升级到 trait object。

## 6. 选定方案

采用“两阶段 typed cell”方案。

第一阶段：

```rust
pub enum TranscriptCell {
    Assistant(TextCell),
    Reasoning(TextCell),
    User(TextCell),
    System(TextCell),
    ToolCall(ToolCallCell),
}
```

每个 cell 通过 methods 表达行为：

```rust
impl TranscriptCell {
    pub(crate) fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    pub(crate) fn raw_lines(&self) -> Vec<Line<'static>>;
    pub(crate) fn desired_height(&self, width: u16) -> u16;
    pub(crate) fn is_stream_continuation(&self) -> bool;
}
```

第二阶段，如果 selection/focus 或 rich rendering 需要异构 cell 集合，再评估：

```rust
trait TranscriptCellView {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    fn raw_lines(&self) -> Vec<Line<'static>>;
    fn desired_height(&self, width: u16) -> u16;
}
```

并将 transcript 存储从 `Vec<TranscriptCell>` 升级为 `Vec<Box<dyn TranscriptCellView>>` 或其他更适合的结构。

## 7. 新模块结构

调整后的 `ui` 结构：

```text
crates/tui/src/ui/
├── cell/
│   ├── mod.rs              # TranscriptCell enum and shared cell behavior
│   ├── text.rs             # TextCell, TextRole, text display/raw logic
│   ├── tool.rs             # ToolCallCell and tool-call display behavior
│   └── terminal_output.rs  # tool output normalization used by ToolCallCell
├── approval.rs
├── composer.rs
├── layout.rs
├── mod.rs
├── render.rs
├── state.rs
├── status.rs
├── transcript.rs
└── view.rs
```

旧模块迁移：

1. `ui/tool_render.rs` 合并到 `ui/cell/tool.rs`。
2. `ui/tool_summary.rs` 合并到 `ui/cell/tool.rs` 或 `ui/cell/tool/summary.rs`。
3. `ui/terminal_output.rs` 移到 `ui/cell/terminal_output.rs`。
4. `ui/transcript.rs` 保留区域滚动和 frame rendering，但不再知道每种 cell 如何生成 lines。

如果 `tool.rs` 超过合理大小，可以使用：

```text
cell/tool/
├── mod.rs
├── render.rs
├── summary.rs
└── output.rs
```

但第一版优先少建层级，只有当文件明显过大时再拆。

## 8. Cell 类型设计

### 8.1 `TextCell`

```rust
pub(crate) enum TextRole {
    Assistant,
    Reasoning,
    User,
    System,
}

pub(crate) struct TextCell {
    role: TextRole,
    text: String,
}
```

职责：

1. 保存文本 role 和内容。
2. 根据 role 生成 styled lines。
3. 提供 append 行为，用于 streaming chunk 合并。
4. 提供 raw line。

显示规则保持现状：

1. Assistant：默认颜色。
2. Reasoning：dark gray + italic。
3. User：第一行 `> ` 前缀 + bold。
4. System：第一行 `system: ` 前缀 + dark gray。

### 8.2 `ToolCallCell`

```rust
pub(crate) struct ToolCallCell {
    call_id: String,
    name: String,
    arguments: String,
    output: String,
    status: ToolCallStatus,
}
```

职责：

1. 保存 ACP tool call display state。
2. 应用 snapshot/update。
3. 生成 header line。
4. 生成 output preview lines。
5. 生成 summary。
6. 生成 raw lines。

`ToolCallCell` 应取代 `ToolCallView`。如果实现阶段需要低风险迁移，可以先保留 `type ToolCallView = ToolCallCell` 不对外暴露，最终删除 `ToolCallView` 名称。

### 8.3 `TranscriptCell`

`TranscriptCell` 保持 enum，但变体持有 typed cell：

```rust
pub(crate) enum TranscriptCell {
    Text(TextCell),
    ToolCall(ToolCallCell),
}
```

或者保留显式 role 变体：

```rust
pub(crate) enum TranscriptCell {
    Assistant(TextCell),
    Reasoning(TextCell),
    User(TextCell),
    System(TextCell),
    ToolCall(ToolCallCell),
}
```

推荐第一版使用 `Text(TextCell)`。原因：

1. 文本 cell 的行为由 `TextRole` 决定，减少 enum 变体数量。
2. `append_to_last_or_push` 可以直接检查 `TextCell.role()`。
3. 后续新增 text role 不需要改 `TranscriptCell` enum。

## 9. Transcript Rendering

`ui/transcript.rs` 调整为：

```rust
for cell in state.transcript() {
    lines.extend(cell.display_lines(area.width));
    lines.push(Line::from(""));
}
```

它不再 match cell 类型，不再调用 `append_tool_call_lines`。

滚动逻辑暂时保持现状：

1. 仍按 logical line 数计算 scroll offset。
2. 不在本阶段引入 Codex 的 `desired_height` wrap 测量作为滚动依据。

原因：当前 TUI 已有可工作的滚动行为，本阶段重点是 ownership boundary。wrap-aware height 可以作为后续 resize/reflow 专项。

## 10. State Reducer 影响

`AppState` 仍负责 ACP update reducer，但构造 cell 的方式变化。

文本 chunk：

```rust
TranscriptCell::Text(TextCell::new(TextRole::Assistant, text))
```

tool call：

```rust
TranscriptCell::ToolCall(ToolCallCell::pending(call_id))
```

tool update：

```rust
cell.apply_update(update)
```

`tool_call_indices: HashMap<String, usize>` 保留。它仍是将 ACP `tool_call_id` 映射到 transcript cell index 的最低风险方案。

## 11. Testing Strategy

迁移测试归属：

1. Text styling tests 移到 `cell/text.rs`。
2. Tool summary tests 移到 `cell/tool.rs` 或 `cell/tool/summary.rs`。
3. Tool output preview tests 移到 `cell/tool.rs`。
4. Terminal output normalization tests 移到 `cell/terminal_output.rs`。
5. Transcript scroll tests 保留在 `transcript.rs`。
6. Full-frame render tests 保留在 `render.rs`。
7. AppState reducer tests 保留在 `state.rs`。

新增测试：

1. `text_cell_display_lines_match_existing_styles`
2. `tool_call_cell_display_lines_match_existing_preview`
3. `transcript_renderer_delegates_cell_display_lines`
4. `state_appends_streaming_text_cells_by_role`
5. `state_updates_tool_call_cell_in_place`

所有视觉行为必须与当前测试保持一致。

## 12. 迁移阶段

### Phase 1: Introduce Typed Cells

1. 新建 `ui/cell/mod.rs`、`text.rs`、`tool.rs`、`terminal_output.rs`。
2. 将 `ToolCallView` 移为 `ToolCallCell`。
3. 新建 `TextCell` / `TextRole`。
4. 让 `TranscriptCell` 持有 typed cells。
5. 将 line generation methods 放到 cell impl 中。
6. 保持 `AppState` public API 尽量不变。
7. 所有现有 tests 通过。

### Phase 2: Remove Procedural Helper Modules

1. 删除 `tool_render.rs`。
2. 删除 `tool_summary.rs`。
3. 删除顶层 `terminal_output.rs`。
4. 精简 `transcript.rs`，仅保留 frame rendering 和 scroll。
5. 更新 `ui/mod.rs`。

### Phase 3: Prepare for Rich Cells

只做接口准备，不实现功能：

1. 保留 `display_lines(width)`。
2. 增加 `raw_lines()`。
3. 增加 `desired_height(width)` 默认实现。
4. 不切换到 trait object。

### Phase 4: Future Dynamic Cells

当 selection/focus、tool expansion、diff/markdown rich rendering 进入实现阶段，再设计：

1. `TranscriptCellView` trait。
2. `Box<dyn TranscriptCellView>` storage。
3. active cell revision/cache。
4. raw/rich transcript mode。

## 13. Risks

### Risk 1: Refactor touches state and rendering at the same time

控制：

1. 先建立 typed cells。
2. 再迁移 rendering。
3. 最后删除旧 helper modules。

### Risk 2: Type churn affects many tests

控制：

1. 保留 `AppState::transcript()` 返回 slice。
2. 保留测试 fixture helper。
3. 迁移测试断言时优先断言 display lines，而不是内部字段。

### Risk 3: Over-engineering

控制：

1. 第一版不引入 trait object。
2. 不引入 generalized renderer trait。
3. 不引入 Codex full `Renderable` stack。
4. 只把已有行为移动到 cell methods。

### Risk 4: Comment and builder rules

控制：

1. 所有新增函数写英文函数级注释。
2. 所有非平凡逻辑写英文注释。
3. 字段数超过 3 的新 struct 使用 `typed-builder`。
4. `Option` builder 字段使用 `#[builder(default, setter(strip_option))]`。

## 14. Acceptance Criteria

1. `transcript.rs` 不再 match `TranscriptCell` 的具体变体来生成 lines。
2. `ToolCallCell` 自己提供 display/raw behavior。
3. `TextCell` 自己提供 role-based display behavior。
4. 旧的 `tool_render.rs`、`tool_summary.rs` 不再作为顶层 helper 模块存在。
5. 当前 TUI 视觉行为不变。
6. `cargo test -p tui` 通过。
7. `cargo clippy --workspace --all-targets -- -D warnings` 通过。
8. `pre-commit run --all-files` 通过。

## 15. Recommendation

推荐先执行 Phase 1 + Phase 2，停止在 enum + typed cell method 层级。这个阶段能解决“helper 只是换地方”的问题，同时避免直接引入 Codex 级别的 trait object 和 renderable stack。

等后续实现 selection/focus/tool expansion 时，再基于真实需求决定是否进入 Phase 4。

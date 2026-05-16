# TUI Code Organization Design

**日期**: 2026-05-16  
**状态**: 待用户审核  
**范围**: `crates/tui`

## 1. 背景

当前 TUI 已经完成 ACP-backed terminal UI、borderless 输入区、状态栏、thinking 样式、历史恢复、tool call 预览等能力。随着 UI 行为增加，`crates/tui/src/ui/render.rs` 和 `crates/tui/src/ui/state.rs` 开始聚集大量 helper 函数。

问题不在于 helper 函数本身，而是 helper 的职责边界不清晰：

1. `render.rs` 同时负责 frame 布局、transcript 渲染、composer 渲染、状态栏、tool call 展示、tool 参数摘要、terminal output 清洗、approval modal 和测试。
2. `state.rs` 同时负责 ACP update 到 UI state 的 reducer、transcript 合并、tool call 原地更新、usage、status line 文本、ACP content block 提取。
3. tool call、terminal output、transcript scroll 等行为都已经有独立复杂度，但仍作为同一个文件中的私有 helper 存在。
4. 后续如果继续增加 selection/focus、tool call 展开、diff 渲染、markdown 渲染，会让 `render.rs` 变成持续增长的中心文件。

Codex TUI 的实现可以作为参考。Codex 没有把所有 UI helper 放在一个文件里，而是按领域拆分，例如 `chatwidget`、`bottom_pane`、`transcript_reflow`、`markdown_render`、`status_indicator_widget`、`exec_command`、`diff_render`。本设计借鉴这种领域边界，但不完整移植 Codex 的复杂 history cell 体系。

## 2. 目标

1. 将 TUI 中的 helper 函数按职责拆到明确的 `ui` 子模块。
2. 保持现有 TUI 行为不变，第一阶段只做结构整理。
3. 降低 `render.rs` 和 `state.rs` 的职责密度，让它们成为 orchestration 层，而不是所有细节的集中点。
4. 让 tool call 显示、terminal output 归一化、transcript 渲染、状态栏格式化都有独立测试位置。
5. 保持当前 ACP-backed UI 边界：UI reducer 继续消费 ACP schema，不直接消费 kernel event。
6. 为后续 selection/focus、tool call 展开、diff/markdown rich rendering 留出清晰扩展点。

## 3. 非目标

1. 不在本阶段改变 TUI 的视觉表现。
2. 不在本阶段实现新的 selection/focus 或 tool call 展开功能。
3. 不在本阶段引入 Codex 的完整 `HistoryCell` trait/object 模型。
4. 不重写 ACP client/server 交互。
5. 不改变 session history store 或 ACP schema。
6. 不做跨 crate 的大规模重构。

## 4. 当前问题分解

### 4.1 `ui/render.rs`

当前 `render.rs` 包含以下职责：

1. 顶层 `render` frame composition。
2. composer 高度计算。
3. transcript 区域渲染和 scroll offset 计算。
4. transcript cell 到 `Line` 的转换。
5. composer/help/status 渲染。
6. tool call 标题、状态、output preview 渲染。
7. shell/read/edit/agent/mcp 等工具参数摘要。
8. terminal output 行归一化和 ANSI control sequence 过滤。
9. approval modal 文本渲染。
10. 以上多个领域的单元测试。

这些职责的变化频率不同。把它们放在同一个文件里，会让小改动需要在大文件中定位上下文，也会让测试难以归属。

### 4.2 `ui/state.rs`

当前 `state.rs` 包含以下职责：

1. `AppState`、`TranscriptCell`、`ToolCallView`、`UsageView` 等 UI model。
2. ACP `SessionUpdate` 到 UI state 的 reducer。
3. tool call index map 和 transcript cell 的同步更新。
4. assistant/thinking/user/system 文本合并。
5. usage update。
6. top/bottom status line 字符串构造。
7. ACP content block / tool content 到 display string 的转换。

`state.rs` 是 UI 状态层，可以保留 model 和 reducer，但不应该继续承载 status line 展示文本和 ACP content extraction 的所有 helper。

## 5. 设计原则

1. 按领域拆分，不创建泛化的 `helpers.rs`。
2. 第一阶段只移动代码和测试，不改变行为。
3. `render.rs` 只做顶层布局和子模块调度。
4. `state.rs` 优先保留数据结构和 reducer；纯格式化逻辑逐步移出。
5. 每个模块都应该回答三个问题：
   - 它负责什么？
   - 它依赖什么输入？
   - 它的输出被谁消费？
6. 优先保持 enum-based `TranscriptCell`，避免过早引入 trait object。
7. 新增或修改的 Rust 函数必须有英文函数级注释；非平凡逻辑必须有英文注释。

## 6. 推荐模块结构

调整后的 `crates/tui/src/ui`：

```text
crates/tui/src/ui/
├── approval.rs         # approval state, key mapping, modal line rendering
├── composer.rs         # composer state and text editing
├── layout.rs           # frame layout, composer height, centered rect
├── mod.rs
├── render.rs           # top-level frame composition only
├── state.rs            # AppState, TranscriptCell, reducer entry points
├── status.rs           # top/bottom status line formatting
├── terminal_output.rs  # ANSI stripping, carriage return handling, preview lines
├── tool_render.rs      # ToolCallView -> ratatui lines
├── tool_summary.rs     # tool name/args -> compact display summary
├── transcript.rs       # transcript rendering and scroll calculations
└── view.rs             # scroll/follow-tail/UI view state
```

## 7. 模块职责

### 7.1 `ui/render.rs`

保留职责：

1. 接收 `Frame`、`AppState`、`ViewState`、composer text。
2. 调用 `layout` 计算区域。
3. 调用 `transcript`、`composer`、`status`、`approval` 渲染对应区域。
4. 保持最小化 orchestration，不直接解析 tool args，不直接处理 ANSI，不直接拼接 tool output preview。

目标规模：约 150 到 250 行。

### 7.2 `ui/layout.rs`

负责所有布局计算：

1. composer 高度。
2. top status、transcript、composer、bottom status、help bar 区域切分。
3. approval modal 的 centered rect。

布局规则集中后，后续修复缩放、状态栏重叠、输入区高度问题时，不需要进入 transcript/tool call 逻辑。

### 7.3 `ui/transcript.rs`

负责 transcript 区域：

1. `render_transcript`。
2. effective scroll 计算。
3. scroll offset 计算。
4. `TranscriptCell` 到 `Vec<Line<'static>>` 的转换入口。
5. 普通 user/assistant/thinking/system 文本行样式。

`TranscriptCell::ToolCall` 不在这里展开具体渲染细节，而是委托给 `tool_render`。

### 7.4 `ui/tool_render.rs`

负责 tool call 的 UI 表现：

1. tool call 状态 bullet。
2. `Running` / `Ran` / `Failed` 等状态动词。
3. 标题行。
4. output preview 行。
5. 无 output、超过 5 行、失败输出等情况。

它依赖：

1. `ToolCallView`
2. `tool_summary`
3. `terminal_output`

它不直接解析 ACP event，也不修改 `AppState`。

### 7.5 `ui/tool_summary.rs`

负责工具参数摘要：

1. `shell` command 摘要。
2. `read_file` path/range 摘要。
3. `edit`/`write_file` path 摘要。
4. `spawn_agent` / `send_input` / MCP tool 摘要。
5. unknown tool fallback。
6. JSON 参数解析和 compact inline。

这个模块只处理 “tool name + arguments string -> summary string”，不负责样式和输出预览。

### 7.6 `ui/terminal_output.rs`

负责 terminal/tool output 的文本归一化：

1. ANSI control sequence 剥离。
2. carriage return 覆盖处理。
3. display lines 切分。
4. preview line 计数辅助。

这个模块的测试应覆盖 shell 进度条、`\r` 覆盖、多行 stderr/stdout、ANSI 彩色输出等情况。

### 7.7 `ui/status.rs`

负责 top/bottom status line：

1. model label。
2. cwd。
3. session mode。
4. token usage。
5. prompt running/idle 状态。
6. display width 截断。

第一阶段可以先只移动 status line 渲染/格式化 helper；如果迁移风险较高，`AppState::top_status_line` 和 `bottom_status_line` 可以暂时保留，第二阶段再迁移。

### 7.8 `ui/approval.rs`

继续负责 approval：

1. pending approval view model。
2. approval key mapping。
3. modal line rendering。

当前 `approval_lines` 应移动到这里，让 approval 的数据和展示靠近。

### 7.9 `ui/state.rs`

第一阶段保留：

1. `AppState`
2. `TranscriptCell`
3. `ToolCallView`
4. `UsageView`
5. `apply_session_update`
6. tool call index update
7. transcript append/coalesce

第二阶段再考虑拆出：

1. `ui/reducer.rs`：ACP `SessionUpdate` reducer。
2. `ui/acp_convert.rs`：ACP content block / tool content extraction。
3. `ui/tool_state.rs`：tool call index/store 更新。

## 8. Codex 借鉴点

可以借鉴：

1. Codex 按 UI 领域拆模块，而不是把所有 rendering helper 放在一个文件。
2. Codex 对 command execution、diff、markdown、status、bottom pane 分别建模，便于独立测试和演进。
3. Codex 的 transcript/reflow 是单独关注点；clawcode 后续如果增强滚动和 resize 行为，也应该让 transcript reflow 独立。
4. Codex 的 bottom pane 是一个独立领域；clawcode 当前 composer/status/help bar 也应该保持边界清晰。

暂时不借鉴：

1. 不引入 Codex 的完整 history cell trait/object 结构。
2. 不引入 Codex 的大型 markdown/diff renderer。
3. 不引入 Codex 的复杂 command lifecycle/state machine。
4. 不把简单 enum 状态过早拆成多层动态分发。

原因：clawcode TUI 还处在轻量 ACP client 阶段，最重要的是清晰边界和低风险迁移，而不是一次性复制 Codex 的成熟复杂结构。

## 9. 迁移阶段

### Phase 1: 纯模块拆分

目标：不改变行为，只移动代码和测试。

步骤：

1. 创建 `layout.rs`，迁移 layout helper。
2. 创建 `terminal_output.rs`，迁移 terminal output normalize helper 和测试。
3. 创建 `tool_summary.rs`，迁移 tool args summary helper 和测试。
4. 创建 `tool_render.rs`，迁移 tool call line rendering helper 和测试。
5. 创建 `transcript.rs`，迁移 transcript rendering/scroll helper 和测试。
6. 创建 `status.rs`，迁移 status line formatter 或 status render helper。
7. 将 `approval_lines` 移到 `approval.rs`。
8. 精简 `render.rs`，只保留 frame composition。

验收标准：

1. TUI 行为不变。
2. `render.rs` 显著缩小。
3. 原有 render/helper 测试全部迁移并通过。
4. `cargo fmt` 通过。
5. `cargo clippy --workspace --all-targets -- -D warnings` 通过，或使用仓库既有 pre-commit 命令验证。

### Phase 2: State/reducer 边界整理

目标：降低 `state.rs` 的非状态职责。

步骤：

1. 拆出 ACP content extraction helper。
2. 拆出 reducer 私有模块，保留 `AppState::apply_session_update` 作为公共入口。
3. 如果 tool call 更新逻辑继续增长，提取 `ToolCallStore` 或 `tool_state.rs`。
4. 将 status line 文本构造完全移到 `status.rs`。

验收标准：

1. `state.rs` 主要表达 model 和 reducer entry point。
2. ACP schema 到 UI text 的转换有独立测试。
3. tool call update 的顺序、resume preview、thinking ordering 行为不回退。

### Phase 3: 后续能力扩展

只有当 selection/focus、tool call 展开、diff/markdown rich rendering 进入实现阶段时，再评估：

1. `TranscriptCell` 是否需要 per-variant renderer。
2. 是否需要 Codex-like `HistoryCell` trait。
3. 是否需要 transcript reflow cache。
4. 是否需要 tool call output pager。

## 10. 测试策略

### 10.1 单元测试迁移

现有 `render.rs` 中的测试应跟随被测逻辑移动：

1. terminal output 归一化测试移动到 `terminal_output.rs`。
2. tool summary 测试移动到 `tool_summary.rs`。
3. tool call preview 测试移动到 `tool_render.rs`。
4. scroll offset / transcript line 测试移动到 `transcript.rs`。
5. layout 高度测试移动到 `layout.rs`。

### 10.2 行为回归测试

需要覆盖：

1. resume 历史中的 tool call 默认仍为 preview。
2. thinking 在历史恢复中仍保持样式。
3. thinking 与 assistant answer 的顺序不变。
4. shell output 仍只预览前 5 行。
5. ANSI 和 carriage return 输出仍正确归一化。
6. terminal resize 后 transcript 区域不覆盖 composer/status。

### 10.3 验证命令

建议执行：

```bash
cargo fmt --all
cargo test -p tui
cargo clippy --workspace --all-targets -- -D warnings
pre-commit run --all-files
```

如果仓库 pre-commit 已经覆盖 fmt/clippy，可以用 pre-commit 作为最终验证，但实现过程中仍建议按模块先跑 `cargo test -p tui`。

## 11. 风险与控制

### 风险 1: 拆分过程中引入行为变化

控制方式：

1. Phase 1 只移动代码，不改逻辑。
2. 每移动一个模块就运行对应测试。
3. 不在同一个 patch 中同时做代码移动和新功能。

### 风险 2: 模块可见性被放大

控制方式：

1. 默认使用 `pub(super)` 或 `pub(crate)`。
2. 只有 `render.rs` 或跨模块必须调用的 API 才公开。
3. 不为了测试把内部实现公开到 crate 外。

### 风险 3: 过早抽象

控制方式：

1. 保留 `TranscriptCell` enum。
2. 不引入 trait object。
3. 不引入通用 `Renderer` trait。
4. 等 selection/focus 或 rich rendering 真正需要时再扩展。

### 风险 4: 注释规则遗漏

控制方式：

1. 所有新增或修改的 Rust 函数写英文函数级注释。
2. 对 ANSI parsing、carriage return 处理、scroll offset 等非平凡逻辑补英文注释。
3. 移动已有函数时同步检查注释是否满足当前项目规则。

## 12. 推荐执行方案

推荐先做 Phase 1。原因：

1. 它收益明确，可以马上降低 `render.rs` 的复杂度。
2. 行为风险最低，因为不改 UI 语义。
3. 它会为 Phase 2 提供更清楚的调用边界。
4. 它不阻塞后续 selection/focus/tool expand 的功能设计。

Phase 1 完成并通过 review 后，再决定是否继续 Phase 2。不要把 Phase 1 和 Phase 2 混在同一个实现批次里。

## 13. Review Checklist

用户 review 时重点确认：

1. 模块拆分边界是否符合预期。
2. 是否接受第一阶段只做纯重组、不改行为。
3. 是否接受暂不引入 Codex-style `HistoryCell`。
4. 是否需要把 `status.rs` 放在 Phase 1，还是延后到 Phase 2。
5. 是否需要将 `terminal_output.rs` 命名为 `output.rs` 或 `ansi.rs`。

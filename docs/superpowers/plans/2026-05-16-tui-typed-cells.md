# TUI Typed Cells Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 TUI transcript 从 procedural helper 渲染升级为 typed cell 渲染，同时保持现有 UI 行为不变。

**Architecture:** 新增 `ui/cell` 子模块，使用 `TextCell`、`ToolCallCell` 和 `TranscriptCell` methods 承载显示行为。`transcript.rs` 只负责区域渲染和滚动，不再 match 每种 cell 的内部显示细节；第一阶段保持 enum 存储，不引入 `Box<dyn HistoryCell>`。

**Tech Stack:** Rust, ratatui, agent-client-protocol, serde_json, unicode-width, typed-builder, existing `tui` crate tests.

---

## 执行约束

1. 不改变现有 TUI 视觉行为。
2. 不引入 trait object 或 Codex full `HistoryCell` 架构。
3. 不实现 selection/focus、tool call 展开、markdown/diff rich rendering。
4. 不改变 ACP schema、ACP client/server、app loop。
5. 所有新增或修改的 Rust 函数必须有英文函数级注释。
6. 非平凡逻辑必须写英文注释。
7. 字段超过 3 个的新 struct 必须使用 `typed-builder`。
8. 不创建 commit，除非用户在 review 后明确要求。

## 文件结构

Create:

- `crates/tui/src/ui/cell/mod.rs`  
  定义 `TranscriptCell` enum，提供 `display_lines`、`raw_lines`、`desired_height`、`text` 等统一行为入口。
- `crates/tui/src/ui/cell/text.rs`  
  定义 `TextCell`、`TextRole`，承载 assistant/reasoning/user/system 文本显示逻辑。
- `crates/tui/src/ui/cell/tool.rs`  
  定义 `ToolCallCell`，承载 tool call 状态、summary、preview、raw lines。
- `crates/tui/src/ui/cell/terminal_output.rs`  
  承载 tool output 的 ANSI 清洗、carriage return 处理和 display lines。

Modify:

- `crates/tui/src/ui/mod.rs`  
  导出 `cell`，删除旧 `tool_render`、`tool_summary`、`terminal_output` 顶层模块。
- `crates/tui/src/ui/state.rs`  
  从 `ui::cell` 使用 `TranscriptCell`、`TextCell`、`TextRole`、`ToolCallCell`，更新 reducer 构造和更新逻辑。
- `crates/tui/src/ui/transcript.rs`  
  删除 cell match，只调用 `cell.display_lines(area.width)`。
- `crates/tui/src/ui/render.rs`  
  保留 full-frame tests，必要时更新测试名和 import。

Delete after migration:

- `crates/tui/src/ui/tool_render.rs`
- `crates/tui/src/ui/tool_summary.rs`
- `crates/tui/src/ui/terminal_output.rs`

Test:

- `cargo test -p tui`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `pre-commit run --all-files`

---

### Task 1: 建立 `ui/cell` 模块骨架

**Files:**
- Create: `crates/tui/src/ui/cell/mod.rs`
- Create: `crates/tui/src/ui/cell/text.rs`
- Create: `crates/tui/src/ui/cell/tool.rs`
- Create: `crates/tui/src/ui/cell/terminal_output.rs`
- Modify: `crates/tui/src/ui/mod.rs`

- [ ] **Step 1: 创建模块文件**

`crates/tui/src/ui/cell/mod.rs`：

```rust
//! Typed transcript cells for the local TUI.

mod terminal_output;
mod text;
mod tool;

pub(crate) use text::{TextCell, TextRole};
pub(crate) use tool::ToolCallCell;
```

`crates/tui/src/ui/cell/text.rs`：

```rust
//! Text transcript cells for the local TUI.
```

`crates/tui/src/ui/cell/tool.rs`：

```rust
//! Tool-call transcript cells for the local TUI.
```

`crates/tui/src/ui/cell/terminal_output.rs`：

```rust
//! Terminal output normalization for tool-call cells.
```

- [ ] **Step 2: 注册 cell 模块**

更新 `crates/tui/src/ui/mod.rs`：

```rust
//! Local UI state, input, approval, and ratatui rendering.

pub mod approval;
pub(crate) mod cell;
pub mod composer;
pub mod layout;
pub mod render;
pub mod state;
pub mod status;
pub mod terminal_output;
pub mod tool_render;
pub mod tool_summary;
pub mod transcript;
pub mod view;
```

注意：本任务先保留旧模块，后续任务迁移完成后再删除。

- [ ] **Step 3: 验证空模块编译**

Run:

```bash
rtk cargo test -p tui
```

Expected: 当前所有 TUI 测试通过。

---

### Task 2: 实现 `TextCell` 和文本显示行为

**Files:**
- Modify: `crates/tui/src/ui/cell/text.rs`

- [ ] **Step 1: 写 `TextRole` 和 `TextCell` 类型**

在 `text.rs` 中添加：

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Role-specific styling for text transcript cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextRole {
    /// Assistant answer text.
    Assistant,
    /// Assistant reasoning text.
    Reasoning,
    /// User prompt text.
    User,
    /// System or runtime text.
    System,
}

/// Renderable text transcript cell.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(crate) struct TextCell {
    /// Role that controls display prefix and style.
    role: TextRole,
    /// Text accumulated for this transcript cell.
    text: String,
}
```

- [ ] **Step 2: 实现构造、访问和 append**

继续添加：

```rust
impl TextCell {
    /// Creates a text cell for one transcript role.
    pub(crate) fn new(role: TextRole, text: impl Into<String>) -> Self {
        Self::builder().role(role).text(text.into()).build()
    }

    /// Returns the role that controls display behavior.
    pub(crate) fn role(&self) -> TextRole {
        self.role
    }

    /// Returns the stored text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Appends streaming text to this cell.
    pub(crate) fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }
}
```

- [ ] **Step 3: 实现 display/raw lines**

继续添加：

```rust
impl TextCell {
    /// Returns styled logical lines for this text cell.
    pub(crate) fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let (first_prefix, style) = match self.role {
            TextRole::Assistant => ("", Style::default().fg(Color::Reset)),
            TextRole::Reasoning => (
                "",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            TextRole::User => ("> ", Style::default().add_modifier(Modifier::BOLD)),
            TextRole::System => ("system: ", Style::default().fg(Color::DarkGray)),
        };
        styled_text_lines(&self.text, first_prefix, style)
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    pub(crate) fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect()
    }
}

/// Builds one styled line per newline-delimited segment.
fn styled_text_lines(text: &str, first_prefix: &str, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut added = false;
    for (index, segment) in text.split('\n').enumerate() {
        let prefix = if index == 0 { first_prefix } else { "" };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{segment}"),
            style,
        )));
        added = true;
    }

    if !added {
        lines.push(Line::from(Span::styled(first_prefix.to_string(), style)));
    }

    lines
}
```

- [ ] **Step 4: 添加 text cell 单元测试**

在 `text.rs` 末尾添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies reasoning text keeps the existing low-emphasis style.
    #[test]
    fn text_cell_reasoning_uses_distinct_style() {
        let cell = TextCell::new(TextRole::Reasoning, "thinking");

        let lines = cell.display_lines(80);
        let span = lines[0].spans.first().expect("reasoning span");

        assert_eq!(span.content, "thinking");
        assert_eq!(span.style.fg, Some(Color::DarkGray));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    /// Verifies user text keeps the prompt prefix on only the first line.
    #[test]
    fn text_cell_user_prefixes_first_line_only() {
        let cell = TextCell::new(TextRole::User, "hello\nworld");

        let rendered = cell
            .display_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["> hello".to_string(), "world".to_string()]);
    }
}
```

- [ ] **Step 5: 验证 TextCell 测试**

Run:

```bash
rtk cargo test -p tui text_cell_reasoning_uses_distinct_style
rtk cargo test -p tui text_cell_user_prefixes_first_line_only
```

Expected: 两个测试通过。

---

### Task 3: 迁移 terminal output 到 cell 模块

**Files:**
- Modify: `crates/tui/src/ui/cell/terminal_output.rs`
- Modify: `crates/tui/src/ui/terminal_output.rs`

- [ ] **Step 1: 复制 terminal output 实现到 cell 模块**

将 `crates/tui/src/ui/terminal_output.rs` 的当前内容移动到 `crates/tui/src/ui/cell/terminal_output.rs`，并将入口函数可见性设为：

```rust
pub(super) fn terminal_display_lines(text: &str) -> Vec<String>
```

保留现有函数级英文注释和 carriage-return 注释。

- [ ] **Step 2: 暂时保留旧模块转发**

把旧 `crates/tui/src/ui/terminal_output.rs` 改成：

```rust
//! Compatibility re-export for terminal output normalization during cell migration.

pub(super) use crate::ui::cell::terminal_output::terminal_display_lines;
```

如果可见性不允许 re-export，则本步骤改为保留旧文件原样，直到 Task 7 删除旧模块。

- [ ] **Step 3: 验证 terminal output 测试**

Run:

```bash
rtk cargo test -p tui terminal_display_lines_handles_carriage_return_updates
```

Expected: 测试通过。

---

### Task 4: 实现 `ToolCallCell`

**Files:**
- Modify: `crates/tui/src/ui/cell/tool.rs`

- [ ] **Step 1: 定义 ToolCallCell**

在 `tool.rs` 中添加：

```rust
use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::cell::terminal_output::terminal_display_lines;

const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;

/// Renderable ACP tool-call transcript cell.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(crate) struct ToolCallCell {
    /// Unique ACP call id for the tool invocation.
    call_id: String,
    /// Tool title shown to the user.
    name: String,
    /// JSON argument text accumulated from ACP raw input.
    arguments: String,
    /// Tool output text accumulated from ACP update content.
    output: String,
    /// Latest ACP execution status for the tool.
    status: ToolCallStatus,
}
```

- [ ] **Step 2: 实现构造、访问和 mutation**

继续添加：

```rust
impl ToolCallCell {
    /// Creates a pending placeholder tool-call cell.
    pub(crate) fn pending(call_id: String) -> Self {
        Self::builder()
            .call_id(call_id)
            .name("tool".to_string())
            .arguments(String::new())
            .output(String::new())
            .status(ToolCallStatus::Pending)
            .build()
    }

    /// Returns the ACP call id.
    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the display tool name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns accumulated argument text.
    pub(crate) fn arguments(&self) -> &str {
        &self.arguments
    }

    /// Returns accumulated output text.
    pub(crate) fn output(&self) -> &str {
        &self.output
    }

    /// Returns the latest tool status.
    pub(crate) fn status(&self) -> ToolCallStatus {
        self.status
    }

    /// Replaces the display tool name.
    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// Replaces stored raw JSON argument text.
    pub(crate) fn set_arguments(&mut self, arguments: String) {
        self.arguments = arguments;
    }

    /// Appends output text in ACP arrival order.
    pub(crate) fn push_output(&mut self, output: &str) {
        self.output.push_str(output);
    }

    /// Replaces the latest ACP tool status.
    pub(crate) fn set_status(&mut self, status: ToolCallStatus) {
        self.status = status;
    }
}
```

- [ ] **Step 3: 迁移 tool render 和 summary 函数**

将 `tool_render.rs` 和 `tool_summary.rs` 中的函数迁移到 `tool.rs`，并改成围绕 `ToolCallCell`：

```rust
impl ToolCallCell {
    /// Returns styled logical lines for this tool-call cell.
    pub(crate) fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            status_bullet(self.status()),
            " ".into(),
            Span::styled(
                format!("{} {}", status_verb(self.status()), self.summary()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        append_tool_output_preview_lines(&mut lines, self.status(), self.output());
        lines
    }

    /// Returns copy-friendly raw lines for this tool-call cell.
    pub(crate) fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(format!(
            "{} {}",
            status_verb(self.status()),
            self.summary()
        ))];
        lines.extend(
            terminal_display_lines(self.output())
                .into_iter()
                .map(Line::from),
        );
        lines
    }

    /// Builds a concise category-specific title for this tool call.
    fn summary(&self) -> String {
        let args = tool_arguments(self.arguments());
        match self.name() {
            "shell" => shell_summary(&args),
            "read_file" => read_file_summary(&args),
            "write_file" => path_summary("Write", &args, "path"),
            "edit" => edit_summary(&args),
            "apply_patch" => "Apply patch".to_string(),
            "skill" => path_summary("Load skill", &args, "name"),
            "spawn_agent" => spawn_agent_summary(&args),
            "send_message" => message_tool_summary("Send message to", &args),
            "followup_task" => message_tool_summary("Follow up", &args),
            "wait_agent" => path_summary("Wait agent", &args, "agent_path"),
            "list_agents" => "List agents".to_string(),
            "close_agent" => path_summary("Close agent", &args, "agent_path"),
            name if name.starts_with("mcp__") => mcp_summary(name, &args),
            name => unknown_tool_summary(name, self.arguments()),
        }
    }
}
```

同时迁移这些 helper 到 `tool.rs` 私有函数：

```rust
append_tool_output_preview_lines
dim_line
status_verb
status_bullet
tool_arguments
shell_summary
read_file_summary
path_summary
edit_summary
spawn_agent_summary
message_tool_summary
mcp_summary
unknown_tool_summary
string_field
truncate_chars
compact_inline
```

- [ ] **Step 4: 添加 ToolCallCell 单元测试**

在 `tool.rs` 添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies shell tool calls keep the existing preview format and line cap.
    #[test]
    fn tool_call_cell_shell_preview_uses_existing_format() {
        let mut cell = ToolCallCell::builder()
            .call_id("call-1".to_string())
            .name("shell".to_string())
            .arguments(serde_json::json!({"command": "cargo test -p tui"}).to_string())
            .output("line1\nline2\nline3\nline4\nline5\nline6".to_string())
            .status(ToolCallStatus::Completed)
            .build();

        cell.set_status(ToolCallStatus::Completed);
        let rendered = cell
            .display_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("• Ran cargo test -p tui"));
        assert!(rendered.contains("  └ line1"));
        assert!(rendered.contains("    line5"));
        assert!(rendered.contains("... +1 lines"));
        assert!(!rendered.contains("line6"));
    }
}
```

- [ ] **Step 5: 验证 ToolCallCell 测试**

Run:

```bash
rtk cargo test -p tui tool_call_cell_shell_preview_uses_existing_format
```

Expected: 测试通过。

---

### Task 5: 将 `TranscriptCell` 迁入 `ui/cell`

**Files:**
- Modify: `crates/tui/src/ui/cell/mod.rs`
- Modify: `crates/tui/src/ui/state.rs`

- [ ] **Step 1: 在 cell/mod.rs 定义 TranscriptCell**

在 `cell/mod.rs` 中添加：

```rust
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

pub(crate) use text::{TextCell, TextRole};
pub(crate) use tool::ToolCallCell;

/// Renderable transcript cell stored in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptCell {
    /// Text content with role-specific display behavior.
    Text(TextCell),
    /// ACP tool invocation and its live output state.
    ToolCall(ToolCallCell),
}

impl TranscriptCell {
    /// Returns styled logical lines for the main transcript viewport.
    pub(crate) fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self {
            Self::Text(cell) => cell.display_lines(width),
            Self::ToolCall(cell) => cell.display_lines(width),
        }
    }

    /// Returns copy-friendly plain logical lines.
    pub(crate) fn raw_lines(&self) -> Vec<Line<'static>> {
        match self {
            Self::Text(cell) => cell.raw_lines(),
            Self::ToolCall(cell) => cell.raw_lines(),
        }
    }

    /// Returns the number of wrapped viewport rows needed for this cell.
    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(self.display_lines(width))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    /// Returns the text payload used by existing tests and simple inspections.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Text(cell) => cell.text(),
            Self::ToolCall(cell) => cell.output(),
        }
    }
}
```

- [ ] **Step 2: 从 state.rs 删除旧 TranscriptCell 和 ToolCallView 定义**

删除 `state.rs` 中旧的：

```rust
pub enum TranscriptCell { ... }
pub struct ToolCallView { ... }
impl ToolCallView { ... }
```

添加 import：

```rust
use crate::ui::cell::{TextCell, TextRole, ToolCallCell, TranscriptCell};
```

- [ ] **Step 3: 先保留 ToolCallView 名称兼容测试**

如果 `state.rs` 或 tests 中仍有大量 `ToolCallView` 引用，先在 `state.rs` 添加临时别名：

```rust
type ToolCallView = ToolCallCell;
```

实现结束时如果引用已清空，再删除别名。

- [ ] **Step 4: 运行编译检查观察迁移错误**

Run:

```bash
rtk cargo test -p tui
```

Expected: 这一步可能失败，错误应集中在旧 enum 变体和 `ToolCallView` 路径引用。继续 Task 6 修复。

---

### Task 6: 更新 AppState reducer 使用 typed cells

**Files:**
- Modify: `crates/tui/src/ui/state.rs`

- [ ] **Step 1: 更新文本 cell 构造**

将：

```rust
TranscriptCell::User(text)
TranscriptCell::Assistant(text)
TranscriptCell::Reasoning(text)
TranscriptCell::System(message)
```

替换为：

```rust
TranscriptCell::Text(TextCell::new(TextRole::User, text))
TranscriptCell::Text(TextCell::new(TextRole::Assistant, text))
TranscriptCell::Text(TextCell::new(TextRole::Reasoning, text))
TranscriptCell::Text(TextCell::new(TextRole::System, message))
```

- [ ] **Step 2: 更新 append_to_last_or_push**

将 role 合并逻辑改成：

```rust
/// Appends text to the previous compatible cell or pushes a new cell.
fn append_to_last_or_push(
    transcript: &mut Vec<TranscriptCell>,
    text: String,
    role: TextRole,
) {
    match transcript.last_mut() {
        Some(TranscriptCell::Text(existing)) if existing.role() == role => {
            existing.push_str(&text);
        }
        _ => transcript.push(TranscriptCell::Text(TextCell::new(role, text))),
    }
}
```

删除旧 `TranscriptRole` enum。

- [ ] **Step 3: 更新 tool call placeholder**

将 `empty_tool_call_view` 改成：

```rust
/// Builds the placeholder tool call used when updates arrive before snapshots.
fn empty_tool_call_cell(call_id: String) -> ToolCallCell {
    ToolCallCell::pending(call_id)
}
```

将调用点：

```rust
TranscriptCell::ToolCall(empty_tool_call_view(call_id.clone()))
```

替换为：

```rust
TranscriptCell::ToolCall(empty_tool_call_cell(call_id.clone()))
```

- [ ] **Step 4: 更新 tool call mutation**

将直接字段赋值：

```rust
entry.name = title;
entry.arguments = raw_input;
entry.status = status;
entry.output.push_str(&output);
```

替换为 methods：

```rust
entry.set_name(title);
entry.set_arguments(raw_input);
entry.set_status(status);
entry.push_output(&output);
```

`apply_tool_call_update` 中同理使用 `set_name`、`set_arguments`、`push_output`、`set_status`。

- [ ] **Step 5: 更新测试 pattern matching**

将 state tests 中类似：

```rust
TranscriptCell::Reasoning("thinking".to_string())
```

替换为基于行为的断言：

```rust
assert_eq!(state.transcript()[0].text(), "thinking");
```

将 tool cell pattern：

```rust
TranscriptCell::ToolCall(tool) => tool
```

保留不变，因为 `ToolCall` 变体仍存在。

- [ ] **Step 6: 验证 state tests**

Run:

```bash
rtk cargo test -p tui state_acp_message_chunks_append_to_assistant_cell
rtk cargo test -p tui state_acp_thought_chunks_append_to_reasoning_cell
rtk cargo test -p tui state_tool_call_update_mutates_existing_transcript_cell
```

Expected: 三个测试通过。

---

### Task 7: 精简 transcript renderer

**Files:**
- Modify: `crates/tui/src/ui/transcript.rs`

- [ ] **Step 1: 删除 role-specific imports**

删除：

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use crate::ui::state::{AppState, TranscriptCell};
use crate::ui::tool_render::append_tool_call_lines;
```

保留：

```rust
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{layout::Rect, Frame};

use crate::ui::state::AppState;
use crate::ui::view::ViewState;
```

- [ ] **Step 2: 让 transcript renderer 委托 cell**

将 loop 改成：

```rust
for cell in state.transcript() {
    lines.extend(cell.display_lines(area.width));
    lines.push(Line::from(""));
}
```

- [ ] **Step 3: 删除 `append_transcript_cell_lines` 和 `append_styled_text_lines`**

删除这两个函数及其 text style 测试。该测试已在 `cell/text.rs` 覆盖。

- [ ] **Step 4: 验证 transcript render tests**

Run:

```bash
rtk cargo test -p tui transcript_area_is_borderless
rtk cargo test -p tui transcript_scrolls_to_latest_output
rtk cargo test -p tui transcript_manual_scroll_shows_older_output
```

Expected: 三个测试通过。

---

### Task 8: 删除旧 tool helper 顶层模块

**Files:**
- Delete: `crates/tui/src/ui/tool_render.rs`
- Delete: `crates/tui/src/ui/tool_summary.rs`
- Delete: `crates/tui/src/ui/terminal_output.rs`
- Modify: `crates/tui/src/ui/mod.rs`

- [ ] **Step 1: 删除旧模块声明**

更新 `crates/tui/src/ui/mod.rs`，删除：

```rust
pub mod terminal_output;
pub mod tool_render;
pub mod tool_summary;
```

确保保留：

```rust
pub(crate) mod cell;
```

- [ ] **Step 2: 删除旧文件**

删除：

```text
crates/tui/src/ui/tool_render.rs
crates/tui/src/ui/tool_summary.rs
crates/tui/src/ui/terminal_output.rs
```

- [ ] **Step 3: 查找残留引用**

Run:

```bash
rtk rg -n "tool_render|tool_summary|terminal_output|append_tool_call_lines|tool_summary\\(" crates/tui/src/ui
```

Expected: 没有旧模块引用；允许 `cell/terminal_output.rs` 中出现 `terminal_display_lines`。

- [ ] **Step 4: 验证 tool render 行为**

Run:

```bash
rtk cargo test -p tui render_tool_call_defaults_to_preview
rtk cargo test -p tui render_tool_call_shell_preview_uses_codex_style
rtk cargo test -p tui render_tool_call_titles_for_supported_categories
```

Expected: 三个测试通过。

---

### Task 9: 暴露 raw/desired behavior 并补最小测试

**Files:**
- Modify: `crates/tui/src/ui/cell/mod.rs`
- Modify: `crates/tui/src/ui/cell/tool.rs`
- Modify: `crates/tui/src/ui/cell/text.rs`

- [ ] **Step 1: 验证 raw lines 可用**

在 `cell/mod.rs` tests 中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies transcript cells expose raw lines independently from styled display lines.
    #[test]
    fn transcript_cell_raw_lines_are_plain_text() {
        let cell = TranscriptCell::Text(TextCell::new(TextRole::User, "hello"));

        let raw = cell.raw_lines();

        assert_eq!(raw[0].spans[0].content, "hello");
    }
}
```

- [ ] **Step 2: 验证 desired height 可用**

继续添加：

```rust
/// Verifies desired height is computed from rendered display lines.
#[test]
fn transcript_cell_desired_height_is_nonzero_for_text() {
    let cell = TranscriptCell::Text(TextCell::new(TextRole::Assistant, "hello"));

    assert_eq!(cell.desired_height(80), 1);
}
```

- [ ] **Step 3: 运行 cell tests**

Run:

```bash
rtk cargo test -p tui transcript_cell_raw_lines_are_plain_text
rtk cargo test -p tui transcript_cell_desired_height_is_nonzero_for_text
```

Expected: 两个测试通过。

---

### Task 10: Full Verification

**Files:**
- No new files.

- [ ] **Step 1: 格式化**

Run:

```bash
rtk cargo fmt --all
```

Expected: 成功。

- [ ] **Step 2: 运行 TUI tests**

Run:

```bash
rtk cargo test -p tui
```

Expected: 所有 TUI tests 通过。

- [ ] **Step 3: 运行 workspace clippy**

Run:

```bash
rtk cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 无 warnings。

- [ ] **Step 4: 运行 pre-commit**

Run:

```bash
rtk pre-commit run --all-files
```

Expected: Rust fmt / clippy hooks 通过。

- [ ] **Step 5: 检查结构残留**

Run:

```bash
rtk rg -n "append_transcript_cell_lines|append_tool_call_lines|ToolCallView|tool_render|tool_summary" crates/tui/src/ui
```

Expected: 没有旧 helper 和旧类型残留。

- [ ] **Step 6: 检查 diff 范围**

Run:

```bash
rtk git diff --stat
rtk git status --short
```

Expected: 改动集中在 `crates/tui/src/ui` 和 typed cells spec/plan 文档。

---

## Review Gate

实现完成并通过验证后，停止并让用户 review。不要创建 commit，除非用户明确说 review 通过并要求提交。

## Self-Review

Spec coverage:

1. Typed cell 模块结构由 Tasks 1、2、4、5 覆盖。
2. ToolCallCell 自己拥有 summary/status/preview/output normalize 行为，由 Tasks 3、4、8 覆盖。
3. `transcript.rs` 不再 match cell 类型，由 Task 7 覆盖。
4. 不引入 trait object，由执行约束和 Task 5 的 enum 存储覆盖。
5. raw/desired behavior 由 Task 9 覆盖。

Placeholder scan:

1. 计划中没有未完成占位项。
2. 每个任务都有明确文件、步骤和验证命令。
3. 删除旧 helper 模块的条件明确。

Type consistency:

1. `TranscriptCell`、`TextCell`、`TextRole`、`ToolCallCell` 的路径均为 `crate::ui::cell::*`。
2. `ToolCallView` 只允许作为临时迁移别名，最终验证要求无残留。
3. `ToolCallCell` 字段超过 3 个，使用 `typed-builder`。

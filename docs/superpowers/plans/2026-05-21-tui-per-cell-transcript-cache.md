# TUI Per-Cell Transcript Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 TUI transcript 从全局 wrapped rows cache 升级为 Codex 风格的 trait-object cell、per-cell render cache 与 active/committed entry 模型，避免 streaming 时重算全部历史。

**Architecture:** `TranscriptCell` trait 参考 Codex `HistoryCell` 设计，`TranscriptEntry` 持有稳定 id、revision、状态和 `Arc<dyn TranscriptCell>`。可变 streaming cell 通过 copy-on-write 方式更新：修改 entry 时用新 cell 替换 `Arc` 并 bump entry revision；cache 按 `entry_id + width + revision` 缓存 `transcript_lines` 的 wrapped rows，render 阶段只重算变化 entry，并通过引用区间只 clone viewport 可见 rows。

**Tech Stack:** Rust, ratatui `Line`/`Paragraph`/`Wrap`, typed-builder, existing ACP TUI state model, existing `ui/transcript/wrap` soft-wrap helper.

---

## 前置约束

- 本计划只覆盖 coding 前的执行步骤；开始实现前必须先由用户 review 通过。
- 所有新增/修改代码的非平凡逻辑必须有英文注释；新增函数必须有英文函数级注释。
- 不创建 commit，除非用户在实现完成后明确要求。
- 当前工作区已有上一轮 TUI 性能修复和 transcript 模块拆分的未提交改动；实现本计划时必须在这些改动基础上继续，不回滚用户或已有变更。

## 目标文件结构

### 新增文件

- `crates/tui/src/ui/transcript/cell.rs`
  - 定义 `TranscriptCell` trait、`TranscriptRenderMode`，以及 `impl dyn TranscriptCell` 的 downcast helpers。

- `crates/tui/src/ui/transcript/entry.rs`
  - 定义 `TranscriptEntryId`、`TranscriptEntryState`、`TranscriptEntry`。
  - 封装 entry revision bump、active/committed 状态转换、`Arc<dyn TranscriptCell>` 替换和 cell downcast helper。

- `crates/tui/src/ui/transcript/cache.rs`
  - 将当前 `TranscriptLinesCache` 替换为 `TranscriptRenderCache` 和 `CachedEntryLines`。
  - 负责 width 变化清空、entry revision 命中判断、per-entry rows 重算。

- `crates/tui/src/ui/transcript/wrap/mod.rs`
  - soft wrap 对外入口，暴露 `wrap_display_lines`。

- `crates/tui/src/ui/transcript/wrap/line.rs`
  - 单个 styled logical line 的 soft wrap。

- `crates/tui/src/ui/transcript/wrap/chars.rs`
  - `Line`/`Span` flatten 为 `StyledChar`，以及 styled chars 重建 `Line`。

- `crates/tui/src/ui/transcript/wrap/boundary.rs`
  - wrap boundary、尾部 whitespace trim、前导 whitespace skip。

### 修改文件

- `crates/tui/src/ui/cell/mod.rs`
  - 删除 `TranscriptCell` enum，改为 re-export `TranscriptCell` trait 和 concrete cells。
  - 保留 `TextCell`、`ToolCallCell` 的构造 API。

- `crates/tui/src/ui/cell/text.rs`
  - `TextCell` 实现 `TranscriptCell` trait。
  - 保留 `role()`、`text()`、`push_str()`。

- `crates/tui/src/ui/cell/tool.rs`
  - `ToolCallCell` 实现 `TranscriptCell` trait。
  - 保留 mutating setters 与 status/content API。

- `crates/tui/src/ui/state.rs`
  - 将 `transcript: Vec<TranscriptCell enum>` 改为 `transcript: Vec<TranscriptEntry>`。
  - 将全局 `transcript_revision` 移除或降级为兼容辅助，核心逻辑转向 entry revision。
  - 将 `tool_call_indices` 指向 entry vec index。
  - 增加 active assistant/reasoning entry 查找和状态转换 helper。
  - 保留 `Clone` derive 的前提是 `TranscriptEntry` 使用 `Arc<dyn TranscriptCell>`，并保证所有 mutation 都通过替换 cell + bump revision 完成。

- `crates/tui/src/ui/view.rs`
  - 将 `RefCell<Option<TranscriptLinesCache>>` 改为 `RefCell<TranscriptRenderCache>`。
  - 暴露 `with_transcript_render_cache` 或等价 helper，避免 view.rs 持有 cache 细节。

- `crates/tui/src/ui/transcript/mod.rs`
  - 使用 entries + render cache 渲染 transcript。
  - 不再构造全局 wrapped transcript cache。

- `crates/tui/src/ui/transcript/viewport.rs`
  - 保持 scroll offset 与 viewport 截取逻辑。
  - 输入从 cached row references / visible rows collection 来，禁止滚动时 clone 全量 transcript rows。

- `crates/tui/src/ui/render.rs`
  - 更新测试 helper 和断言以适配 trait-object entry。

## Task 1: 拆分 wrap 模块目录

**Files:**
- Create: `crates/tui/src/ui/transcript/wrap/mod.rs`
- Create: `crates/tui/src/ui/transcript/wrap/line.rs`
- Create: `crates/tui/src/ui/transcript/wrap/chars.rs`
- Create: `crates/tui/src/ui/transcript/wrap/boundary.rs`
- Delete: `crates/tui/src/ui/transcript/wrap.rs`
- Modify: `crates/tui/src/ui/transcript/mod.rs`

- [ ] **Step 1: 移动现有 wrap 代码到目录结构**

将当前 `wrap.rs` 的代码按职责拆开。`wrap/mod.rs` 只保留入口：

```rust
//! Soft wrapping helpers for transcript display lines.

use ratatui::text::Line;

mod boundary;
mod chars;
mod line;

/// Wraps styled logical lines into physical terminal rows before rendering.
pub(super) fn wrap_display_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    lines
        .into_iter()
        .flat_map(|line| line::wrap_display_line(line, usize::from(width)))
        .collect()
}
```

`wrap/line.rs`：

```rust
//! Single-line soft wrapping.

use ratatui::text::Line;

use super::boundary::next_wrap_range;
use super::chars::{styled_chars, styled_line_from_chars};

/// Wraps one styled line by character display width.
pub(super) fn wrap_display_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let chars = styled_chars(line);
    if chars.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let (end, next_start) = next_wrap_range(&chars, start, width);
        if let Some(slice) = chars.get(start..end) {
            wrapped.push(styled_line_from_chars(slice));
        }
        start = next_start;
    }
    wrapped
}
```

`wrap/chars.rs`：

```rust
//! Styled character conversion for transcript wrapping.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy)]
pub(super) struct StyledChar {
    pub(super) ch: char,
    pub(super) style: ratatui::style::Style,
    pub(super) width: usize,
}

/// Flattens a styled line into per-character units for wrapping.
pub(super) fn styled_chars(line: Line<'static>) -> Vec<StyledChar> {
    line.spans
        .into_iter()
        .flat_map(|span| {
            let style = span.style;
            span.content
                .chars()
                .map(move |ch| StyledChar {
                    ch,
                    style,
                    width: UnicodeWidthChar::width(ch).unwrap_or(0),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Builds a ratatui line from styled character units.
pub(super) fn styled_line_from_chars(chars: &[StyledChar]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut current_style = None;
    let mut current_text = String::new();

    for styled in chars {
        if current_style == Some(styled.style) {
            current_text.push(styled.ch);
            continue;
        }

        if let Some(style) = current_style {
            spans.push(Span::styled(std::mem::take(&mut current_text), style));
        }
        current_style = Some(styled.style);
        current_text.push(styled.ch);
    }

    if let Some(style) = current_style {
        spans.push(Span::styled(current_text, style));
    }

    Line::from(spans)
}
```

`wrap/boundary.rs`：

```rust
//! Wrap boundary helpers for styled characters.

use super::chars::StyledChar;

/// Returns the visible range and next start offset for one wrapped row.
pub(super) fn next_wrap_range(
    chars: &[StyledChar],
    start: usize,
    width: usize,
) -> (usize, usize) {
    let mut used_width = 0usize;
    let mut end = start;
    let mut last_space = None;
    while let Some(styled) = chars.get(end) {
        let next_width = used_width + styled.width;
        if used_width > 0 && next_width > width {
            break;
        }
        used_width = next_width;
        end += 1;
        if styled.ch.is_whitespace() && end > start + 1 {
            last_space = Some(end);
        }
    }

    if end == chars.len() {
        return (end, end);
    }

    if let Some(space_end) = last_space {
        let visible_end = trim_trailing_whitespace(chars, start, space_end);
        return (
            visible_end.max(start + 1),
            skip_leading_whitespace(chars, space_end),
        );
    }

    (end.max(start + 1), end.max(start + 1))
}

/// Removes whitespace at the end of a soft-wrapped row.
fn trim_trailing_whitespace(chars: &[StyledChar], start: usize, mut end: usize) -> usize {
    while end > start
        && chars
            .get(end - 1)
            .map(|styled| styled.ch.is_whitespace())
            .unwrap_or(false)
    {
        end -= 1;
    }
    end
}

/// Skips whitespace consumed as the soft-wrap boundary.
fn skip_leading_whitespace(chars: &[StyledChar], mut start: usize) -> usize {
    while chars
        .get(start)
        .map(|styled| styled.ch.is_whitespace())
        .unwrap_or(false)
    {
        start += 1;
    }
    start
}
```

- [ ] **Step 2: 删除旧文件并确认模块引用**

Run:

```bash
rtk cargo fmt --all
rtk cargo test -p tui transcript_ -- --nocapture
```

Expected:

```text
cargo test: ... passed
```

## Task 2: 引入 Codex 风格 TranscriptCell trait 并移除 enum 冲突

**Files:**
- Create: `crates/tui/src/ui/transcript/cell.rs`
- Modify: `crates/tui/src/ui/transcript/mod.rs`
- Modify: `crates/tui/src/ui/cell/mod.rs`
- Modify: `crates/tui/src/ui/cell/text.rs`
- Modify: `crates/tui/src/ui/cell/tool.rs`

- [ ] **Step 1: 新增 trait 与 render mode**

Create `crates/tui/src/ui/transcript/cell.rs`:

```rust
//! Trait-object transcript cells for TUI rendering.

use std::any::Any;

use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};

use crate::ui::theme::Theme;

/// Render mode for transcript cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptRenderMode {
    /// Rich terminal rendering.
    Rich,
    /// Copy-friendly plain rendering.
    Raw,
}

/// A single renderable unit in the TUI transcript.
pub trait TranscriptCell: std::fmt::Debug + Send + Sync + Any {
    /// Returns logical lines for the main rich transcript view.
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>>;

    /// Returns copy-friendly plain logical lines.
    fn raw_lines(&self) -> Vec<Line<'static>>;

    /// Returns logical lines for one render mode.
    fn display_lines_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> Vec<Line<'static>> {
        match mode {
            TranscriptRenderMode::Rich => self.display_lines(width, theme),
            TranscriptRenderMode::Raw => self.raw_lines(),
        }
    }

    /// Returns viewport rows needed by rich display lines.
    fn desired_height(&self, width: u16, theme: &Theme) -> u16 {
        self.desired_height_for_mode(width, theme, TranscriptRenderMode::Rich)
    }

    /// Returns viewport rows needed by one render mode.
    fn desired_height_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> u16 {
        Paragraph::new(Text::from(self.display_lines_for_mode(width, theme, mode)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    /// Returns logical lines for transcript cache rendering.
    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.display_lines(width, theme)
    }

    /// Returns viewport rows needed by transcript lines.
    fn desired_transcript_height(&self, width: u16, theme: &Theme) -> u16 {
        let lines = self.transcript_lines(width, theme);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    /// Returns whether this cell continues a previous stream segment.
    fn is_stream_continuation(&self) -> bool {
        false
    }

    /// Returns a coarse cache key tick for animated transcript output.
    fn transcript_animation_tick(&self) -> Option<u64> {
        None
    }
}

impl dyn TranscriptCell {
    /// Returns this cell as `Any` for type-specific state updates.
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    /// Returns this mutable cell as `Any` for type-specific state updates.
    pub fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
```

Modify `crates/tui/src/ui/transcript/mod.rs`:

```rust
pub(super) mod cache;
pub mod cell;
```

- [ ] **Step 2: 先删除旧 enum 类型，避免 `TranscriptCell` 名称冲突**

在 `crates/tui/src/ui/cell/mod.rs` 中删除旧的 `pub enum TranscriptCell` 及其 impl block。保留 concrete cell exports，并 re-export trait：

```rust
//! Typed transcript cells for the local TUI.

mod terminal_output;
mod text;
mod tool;

pub use super::transcript::cell::{TranscriptCell, TranscriptRenderMode};
pub use text::{TextCell, TextRole};
pub use tool::ToolCallCell;
```

删除旧 enum 后，所有旧的 `TranscriptCell::text_cell(...)` / `TranscriptCell::tool_call(...)` 调用会暂时编译失败；Task 3 会统一迁移到 `TranscriptEntry`。不要临时重命名 enum，避免中间态和最终设计不一致。

- [ ] **Step 3: Implement trait for TextCell**

In `crates/tui/src/ui/cell/text.rs`, add:

```rust
use crate::ui::transcript::cell::TranscriptCell;
```

Replace the display/raw methods with trait implementation:

```rust
impl TranscriptCell for TextCell {
    /// Returns styled logical lines using the configured render theme.
    fn display_lines(&self, _width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let (first_prefix, style) = match self.role {
            TextRole::Assistant => ("", Style::default().fg(theme.text)),
            TextRole::Reasoning => (
                "",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            ),
            TextRole::User => ("> ", Style::default().add_modifier(Modifier::BOLD)),
            TextRole::System => ("system: ", Style::default().fg(theme.muted)),
        };
        styled_text_lines(&self.text, first_prefix, style)
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect()
    }

}
```

Keep inherent methods:

```rust
impl TextCell {
    /// Returns styled logical lines for default dark theme tests.
    pub fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        <Self as TranscriptCell>::display_lines(self, width, &Theme::dark())
    }

    /// Returns styled logical lines using the configured render theme.
    pub fn display_lines_with_theme(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        <Self as TranscriptCell>::display_lines(self, width, theme)
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    pub fn raw_lines(&self) -> Vec<Line<'static>> {
        <Self as TranscriptCell>::raw_lines(self)
    }
}
```

- [ ] **Step 4: Implement trait for ToolCallCell**

In `crates/tui/src/ui/cell/tool.rs`, add:

```rust
use crate::ui::transcript::cell::TranscriptCell;
```

Add:

```rust
impl TranscriptCell for ToolCallCell {
    /// Returns styled logical lines using the configured render theme.
    fn display_lines(&self, _width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            status_bullet(self.status, theme),
            " ".into(),
            Span::styled(
                format!("{} {}", status_verb(self.status), self.summary()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        append_tool_output_preview_lines(
            &mut lines,
            self.status,
            &self.output,
            self.diffs.is_empty(),
        );
        append_tool_diff_preview_lines(&mut lines, &self.diffs, theme);
        lines
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(format!(
            "{} {}",
            status_verb(self.status),
            self.summary()
        ))];
        lines.extend(
            terminal_display_lines(&self.output)
                .into_iter()
                .map(Line::from),
        );
        for diff in &self.diffs {
            lines.extend(diff.raw_lines().into_iter().map(Line::from));
        }
        lines
    }

}
```

Keep inherent wrappers as in `TextCell`.

- [ ] **Step 5: Run targeted tests**

Run:

```bash
rtk cargo test -p tui text_cell_ tool_call_cell_ -- --nocapture
```

Expected:

```text
cargo test: ... passed
```

## Task 3: 引入 TranscriptEntry 并替换 AppState transcript storage

**Files:**
- Create: `crates/tui/src/ui/transcript/entry.rs`
- Modify: `crates/tui/src/ui/transcript/mod.rs`
- Modify: `crates/tui/src/ui/state.rs`
- Modify: `crates/tui/src/ui/cell/mod.rs`

- [ ] **Step 1: Add entry types**

Create `crates/tui/src/ui/transcript/entry.rs`:

```rust
//! Transcript entry metadata and mutable cell wrapper.

use std::sync::Arc;

use crate::ui::cell::{TextCell, ToolCallCell};
use crate::ui::transcript::cell::TranscriptCell;

/// Stable internal id for transcript entry cache keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranscriptEntryId(u64);

impl TranscriptEntryId {
    /// Creates a new stable entry id.
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Mutability state for one transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptEntryState {
    /// Entry is stable and should only rerender on width/theme changes.
    Committed,
    /// Entry may still receive streaming updates.
    Active,
}

/// One transcript cell plus cache metadata.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    id: TranscriptEntryId,
    revision: u64,
    state: TranscriptEntryState,
    cell: Arc<dyn TranscriptCell>,
}

impl TranscriptEntry {
    /// Creates a transcript entry around one cell.
    pub fn new(
        id: TranscriptEntryId,
        state: TranscriptEntryState,
        cell: Arc<dyn TranscriptCell>,
    ) -> Self {
        Self {
            id,
            revision: 0,
            state,
            cell,
        }
    }

    /// Returns the stable entry id.
    pub fn id(&self) -> TranscriptEntryId {
        self.id
    }

    /// Returns the render revision for this entry.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns whether this entry is active or committed.
    pub fn state(&self) -> TranscriptEntryState {
        self.state
    }

    /// Returns the underlying cell.
    pub fn cell(&self) -> Arc<dyn TranscriptCell> {
        Arc::clone(&self.cell)
    }

    /// Replaces the underlying cell and bumps this entry revision.
    pub fn replace_cell(&mut self, cell: Arc<dyn TranscriptCell>) {
        self.cell = cell;
        self.bump_revision();
    }

    /// Marks this entry as committed.
    pub fn commit(&mut self) {
        self.state = TranscriptEntryState::Committed;
    }

    /// Marks this entry as active.
    pub fn activate(&mut self) {
        self.state = TranscriptEntryState::Active;
    }

    /// Bumps this entry revision after mutating cell contents.
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Returns this entry as a text cell when possible.
    pub fn text_cell(&self) -> Option<&TextCell> {
        self.cell.as_ref().as_any().downcast_ref::<TextCell>()
    }

    /// Returns this entry as a tool-call cell when possible.
    pub fn tool_call(&self) -> Option<&ToolCallCell> {
        self.cell.as_ref().as_any().downcast_ref::<ToolCallCell>()
    }
}
```

Modify `transcript/mod.rs`:

```rust
pub mod entry;
```

- [ ] **Step 2: Update AppState fields**

In `crates/tui/src/ui/state.rs`, replace:

```rust
transcript: Vec<TranscriptCell>,
transcript_revision: u64,
tool_call_indices: HashMap<String, usize>,
```

with:

```rust
/// Ordered transcript entries ready for rendering.
#[builder(default)]
transcript: Vec<TranscriptEntry>,
/// Next stable transcript entry id.
#[builder(default)]
next_transcript_entry_id: u64,
/// Transcript entry index for each tool call id.
#[builder(default)]
tool_call_indices: HashMap<String, usize>,
```

Add imports:

```rust
use std::sync::Arc;

use crate::ui::cell::{TextCell, TextRole, ToolCallCell};
use crate::ui::transcript::entry::{TranscriptEntry, TranscriptEntryId, TranscriptEntryState};
```

Keep the struct derive as:

```rust
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
```

This remains valid because entries hold `Arc<dyn TranscriptCell>`. Mutation must never require `&mut dyn TranscriptCell`; replace the cell with a new `Arc` and bump the entry revision instead.

- [ ] **Step 3: Add AppState entry helpers**

Add these helpers to `impl AppState`:

```rust
/// Returns the renderable transcript entries.
pub fn transcript(&self) -> &[TranscriptEntry] {
    &self.transcript
}

/// Allocates the next stable transcript entry id.
fn next_entry_id(&mut self) -> TranscriptEntryId {
    let id = TranscriptEntryId::new(self.next_transcript_entry_id);
    self.next_transcript_entry_id = self.next_transcript_entry_id.wrapping_add(1);
    id
}

/// Pushes a new transcript entry and returns its index.
fn push_entry(&mut self, state: TranscriptEntryState, cell: Arc<dyn TranscriptCell>) -> usize {
    let id = self.next_entry_id();
    let index = self.transcript.len();
    self.transcript.push(TranscriptEntry::new(id, state, cell));
    index
}

/// Pushes a committed text entry.
fn push_committed_text(&mut self, role: TextRole, text: impl Into<String>) {
    self.push_entry(
        TranscriptEntryState::Committed,
        Arc::new(TextCell::new(role, text)),
    );
}
```

Remove or stop using `transcript_revision()` and `bump_transcript_revision()`.

- [ ] **Step 4: Update user/system paths to committed entries**

Replace user/system pushes:

```rust
self.transcript
    .push(TranscriptCell::text_cell(TextRole::User, text));
```

with:

```rust
self.push_committed_text(TextRole::User, text);
```

For errors:

```rust
self.push_committed_text(TextRole::System, message);
```

- [ ] **Step 5: Update tests that read `state.transcript()[index]`**

Replace enum matching helpers with downcast helpers:

```rust
fn transcript_tool(state: &AppState, index: usize) -> &ToolCallCell {
    state.transcript()[index]
        .tool_call()
        .expect("expected tool cell")
}
```

For text:

```rust
fn transcript_text(state: &AppState, index: usize) -> &TextCell {
    state.transcript()[index]
        .text_cell()
        .expect("expected text cell")
}
```

- [ ] **Step 6: Run compile-focused tests**

Run:

```bash
rtk cargo test -p tui state_append_user_message_clears_pending_approval -- --nocapture
```

Expected:

```text
cargo test: 1 passed
```

## Task 4: Active/committed mutation rules

**Files:**
- Modify: `crates/tui/src/ui/state.rs`
- Modify: `crates/tui/src/ui/transcript/entry.rs`

- [ ] **Step 1: Add active text lookup helper**

Add to `AppState`:

```rust
/// Returns the last active text entry index for the requested role.
fn last_active_text_entry_index(&self, role: TextRole) -> Option<usize> {
    self.transcript
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| {
            if entry.state() != TranscriptEntryState::Active {
                return None;
            }
            let text = entry.text_cell()?;
            (text.role() == role).then_some(index)
        })
}

/// Appends text to the last active text entry or creates a new active entry.
fn append_to_active_text_or_push(&mut self, role: TextRole, text: String) {
    if let Some(index) = self.last_active_text_entry_index(role)
        && let Some(entry) = self.transcript.get_mut(index)
        && let Some(cell) = entry.text_cell()
    {
        let mut updated = cell.clone();
        updated.push_str(&text);
        entry.replace_cell(Arc::new(updated));
        return;
    }

    self.push_entry(
        TranscriptEntryState::Active,
        Arc::new(TextCell::new(role, text)),
    );
}
```

- [ ] **Step 2: Use active helper for assistant/reasoning chunks**

Replace:

```rust
Self::append_to_last_or_push(&mut self.transcript, text, TextRole::Assistant);
self.bump_transcript_revision();
```

with:

```rust
self.append_to_active_text_or_push(TextRole::Assistant, text);
```

Do the same for `TextRole::Reasoning`.

- [ ] **Step 3: Commit active text entries on finish_prompt**

Update `finish_prompt`:

```rust
/// Records a prompt completion returned by ACP.
pub fn finish_prompt(&mut self, stop_reason: StopReason) {
    for entry in &mut self.transcript {
        if entry.state() == TranscriptEntryState::Active
            && entry.text_cell().is_some()
        {
            entry.commit();
        }
    }
    self.running_prompt = false;
    self.last_stop_reason = Some(stop_reason);
    self.pending_approval = None;
}
```

- [ ] **Step 4: Update tool creation/update to bump only one entry**

Update `pending_tool_call_cell` into an index-returning helper:

```rust
/// Returns an existing tool entry index or creates a pending placeholder.
fn pending_tool_call_entry_index(&mut self, call_id: String) -> usize {
    if let Some(index) = self.tool_call_indices.get(&call_id) {
        return *index;
    }

    let index = self.push_entry(
        TranscriptEntryState::Active,
        Arc::new(ToolCallCell::pending(call_id.clone())),
    );
    self.tool_call_indices.insert(call_id, index);
    index
}
```

Add:

```rust
/// Applies a mutation to a copied tool-call entry and bumps only that entry.
fn mutate_tool_call(
    &mut self,
    call_id: String,
    mutate: impl FnOnce(&mut ToolCallCell),
) {
    let index = self.pending_tool_call_entry_index(call_id);
    let Some(entry) = self.transcript.get_mut(index) else {
        unreachable!("tool call index must point inside transcript");
    };
    let Some(tool) = entry.tool_call() else {
        unreachable!("tool call index must point at a tool call transcript cell");
    };
    let was_committed = entry.state() == TranscriptEntryState::Committed;
    let mut updated = tool.clone();
    mutate(&mut updated);
    if was_committed {
        entry.replace_cell(Arc::new(updated));
        entry.commit();
        return;
    }

    let is_terminal = matches!(
        updated.status(),
        agent_client_protocol::schema::ToolCallStatus::Completed
            | agent_client_protocol::schema::ToolCallStatus::Failed
    );
    entry.replace_cell(Arc::new(updated));
    if is_terminal {
        entry.commit();
    } else {
        entry.activate();
    }
}
```

Use `mutate_tool_call` from `apply_tool_call` and `apply_tool_call_update`.

This structure avoids borrow checker conflicts: the immutable `tool` reference is used only to clone into `updated`; all entry mutation happens after that reference is no longer used.

Fixed late-update rule: once a tool entry is committed, any later update may replace the cell and bump the entry revision, but it must keep the entry committed regardless of the updated status payload. Only entries that were active before the mutation may transition through the status-based active/committed branch.

- [ ] **Step 5: Add active/committed tests**

Add tests in `state.rs`:

```rust
/// Verifies assistant chunks mutate one active entry revision.
#[test]
fn state_agent_chunks_mutate_one_active_text_entry() {
    let session_id = sid("s1");
    let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

    state.apply_session_update(notification(
        session_id.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hel"))),
    ));
    let first_revision = state.transcript()[0].revision();
    state.apply_session_update(notification(
        session_id,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(text("lo"))),
    ));

    assert_eq!(state.transcript().len(), 1);
    assert!(state.transcript()[0].revision() > first_revision);
    assert_eq!(state.transcript()[0].state(), TranscriptEntryState::Active);
    assert_eq!(
        state.transcript()[0]
            .text_cell()
            .expect("assistant text cell")
            .text(),
        "hello"
    );
}
```

```rust
/// Verifies finish_prompt commits active assistant entries.
#[test]
fn state_finish_prompt_commits_active_text_entries() {
    let session_id = sid("s1");
    let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

    state.apply_session_update(notification(
        session_id,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hello"))),
    ));
    state.finish_prompt(StopReason::EndTurn);

    assert_eq!(state.transcript()[0].state(), TranscriptEntryState::Committed);
}
```

```rust
/// Verifies late tool updates cannot reactivate a committed tool entry.
#[test]
fn state_late_tool_update_keeps_committed_tool_entry_committed() {
    let session_id = sid("s1");
    let call_id = "call-1".to_string();
    let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

    state.apply_session_update(notification(
        session_id.clone(),
        SessionUpdate::ToolCall(
            ToolCall::new(ToolCallId::new(&call_id), "shell")
                .status(ToolCallStatus::Completed),
        ),
    ));
    let first_revision = state.transcript()[0].revision();

    state.apply_session_update(notification(
        session_id,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            ToolCallId::new(&call_id),
            ToolCallUpdateFields::new()
                .content(vec![ToolCallContent::Content(Content::new(text(
                    "late output",
                )))])
                .status(ToolCallStatus::InProgress),
        )),
    ));

    assert!(state.transcript()[0].revision() > first_revision);
    assert_eq!(state.transcript()[0].state(), TranscriptEntryState::Committed);
}
```

- [ ] **Step 6: Run state tests**

Run:

```bash
rtk cargo test -p tui state_ -- --nocapture
```

Expected:

```text
cargo test: ... passed
```

## Task 5: Replace global cache with TranscriptRenderCache

**Files:**
- Modify: `crates/tui/src/ui/transcript/cache.rs`
- Modify: `crates/tui/src/ui/view.rs`
- Modify: `crates/tui/src/ui/transcript/mod.rs`
- Modify: `crates/tui/src/ui/transcript/viewport.rs`

- [ ] **Step 1: Implement render cache**

Replace `TranscriptLinesCache` with:

```rust
//! Cached wrapped transcript rows for the local TUI.

use std::collections::HashMap;

use ratatui::text::Line;

use crate::ui::theme::Theme;
use crate::ui::transcript::entry::{TranscriptEntry, TranscriptEntryId};
use crate::ui::transcript::wrap::wrap_display_lines;

/// Cached soft-wrapped rows for one transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct CachedEntryLines {
    revision: u64,
    lines: Vec<Line<'static>>,
}

/// Per-entry transcript render cache for one terminal width.
#[derive(Debug, Default)]
pub(in crate::ui) struct TranscriptRenderCache {
    width: Option<u16>,
    entries: HashMap<TranscriptEntryId, CachedEntryLines>,
    #[cfg(test)]
    rebuild_count: usize,
}

impl TranscriptRenderCache {
    /// Creates an empty render cache.
    pub(in crate::ui) fn new() -> Self {
        Self::default()
    }

    /// Returns cached wrapped rows for one entry, rebuilding only when needed.
    pub(in crate::ui) fn entry_lines(
        &mut self,
        width: u16,
        theme: &Theme,
        entry: &TranscriptEntry,
    ) -> &[Line<'static>] {
        if self.width != Some(width) {
            self.width = Some(width);
            self.entries.clear();
        }

        let needs_rebuild = self
            .entries
            .get(&entry.id())
            .map(|cached| cached.revision != entry.revision())
            .unwrap_or(true);

        if needs_rebuild {
            #[cfg(test)]
            {
                self.rebuild_count = self.rebuild_count.saturating_add(1);
            }
            let lines = wrap_display_lines(entry.cell().transcript_lines(width, theme), width);
            self.entries.insert(
                entry.id(),
                CachedEntryLines {
                    revision: entry.revision(),
                    lines,
                },
            );
        }

        self.entries
            .get(&entry.id())
            .map(|cached| cached.lines.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the number of cached rows for one entry, rebuilding only when needed.
    pub(in crate::ui) fn entry_line_count(
        &mut self,
        width: u16,
        theme: &Theme,
        entry: &TranscriptEntry,
    ) -> usize {
        self.entry_lines(width, theme, entry).len()
    }

    /// Retains cache entries that still exist in transcript state.
    pub(in crate::ui) fn retain_entries(&mut self, ids: impl Iterator<Item = TranscriptEntryId>) {
        let live = ids.collect::<std::collections::HashSet<_>>();
        self.entries.retain(|id, _| live.contains(id));
    }

    /// Returns the number of entry rebuilds in tests.
    #[cfg(test)]
    pub(in crate::ui) fn rebuild_count(&self) -> usize {
        self.rebuild_count
    }
}
```

- [ ] **Step 2: Update ViewState cache field and manual impls**

In `view.rs`, replace:

```rust
transcript_lines_cache: RefCell<Option<TranscriptLinesCache>>,
```

with:

```rust
transcript_render_cache: RefCell<TranscriptRenderCache>,
```

Builder default:

```rust
#[builder(default = RefCell::new(TranscriptRenderCache::new()))]
```

Add:

```rust
/// Provides mutable access to the transcript render cache.
pub(super) fn with_transcript_render_cache<R>(
    &self,
    use_cache: impl FnOnce(&mut TranscriptRenderCache) -> R,
) -> R {
    use_cache(&mut self.transcript_render_cache.borrow_mut())
}
```

Clone should reset cache:

```rust
transcript_render_cache: RefCell::new(TranscriptRenderCache::new()),
```

Update the manual impls explicitly:

```rust
impl std::fmt::Debug for ViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewState")
            .field("transcript_scroll", &self.transcript_scroll)
            .field("follow_tail", &self.follow_tail)
            .finish_non_exhaustive()
    }
}

impl Clone for ViewState {
    fn clone(&self) -> Self {
        Self {
            transcript_scroll: self.transcript_scroll,
            follow_tail: self.follow_tail,
            transcript_render_cache: RefCell::new(TranscriptRenderCache::new()),
        }
    }
}

impl PartialEq for ViewState {
    fn eq(&self, other: &Self) -> bool {
        self.transcript_scroll == other.transcript_scroll && self.follow_tail == other.follow_tail
    }
}

impl Eq for ViewState {}
```

- [ ] **Step 3: Render entries through per-cell cache without cloning all rows**

In `transcript/mod.rs`, do not build a full `Vec<Line>` for the whole transcript on every render. Replace `wrapped_transcript_lines` with viewport-aware rendering:

```rust
/// Renders transcript entries through the per-entry cache.
fn render_cached_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
    cache: &mut TranscriptRenderCache,
) {
    cache.retain_entries(state.transcript().iter().map(TranscriptEntry::id));
    let total_rows = transcript_row_count(area.width, state, cache);
    if total_rows == 0 {
        viewport::render_transcript_lines(frame, area, &[Line::from("Ready. Type a message and press Enter.")], view);
        return;
    }

    let (start, end) = viewport::visible_row_range(total_rows, area, view);
    let visible = visible_transcript_lines(area.width, state, cache, start, end);
    viewport::render_transcript_lines_at_top(frame, area, &visible);
}

/// Counts transcript rows without cloning cached row contents.
fn transcript_row_count(width: u16, state: &AppState, cache: &mut TranscriptRenderCache) -> usize {
    state
        .transcript()
        .iter()
        .map(|entry| cache.entry_line_count(width, state.theme(), entry).saturating_add(1))
        .sum()
}

/// Clones only rows that intersect the requested visible range.
fn visible_transcript_lines(
    width: u16,
    state: &AppState,
    cache: &mut TranscriptRenderCache,
    start: usize,
    end: usize,
) -> Vec<Line<'static>> {
    let mut visible = Vec::new();
    let mut cursor = 0usize;
    for entry in state.transcript() {
        let lines = cache.entry_lines(width, state.theme(), entry);
        append_visible_rows(&mut visible, lines, cursor, start, end);
        cursor = cursor.saturating_add(lines.len());
        append_visible_rows(&mut visible, &[Line::from("")], cursor, start, end);
        cursor = cursor.saturating_add(1);
        if cursor >= end {
            break;
        }
    }
    visible
}

/// Appends only the intersection between one row slice and the visible range.
fn append_visible_rows(
    visible: &mut Vec<Line<'static>>,
    rows: &[Line<'static>],
    row_start: usize,
    visible_start: usize,
    visible_end: usize,
) {
    let row_end = row_start.saturating_add(rows.len());
    let start = row_start.max(visible_start);
    let end = row_end.min(visible_end);
    if start >= end {
        return;
    }
    let local_start = start.saturating_sub(row_start);
    let local_end = end.saturating_sub(row_start);
    if let Some(slice) = rows.get(local_start..local_end) {
        visible.extend(slice.iter().cloned());
    }
}
```

Update `render_transcript`:

```rust
view.with_transcript_render_cache(|cache| {
    render_cached_transcript(frame, area, state, view, cache);
});
```

Update `viewport.rs` to expose range calculation and top rendering:

```rust
/// Returns the visible transcript row range.
pub(super) fn visible_row_range(line_count: usize, area: Rect, view: &ViewState) -> (usize, usize) {
    let max_scroll = transcript_scroll_offset(line_count, area);
    let scroll_offset = effective_transcript_scroll(max_scroll, view);
    let start = usize::from(scroll_offset).min(line_count);
    let end = start.saturating_add(usize::from(area.height)).min(line_count);
    (start, end)
}

/// Renders already-sliced rows at the top of the transcript area.
pub(super) fn render_transcript_lines_at_top(
    frame: &mut Frame<'_>,
    area: Rect,
    visible_lines: &[Line<'static>],
) {
    frame.render_widget(Paragraph::new(visible_lines.to_vec()), area);
}
```

`render_transcript_lines` can remain as a compatibility wrapper for existing tests, but production code should use `visible_row_range` plus `render_transcript_lines_at_top` to avoid cloning all rows.

- [ ] **Step 4: Add cache tests**

In `cache.rs` tests, ensure imports include `std::sync::Arc` plus the concrete cell and entry types:

```rust
use std::sync::Arc;
```

```rust
/// Verifies only changed entries rebuild after an active update.
#[test]
fn render_cache_rebuilds_only_changed_entry() {
    let theme = Theme::dark();
    let mut cache = TranscriptRenderCache::new();
    let committed = TranscriptEntry::new(
        TranscriptEntryId::new(1),
        TranscriptEntryState::Committed,
        Arc::new(TextCell::new(TextRole::User, "old history")),
    );
    let mut active = TranscriptEntry::new(
        TranscriptEntryId::new(2),
        TranscriptEntryState::Active,
        Arc::new(TextCell::new(TextRole::Assistant, "hel")),
    );

    let _ = cache.entry_lines(80, &theme, &committed);
    let _ = cache.entry_lines(80, &theme, &active);
    assert_eq!(cache.rebuild_count(), 2);

    let text = active.text_cell().expect("text cell");
    let mut updated = text.clone();
    updated.push_str("lo");
    active.replace_cell(Arc::new(updated));
    let _ = cache.entry_lines(80, &theme, &committed);
    let _ = cache.entry_lines(80, &theme, &active);

    assert_eq!(cache.rebuild_count(), 3);
}
```

- [ ] **Step 5: Run cache/render tests**

Run:

```bash
rtk cargo test -p tui render_cache_ transcript_ -- --nocapture
```

Expected:

```text
cargo test: ... passed
```

## Task 6: Update transcript rendering tests and compatibility helpers

**Files:**
- Modify: `crates/tui/src/ui/render.rs`
- Modify: `crates/tui/src/ui/cell/mod.rs`
- Modify: `crates/tui/src/ui/state.rs`

- [ ] **Step 1: Remove old enum constructor usage**

Find:

```bash
rtk rg -n "TranscriptCell::|text_cell\\(|tool_call\\(" crates/tui/src
```

Replace production uses with entry helpers:

```rust
self.push_entry(
    TranscriptEntryState::Committed,
    Arc::new(TextCell::new(role, text)),
);
```

Replace test-only direct enum checks with trait downcast helpers from Task 3.

- [ ] **Step 2: Keep concrete cell test APIs stable**

Ensure `TextCell::display_lines`, `ToolCallCell::display_lines`, `raw_lines`, and `display_lines_with_theme` still exist as inherent wrappers. This avoids rewriting low-value tests and keeps direct cell tests simple.

- [ ] **Step 3: Run all TUI tests**

Run:

```bash
rtk cargo test -p tui -- --nocapture
```

Expected:

```text
cargo test: all TUI tests passed
```

## Task 7: Full verification and review handoff

**Files:**
- Modify only files already listed above.

- [ ] **Step 1: Format**

Run:

```bash
rtk cargo fmt --all
```

Expected: no output and exit code 0.

- [ ] **Step 2: Full TUI tests**

Run:

```bash
rtk cargo test -p tui -- --nocapture
```

Expected: all TUI tests pass.

- [ ] **Step 3: Full clippy**

Run:

```bash
rtk cargo clippy --all-targets --all-features --locked -- -D warnings
```

Expected:

```text
cargo clippy: No issues found
```

- [ ] **Step 4: Pre-commit**

Run:

```bash
rtk pre-commit run --all-files
```

Expected:

```text
Rust (fmt)...............................................................Passed
Rust (clippy)............................................................Passed
```

- [ ] **Step 5: Prepare review summary without commit**

Collect:

```bash
rtk git status --short
rtk git diff --stat
```

Expected:

```text
modified/new TUI transcript files and docs only
```

Final handoff should mention:

- trait object introduced and aligned with Codex `HistoryCell` semantics
- active/committed entry state implemented
- per-cell cache rebuilds only changed entries
- scroll-only rendering does not rebuild wraps
- verification commands and results
- no commit created

## Plan Self-Review

- Spec coverage: trait object, active/committed entries, per-cell cache, wrap module split, tests, and verification are covered by Tasks 1-7.
- Placeholder scan: no implementation step relies on unspecified behavior; code snippets provide concrete type and method names.
- Type consistency: `TranscriptCell`, `TranscriptEntry`, `TranscriptEntryId`, `TranscriptEntryState`, and `TranscriptRenderCache` names are consistent across tasks.
- Scope control: this plan does not introduce Codex terminal scrollback/reflow, markdown renderer changes, or ACP protocol changes.

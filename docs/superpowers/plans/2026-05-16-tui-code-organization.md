# TUI Code Organization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the TUI rendering helpers into focused `ui` submodules without changing visible behavior.

**Architecture:** Keep `AppState` and ACP reducer behavior stable while moving pure rendering, layout, summary, and output-normalization helpers into domain modules. `ui/render.rs` becomes the top-level frame composer that delegates to focused modules.

**Tech Stack:** Rust, ratatui, crossterm, agent-client-protocol, serde_json, unicode-width, existing `tui` crate tests.

---

## Ground Rules

1. Do not change visible TUI behavior in Phase 1.
2. Do not introduce Codex-style `HistoryCell` trait objects in this plan.
3. Do not create commits during implementation unless the user explicitly approves after review.
4. Every newly added or modified Rust function must have an English function-level comment.
5. Keep non-trivial logic comments in English, especially ANSI parsing, carriage-return handling, scroll offset, and layout decisions.
6. Prefer `pub(super)` or `pub(crate)` only when cross-module access requires it.

## File Structure

Create:

- `crates/tui/src/ui/layout.rs`  
  Owns frame geometry, composer height calculation, and centered modal rectangles.
- `crates/tui/src/ui/terminal_output.rs`  
  Owns ANSI stripping, carriage-return handling, display-line conversion, and preview line constants.
- `crates/tui/src/ui/tool_summary.rs`  
  Owns tool name/argument summary strings.
- `crates/tui/src/ui/tool_render.rs`  
  Owns `ToolCallView` to ratatui `Line` conversion.
- `crates/tui/src/ui/transcript.rs`  
  Owns transcript area rendering, scroll calculations, and transcript cell line conversion.
- `crates/tui/src/ui/status.rs`  
  Owns rendering of top and bottom status rows for Phase 1. State-owned status string construction can remain in `AppState` until Phase 2.

Modify:

- `crates/tui/src/ui/mod.rs`  
  Export the new internal modules.
- `crates/tui/src/ui/render.rs`  
  Reduce to top-level composition and remove moved helpers/tests.
- `crates/tui/src/ui/approval.rs`  
  Move approval modal line rendering here.
- `crates/tui/src/ui/state.rs`  
  Keep unchanged unless visibility adjustments are required by moved tests or status rendering.

Test:

- `cargo test -p tui`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `pre-commit run --all-files`

---

### Task 1: Add UI Module Skeletons

**Files:**
- Create: `crates/tui/src/ui/layout.rs`
- Create: `crates/tui/src/ui/terminal_output.rs`
- Create: `crates/tui/src/ui/tool_summary.rs`
- Create: `crates/tui/src/ui/tool_render.rs`
- Create: `crates/tui/src/ui/transcript.rs`
- Create: `crates/tui/src/ui/status.rs`
- Modify: `crates/tui/src/ui/mod.rs`

- [ ] **Step 1: Create empty focused modules**

Add each new file with only a module-level comment:

```rust
//! Layout helpers for the local TUI.
```

Use the matching descriptions:

```rust
//! Terminal output normalization for the local TUI.
//! Tool-call summary helpers for the local TUI.
//! Tool-call rendering helpers for the local TUI.
//! Transcript rendering helpers for the local TUI.
//! Status row rendering helpers for the local TUI.
```

- [ ] **Step 2: Register the modules**

Update `crates/tui/src/ui/mod.rs`:

```rust
//! Local UI state, input, approval, and ratatui rendering.

pub mod approval;
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

- [ ] **Step 3: Verify module skeletons compile**

Run:

```bash
cargo test -p tui
```

Expected: all existing `tui` tests pass.

---

### Task 2: Move Layout Helpers

**Files:**
- Modify: `crates/tui/src/ui/layout.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Move composer height and centered rect**

Move these functions from `render.rs` to `layout.rs`:

```rust
pub(super) fn composer_height(text: &str) -> u16
pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect
```

Keep their existing English function comments. Import `ratatui::layout::Rect` in `layout.rs`.

- [ ] **Step 2: Add a small layout model for frame rows**

Add this struct to `layout.rs`:

```rust
/// Holds the vertical regions used by the main TUI frame.
pub(super) struct FrameRows {
    pub(super) transcript: Rect,
    pub(super) top_status: Rect,
    pub(super) composer: Rect,
    pub(super) bottom_status: Rect,
    pub(super) help: Option<Rect>,
}
```

Add this function:

```rust
/// Splits the terminal frame into stable vertical regions.
pub(super) fn frame_rows(area: Rect, composer_text: &str) -> Option<FrameRows>
```

The body should reuse the exact existing `show_help`, `transcript_min`, `Constraint`, and `Layout` logic from `render`.

- [ ] **Step 3: Update top-level render to use layout helpers**

In `render.rs`, replace inline layout calculation with:

```rust
let Some(rows) = layout::frame_rows(frame.area(), composer_text) else {
    return;
};
```

Then use:

```rust
rows.transcript
rows.top_status
rows.composer
rows.bottom_status
rows.help
```

- [ ] **Step 4: Move layout tests**

Move `composer_height_with_limits` and `centered_rect_stays_in_area` from `render.rs` tests into `layout.rs`.

- [ ] **Step 5: Verify layout move**

Run:

```bash
cargo test -p tui composer_height_with_limits centered_rect_stays_in_area
cargo test -p tui render_handles_small_terminal
```

Expected: moved tests and small terminal render test pass.

---

### Task 3: Move Terminal Output Normalization

**Files:**
- Modify: `crates/tui/src/ui/terminal_output.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Move terminal output functions**

Move these functions from `render.rs` to `terminal_output.rs`:

```rust
pub(super) fn terminal_display_lines(text: &str) -> Vec<String>
fn strip_ansi_control_sequences(text: &str) -> String
```

Keep `strip_ansi_control_sequences` private to `terminal_output.rs`.

- [ ] **Step 2: Keep carriage-return behavior unchanged**

The moved `terminal_display_lines` must preserve the existing behavior:

```rust
'\r' followed by '\n' pushes the current line
'\r' not followed by '\n' clears the current line
other control characters are ignored
```

Do not change output semantics in this task.

- [ ] **Step 3: Move terminal output tests**

Move the terminal-output assertions currently embedded in `render_tool_output_handles_carriage_return_updates` into a direct unit test in `terminal_output.rs`:

```rust
#[test]
fn terminal_display_lines_handles_carriage_return_updates()
```

Keep the existing render-level test if it verifies final screen behavior.

- [ ] **Step 4: Verify terminal output move**

Run:

```bash
cargo test -p tui terminal_display_lines_handles_carriage_return_updates
cargo test -p tui render_tool_output_handles_carriage_return_updates
```

Expected: both tests pass.

---

### Task 4: Move Tool Summary Helpers

**Files:**
- Modify: `crates/tui/src/ui/tool_summary.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Move tool summary functions**

Move these functions from `render.rs` to `tool_summary.rs`:

```rust
pub(super) fn tool_summary(call: &ToolCallView) -> String
fn tool_arguments(arguments: &str) -> serde_json::Value
fn shell_summary(args: &serde_json::Value) -> String
fn read_file_summary(args: &serde_json::Value) -> String
fn path_summary(prefix: &str, args: &serde_json::Value, field: &str) -> String
fn edit_summary(args: &serde_json::Value) -> String
fn spawn_agent_summary(args: &serde_json::Value) -> String
fn message_tool_summary(prefix: &str, args: &serde_json::Value) -> String
fn mcp_summary(name: &str, args: &serde_json::Value) -> String
fn unknown_tool_summary(name: &str, arguments: &str) -> String
fn string_field<'a>(args: &'a serde_json::Value, field: &str) -> Option<&'a str>
fn truncate_chars(value: &str, max_chars: usize) -> String
fn compact_inline(text: &str) -> String
```

Import `crate::ui::state::ToolCallView`.

- [ ] **Step 2: Preserve unknown-tool fallback**

The fallback must still return only the tool name when compacted arguments are empty:

```rust
if args == "<empty>" {
    name.to_string()
} else {
    format!("{name} {args}")
}
```

- [ ] **Step 3: Move summary tests**

Move these tests from `render.rs` into `tool_summary.rs` or convert their summary-specific assertions into direct unit tests:

```rust
render_tool_call_titles_for_supported_categories
render_subagent_message_tool_titles_include_bounded_content_preview
```

The tests should assert summary strings directly where possible, and leave one render-level test for end-to-end screen output.

- [ ] **Step 4: Verify tool summaries**

Run:

```bash
cargo test -p tui render_tool_call_titles_for_supported_categories
cargo test -p tui render_subagent_message_tool_titles_include_bounded_content_preview
```

Expected: tests pass after migration or replacement with direct summary tests.

---

### Task 5: Move Tool Rendering Helpers

**Files:**
- Modify: `crates/tui/src/ui/tool_render.rs`
- Modify: `crates/tui/src/ui/render.rs`
- Modify: `crates/tui/src/ui/transcript.rs` if Task 6 has already created transcript rendering

- [ ] **Step 1: Move tool rendering functions**

Move these functions and constant from `render.rs` to `tool_render.rs`:

```rust
const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;
pub(super) fn append_tool_call_lines(lines: &mut Vec<Line<'static>>, call: &ToolCallView)
fn append_tool_output_preview_lines(
    lines: &mut Vec<Line<'static>>,
    status: ToolCallStatus,
    text: &str,
)
fn dim_line(text: impl Into<String>) -> Line<'static>
fn status_verb(status: ToolCallStatus) -> &'static str
fn status_bullet(status: ToolCallStatus) -> Span<'static>
```

Import:

```rust
use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use crate::ui::state::ToolCallView;
use crate::ui::terminal_output::terminal_display_lines;
use crate::ui::tool_summary::tool_summary;
```

- [ ] **Step 2: Keep preview rules unchanged**

The moved renderer must preserve:

```text
completed or failed with no output -> "  └ (no output)"
running with no output -> header only
output preview -> first 5 display lines
overflow -> "    ... +N lines"
```

- [ ] **Step 3: Move tool render tests**

Move or keep end-to-end render tests so these behaviors remain covered:

```rust
render_tool_call_defaults_to_preview
render_tool_call_shell_preview_uses_codex_style
render_running_tool_call_without_output_shows_header_only
```

Prefer direct unit tests in `tool_render.rs` for line generation plus one render-level integration test.

- [ ] **Step 4: Verify tool rendering**

Run:

```bash
cargo test -p tui render_tool_call_defaults_to_preview
cargo test -p tui render_tool_call_shell_preview_uses_codex_style
cargo test -p tui render_running_tool_call_without_output_shows_header_only
```

Expected: tests pass with identical visible text.

---

### Task 6: Move Transcript Rendering

**Files:**
- Modify: `crates/tui/src/ui/transcript.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Move transcript functions**

Move these functions from `render.rs` to `transcript.rs`:

```rust
pub(super) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    view: &ViewState,
)
fn effective_transcript_scroll(max_scroll: u16, view: &ViewState) -> u16
fn transcript_scroll_offset(line_count: usize, area: Rect) -> u16
fn append_transcript_cell_lines(lines: &mut Vec<Line<'static>>, cell: &TranscriptCell)
fn append_styled_text_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    first_prefix: &str,
    style: Style,
)
```

Import:

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};
use crate::ui::state::{AppState, TranscriptCell};
use crate::ui::tool_render::append_tool_call_lines;
use crate::ui::view::ViewState;
```

- [ ] **Step 2: Keep role styling unchanged**

Preserve current styles:

```text
assistant -> reset foreground
reasoning -> dark gray italic
user -> bold with "> " first-line prefix
system -> dark gray with "system: " first-line prefix
tool call -> delegated to tool_render
```

- [ ] **Step 3: Move transcript tests**

Move or keep render-level tests so these cases stay covered:

```rust
render_transcript_is_borderless
render_reasoning_uses_distinct_style
render_transcript_scrolls_to_latest_output
render_transcript_manual_scroll_shows_older_output
```

Direct unit tests in `transcript.rs` should cover `effective_transcript_scroll` and `transcript_scroll_offset`.

- [ ] **Step 4: Verify transcript rendering**

Run:

```bash
cargo test -p tui render_transcript_is_borderless
cargo test -p tui render_reasoning_uses_distinct_style
cargo test -p tui render_transcript_scrolls_to_latest_output
cargo test -p tui render_transcript_manual_scroll_shows_older_output
```

Expected: tests pass with the same screen text and scroll behavior.

---

### Task 7: Move Status and Approval Rendering

**Files:**
- Modify: `crates/tui/src/ui/status.rs`
- Modify: `crates/tui/src/ui/approval.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Add status row render helpers**

Add these functions to `status.rs`:

```rust
/// Renders the top status row.
pub(super) fn render_top_status(frame: &mut Frame<'_>, area: Rect, state: &AppState)

/// Renders the bottom status row constrained to the available terminal width.
pub(super) fn render_bottom_status(frame: &mut Frame<'_>, area: Rect, state: &AppState)
```

The functions should call the existing `state.top_status_line()` and `state.bottom_status_line(area.width as usize)`.

- [ ] **Step 2: Move approval modal line rendering**

Move this function from `render.rs` to `approval.rs`:

```rust
pub(crate) fn approval_lines(title: &str, body: &str) -> Vec<Line<'static>>
```

Keep the existing bold title, blank lines, body text, and dim key hint.

- [ ] **Step 3: Update render.rs callers**

Replace direct status rendering with:

```rust
status::render_top_status(frame, rows.top_status, state);
status::render_bottom_status(frame, rows.bottom_status, state);
```

Replace direct `approval_lines` access with:

```rust
approval::approval_lines(approval.title(), approval.body())
```

- [ ] **Step 4: Verify status and approval behavior**

Run:

```bash
cargo test -p tui state_bottom_status_includes_model_cwd_and_tokens
cargo test -p tui state_bottom_status_line_fits_narrow_width
cargo test -p tui approval_from_acp_request_extracts_id_title_and_body
```

Expected: existing state and approval tests still pass.

---

### Task 8: Shrink Top-Level Render

**Files:**
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Remove moved imports**

After the prior tasks, `render.rs` should no longer import:

```rust
ToolCallStatus
Color
Modifier
Layout
Direction
Constraint
ToolCallView
TranscriptCell
```

Keep only imports needed for top-level composition:

```rust
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::{approval, layout, status, transcript};
use crate::ui::state::AppState;
use crate::ui::view::ViewState;
```

If `render_composer` and `render_help_bar` remain in `render.rs`, keep their required `Color`, `Style`, `Paragraph`, and `Wrap` imports.

- [ ] **Step 2: Keep render.rs as orchestration**

`render.rs` should contain only:

```rust
pub fn render(...)
fn render_composer(...)
fn render_help_bar(...)
```

Render-level integration tests can remain in this file when they exercise full frame behavior.

- [ ] **Step 3: Verify file responsibilities**

Run:

```bash
rg -n "fn (tool_summary|terminal_display_lines|append_tool_call_lines|render_transcript|centered_rect|composer_height|approval_lines)" crates/tui/src/ui/render.rs
```

Expected: no output.

Run:

```bash
rg -n "fn render\\(|fn render_composer|fn render_help_bar" crates/tui/src/ui/render.rs
```

Expected: only the top-level render and local composer/help functions appear.

---

### Task 9: Full Verification

**Files:**
- No new files.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: command succeeds.

- [ ] **Step 2: Run TUI tests**

Run:

```bash
cargo test -p tui
```

Expected: all `tui` tests pass.

- [ ] **Step 3: Run workspace clippy**

Run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clippy passes with no warnings.

- [ ] **Step 4: Run pre-commit**

Run:

```bash
pre-commit run --all-files
```

Expected: all hooks pass.

- [ ] **Step 5: Inspect diff**

Run:

```bash
git diff --stat
git diff -- crates/tui/src/ui docs/superpowers/specs/2026-05-16-tui-code-organization-design.md docs/superpowers/plans/2026-05-16-tui-code-organization.md
```

Expected: changes are limited to TUI UI module reorganization plus the spec/plan docs.

---

## Review Gate

Stop after verification and ask the user to review the diff. Do not create a commit until the user explicitly confirms review passed and asks for a commit.

## Self-Review

Spec coverage:

1. Module split from the spec is covered by Tasks 1 through 8.
2. Behavior preservation is covered by per-task tests and full verification in Task 9.
3. Codex-style `HistoryCell` is explicitly excluded in Ground Rules.
4. Phase 2 reducer cleanup is intentionally not implemented by this plan.

Placeholder scan:

1. The plan contains no unresolved placeholder sections.
2. Every task names exact files and commands.
3. No task asks the implementer to invent behavior beyond moving existing code.

Type consistency:

1. `FrameRows` uses `Rect` consistently.
2. Cross-module helpers use `pub(super)` unless `approval_lines` needs `pub(crate)` for module access.
3. Tool rendering still depends on `ToolCallView`, `ToolCallStatus`, `terminal_display_lines`, and `tool_summary`.

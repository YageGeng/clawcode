# TUI Tool Call Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render ACP tool calls as ordered transcript cells with Codex-style previews capped at five output lines.

**Architecture:** `AppState` will make tool calls part of `TranscriptCell` and keep a private call-id index only for updates. Rendering will walk transcript cells in order and delegate tool-specific summaries to focused renderer helpers. The old global tool-call tail rendering and `Ctrl+T` toggle behavior will be removed from the first pass.

**Tech Stack:** Rust, ratatui, crossterm, agent-client-protocol schema, existing TUI unit tests.

---

### Task 1: Transcript-First Tool State

**Files:**
- Modify: `crates/tui/src/ui/state.rs`

- [ ] **Step 1: Write failing state tests**

Add tests that assert `SessionUpdate::ToolCall` inserts `TranscriptCell::ToolCall`, `ToolCallUpdate` updates that same cell, and an update arriving first creates a pending tool cell.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tui state_tool_call`

Expected: tests fail because `TranscriptCell::ToolCall` and transcript-first storage do not exist.

- [ ] **Step 3: Implement transcript-first state**

Add `TranscriptCell::ToolCall(ToolCallView)`, add private `tool_call_indices: HashMap<String, usize>`, and update `apply_tool_call`, `apply_tool_call_update`, and placeholder creation to mutate transcript cells by index.

- [ ] **Step 4: Verify state tests**

Run: `cargo test -p tui state_tool_call`

Expected: new state tests pass.

### Task 2: Codex-Style Tool Renderer

**Files:**
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Write failing render tests**

Add render tests for shell, read_file, write_file, edit, apply_patch, subagent, MCP, and unknown tools. Each test should assert the rendered title and confirm output preview is capped at five lines plus an omitted-line marker.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tui render_tool`

Expected: tests fail because rendering still appends global tool calls and uses the old `[status]` format.

- [ ] **Step 3: Implement renderer helpers**

Replace global `state.tool_calls()` tail rendering with transcript-cell rendering. Add helpers to build tool summaries, state verbs, output preview lines, MCP name parsing, and compact argument fallbacks.

- [ ] **Step 4: Verify render tests**

Run: `cargo test -p tui render_tool`

Expected: new render tests pass.

### Task 3: Remove First-Pass Toggle UI

**Files:**
- Modify: `crates/tui/src/app.rs`
- Modify: `crates/tui/src/ui/view.rs`
- Modify: `crates/tui/src/ui/render.rs`

- [ ] **Step 1: Write/update tests for no toggle dependency**

Update old collapsed-tool tests so tool calls always render the same five-line preview. Remove or replace view tests that only cover `tool_calls_collapsed`.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p tui tool_call`

Expected: tests fail until the old toggle state and key binding are removed.

- [ ] **Step 3: Remove toggle behavior**

Remove `Ctrl+T` handling from `app.rs`, remove `tool_calls_collapsed` from `ViewState`, and update render signatures to stop depending on collapsed mode.

- [ ] **Step 4: Verify package**

Run: `cargo test -p tui`

Expected: all TUI tests pass.

### Task 4: Final Verification

**Files:**
- Verify only

- [ ] **Step 1: Format check**

Run: `cargo fmt --check -p tui`

Expected: no output and exit code 0.

- [ ] **Step 2: Full tests**

Run: `cargo test -p tui`

Expected: all tests pass.

- [ ] **Step 3: Clippy**

Run: `cargo clippy -p tui --all-targets -- -D warnings`

Expected: no warnings and exit code 0.

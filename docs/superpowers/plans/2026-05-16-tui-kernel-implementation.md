# TUI Kernel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a first-pass local TUI that talks directly to `protocol::AgentKernel`, renders streamed kernel events with `ratatui + crossterm`, and supports session lifecycle, prompt submission, cancellation, approval, model/cwd/token status, and safe terminal restore.

**Architecture:** Add a new `crates/tui` binary crate. Keep ACP as a parallel external adapter; the TUI constructs `kernel::Kernel`, calls `AgentKernel` methods directly, consumes `protocol::Event`, and reduces those events into local render state. The UI is split into focused modules for terminal setup, event plumbing, composer state, approval state, app state, rendering, bootstrap, and the app loop.

**Tech Stack:** Rust 2024, `tokio`, `futures`, `ratatui 0.29`, `crossterm 0.28`, `tokio-stream`, `unicode-width`, existing `protocol`, `kernel`, `config`, `provider`, and `tools` crates.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `Cargo.toml` | Modify | Add TUI workspace dependencies. |
| `crates/tui/Cargo.toml` | Create | Define the new TUI crate and dependencies. |
| `crates/tui/src/lib.rs` | Create | Module declarations and shared exports for tests. |
| `crates/tui/src/main.rs` | Create | CLI entrypoint and top-level error handling. |
| `crates/tui/src/bootstrap.rs` | Create | Build `Arc<kernel::Kernel>` from config/provider/tools. |
| `crates/tui/src/composer.rs` | Create | Editable prompt buffer and key handling. |
| `crates/tui/src/approval.rs` | Create | Normalize approval events and map keys to `ReviewDecision`. |
| `crates/tui/src/state.rs` | Create | Transcript, tool state, usage, runtime status, and event reducer. |
| `crates/tui/src/event.rs` | Create | Terminal/app event types and crossterm event stream adapter. |
| `crates/tui/src/terminal.rs` | Create | Raw mode, bracketed paste, alternate screen, and restore guard. |
| `crates/tui/src/render.rs` | Create | Ratatui layout and widgets. |
| `crates/tui/src/app.rs` | Create | Session startup, prompt tasks, cancellation, approval resolution, and main loop. |

Project rule reminder: do not run `git commit` unless the user explicitly grants permission. Plan steps include verification checkpoints instead of commits.

---

### Task 1: Add TUI Crate Skeleton And Dependencies

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/tui/Cargo.toml`
- Create: `crates/tui/src/lib.rs`
- Create: `crates/tui/src/main.rs`

- [ ] **Step 1: Add workspace dependencies**

Modify root `Cargo.toml` under `[workspace.dependencies]`:

```toml
# tui
crossterm = "0.28"
ratatui = "0.29"
tokio-stream = "0.1"
unicode-width = "0.2"
```

- [ ] **Step 2: Create `crates/tui/Cargo.toml`**

```toml
[package]
name = "tui"
edition.workspace = true
version.workspace = true
description = "Local terminal UI for clawcode"

[lints]
workspace = true

[[bin]]
name = "claw-tui"
path = "src/main.rs"

[dependencies]
protocol = { path = "../protocol" }
kernel = { path = "../kernel" }
config = { path = "../config" }
provider = { path = "../provider" }
tools = { path = "../tools" }

anyhow = { workspace = true }
clap = { workspace = true, features = ["derive"] }
crossterm = { workspace = true, features = ["bracketed-paste", "event-stream"] }
futures = { workspace = true }
ratatui = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std", "signal", "time", "sync"] }
tokio-stream = { workspace = true, features = ["sync"] }
tracing = { workspace = true }
tracing-subscriber = { workspace = true, features = ["env-filter"] }
unicode-width = { workspace = true }
```

- [ ] **Step 3: Create `crates/tui/src/lib.rs`**

```rust
//! Local terminal UI for clawcode.
//!
//! The TUI talks directly to `protocol::AgentKernel` and renders
//! `protocol::Event` values with ratatui.

pub mod app;
pub mod approval;
pub mod bootstrap;
pub mod composer;
pub mod event;
pub mod render;
pub mod state;
pub mod terminal;
```

- [ ] **Step 4: Create a temporary minimal `main.rs`**

```rust
//! Entry point for the clawcode TUI binary.

/// Run the TUI binary.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    Ok(())
}
```

- [ ] **Step 5: Verify the skeleton builds**

Run:

```bash
cargo check -p tui
```

Expected: command succeeds with no compile errors.

---

### Task 2: Implement Composer State With Tests

**Files:**
- Create/Modify: `crates/tui/src/composer.rs`

- [ ] **Step 1: Add failing composer tests**

Append tests in `crates/tui/src/composer.rs` while initially leaving implementation minimal enough to fail:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn composer_inserts_plain_characters_at_cursor() {
        let mut composer = Composer::default();

        assert_eq!(composer.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)), ComposerAction::Redraw);
        assert_eq!(composer.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)), ComposerAction::Redraw);

        assert_eq!(composer.text(), "hi");
        assert_eq!(composer.cursor(), 2);
    }

    #[test]
    fn composer_submits_and_clears_on_enter() {
        let mut composer = Composer::default();
        composer.insert_str("hello");

        let action = composer.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Submit("hello".to_string()));
        assert!(composer.is_empty());
        assert_eq!(composer.cursor(), 0);
    }

    #[test]
    fn composer_ctrl_j_inserts_newline() {
        let mut composer = Composer::default();
        composer.insert_str("a");

        let action = composer.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "a\n");
    }

    #[test]
    fn composer_backspace_removes_previous_character() {
        let mut composer = Composer::default();
        composer.insert_str("abc");
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        assert_eq!(composer.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)), ComposerAction::Redraw);
        assert_eq!(composer.text(), "ac");
        assert_eq!(composer.cursor(), 1);
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p tui composer_
```

Expected: fails because `Composer`, `ComposerAction`, or methods are not implemented.

- [ ] **Step 3: Implement composer**

Create implementation in `crates/tui/src/composer.rs`:

```rust
//! Editable prompt composer state.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Action produced after a composer key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    /// Redraw the TUI without submitting.
    Redraw,
    /// Submit the current prompt text.
    Submit(String),
    /// The key was not handled by the composer.
    Ignored,
}

/// Editable prompt buffer used by the TUI input area.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    text: String,
    cursor: usize,
}

impl Composer {
    /// Return the current composer text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the current byte cursor position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Return true when the composer has no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Clear the composer text and reset the cursor.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Insert a string at the current cursor position.
    pub fn insert_str(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    /// Handle a key event and return the resulting composer action.
    pub fn handle_key(&mut self, event: KeyEvent) -> ComposerAction {
        match (event.code, event.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let submitted = self.text.trim_end().to_string();
                self.clear();
                ComposerAction::Submit(submitted)
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.insert_str("\n");
                ComposerAction::Redraw
            }
            (KeyCode::Char(ch), KeyModifiers::NONE) | (KeyCode::Char(ch), KeyModifiers::SHIFT) => {
                self.insert_str(ch.encode_utf8(&mut [0; 4]));
                ComposerAction::Redraw
            }
            (KeyCode::Backspace, _) => {
                self.delete_before_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Delete, _) => {
                self.delete_at_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Left, _) => {
                self.move_left();
                ComposerAction::Redraw
            }
            (KeyCode::Right, _) => {
                self.move_right();
                ComposerAction::Redraw
            }
            (KeyCode::Home, _) => {
                self.cursor = 0;
                ComposerAction::Redraw
            }
            (KeyCode::End, _) => {
                self.cursor = self.text.len();
                ComposerAction::Redraw
            }
            _ => ComposerAction::Ignored,
        }
    }

    /// Remove the character immediately before the cursor.
    fn delete_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.previous_boundary(self.cursor);
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
    }

    /// Remove the character at the cursor.
    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..next, "");
    }

    /// Move the cursor one character left.
    fn move_left(&mut self) {
        self.cursor = self.previous_boundary(self.cursor);
    }

    /// Move the cursor one character right.
    fn move_right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
    }

    /// Return the previous UTF-8 boundary at or before `index`.
    fn previous_boundary(&self, index: usize) -> usize {
        self.text[..index].char_indices().last().map(|(idx, _)| idx).unwrap_or(0)
    }

    /// Return the next UTF-8 boundary after `index`.
    fn next_boundary(&self, index: usize) -> usize {
        self.text[index..]
            .char_indices()
            .nth(1)
            .map(|(offset, _)| index + offset)
            .unwrap_or_else(|| self.text.len())
    }
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p tui composer_
```

Expected: composer tests pass.

---

### Task 3: Implement Approval State With Tests

**Files:**
- Create/Modify: `crates/tui/src/approval.rs`

- [ ] **Step 1: Add failing approval tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use protocol::{Event, ReviewDecision, SessionId};

    #[test]
    fn pending_approval_from_exec_event_extracts_call_id_and_summary() {
        let event = Event::ExecApprovalRequested {
            session_id: SessionId("s1".to_string()),
            call_id: "call-1".to_string(),
            tool_name: "shell".to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
            cwd: "/tmp".into(),
        };

        let approval = PendingApproval::from_event(&event).expect("approval");

        assert_eq!(approval.call_id(), "call-1");
        assert!(approval.title().contains("shell"));
        assert!(approval.body().contains("ls"));
    }

    #[test]
    fn approval_keys_map_to_review_decisions() {
        let allow = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let reject = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(decision_for_key(allow), Some(ReviewDecision::AllowOnce));
        assert_eq!(decision_for_key(reject), Some(ReviewDecision::RejectOnce));
        assert_eq!(decision_for_key(escape), Some(ReviewDecision::RejectOnce));
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p tui approval_
```

Expected: fails because `PendingApproval` and `decision_for_key` are missing.

- [ ] **Step 3: Implement approval state**

```rust
//! Approval overlay state and key mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use protocol::{Event, ReviewDecision};

/// A normalized approval request that the overlay can render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    call_id: String,
    title: String,
    body: String,
}

impl PendingApproval {
    /// Build a pending approval from a kernel event.
    pub fn from_event(event: &Event) -> Option<Self> {
        match event {
            Event::ExecApprovalRequested {
                call_id,
                tool_name,
                arguments,
                cwd,
                ..
            } => Some(Self {
                call_id: call_id.clone(),
                title: format!("Approve {tool_name}"),
                body: format!("cwd: {}\nargs: {arguments}", cwd.display()),
            }),
            Event::PermissionRequested { request, .. } => Some(Self {
                call_id: request.call_id.clone(),
                title: "Permission requested".to_string(),
                body: request.message.clone(),
            }),
            _ => None,
        }
    }

    /// Return the call id used to resolve this approval.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Return the title shown in the overlay.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the body shown in the overlay.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Map approval overlay key input to a kernel review decision.
pub fn decision_for_key(event: KeyEvent) -> Option<ReviewDecision> {
    match (event.code, event.modifiers) {
        (KeyCode::Char('a') | KeyCode::Char('y'), KeyModifiers::NONE) => {
            Some(ReviewDecision::AllowOnce)
        }
        (KeyCode::Char('r') | KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Esc, _) => {
            Some(ReviewDecision::RejectOnce)
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p tui approval_
```

Expected: approval tests pass.

---

### Task 4: Implement AppState Event Reducer And Runtime Status

**Files:**
- Create/Modify: `crates/tui/src/state.rs`

- [ ] **Step 1: Add failing reducer tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentPath, Event, SessionId, ToolCallDeltaContent, ToolCallStatus};

    #[test]
    fn message_chunks_append_to_assistant_cell() {
        let mut state = AppState::new(SessionId("s1".to_string()), "/tmp".into(), "deepseek/model".to_string());

        state.apply_event(Event::message_chunk(SessionId("s1".to_string()), "hel"));
        state.apply_event(Event::message_chunk(SessionId("s1".to_string()), "lo"));

        assert_eq!(state.transcript()[0].text(), "hello");
    }

    #[test]
    fn tool_call_delta_and_tool_call_update_tool_state() {
        let mut state = AppState::new(SessionId("s1".to_string()), "/tmp".into(), "deepseek/model".to_string());

        state.apply_event(Event::tool_call_delta(
            SessionId("s1".to_string()),
            "call-1",
            ToolCallDeltaContent::name("shell"),
        ));
        state.apply_event(Event::tool_call(
            SessionId("s1".to_string()),
            AgentPath::root(),
            "call-1",
            "shell",
            serde_json::json!({"cmd": "pwd"}),
            ToolCallStatus::InProgress,
        ));
        state.apply_event(Event::tool_call_update(
            SessionId("s1".to_string()),
            "call-1",
            Some("/tmp".to_string()),
            Some(ToolCallStatus::Completed),
        ));

        let tool = state.tool_calls().get("call-1").expect("tool");
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.output(), "/tmp");
        assert_eq!(tool.status(), ToolCallStatus::Completed);
    }

    #[test]
    fn bottom_status_includes_model_cwd_and_tokens() {
        let mut state = AppState::new(SessionId("s1".to_string()), "/tmp/project".into(), "deepseek/model".to_string());
        state.apply_event(Event::usage_update(SessionId("s1".to_string()), 10, 20));

        let status = state.bottom_status_line(80);

        assert!(status.contains("deepseek/model"));
        assert!(status.contains("/tmp/project"));
        assert!(status.contains("30"));
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p tui state_
```

Expected: fails because state types are missing.

- [ ] **Step 3: Implement state types and reducer**

Implement `AppState`, `TranscriptCell`, `ToolCallView`, and `UsageView`. Use `typed-builder` only if a struct exceeds three fields; otherwise normal constructors are fine.

Required public API:

```rust
//! Renderable TUI state reduced from kernel events.

use std::collections::HashMap;
use std::path::PathBuf;

use protocol::{Event, SessionId, StopReason, ToolCallDeltaContent, ToolCallStatus};

use crate::approval::PendingApproval;

/// A transcript row rendered in the main transcript area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptCell {
    Assistant(String),
    Reasoning(String),
    User(String),
    System(String),
}

impl TranscriptCell {
    /// Return the display text for this transcript cell.
    pub fn text(&self) -> &str {
        match self {
            Self::Assistant(text) | Self::Reasoning(text) | Self::User(text) | Self::System(text) => text,
        }
    }
}

/// Renderable state for a tool call.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ToolCallView {
    /// Stable tool call id.
    pub call_id: String,
    /// Tool name shown to the user.
    pub name: String,
    /// JSON arguments rendered as text.
    pub arguments: String,
    /// Accumulated tool output.
    pub output: String,
    /// Current tool status.
    pub status: ToolCallStatus,
}
```

Continue implementation with:

- `ToolCallView::name()`, `output()`, `status()`.
- `AppState::new(session_id, cwd, model_label)`.
- `AppState::apply_event(event)`.
- `AppState::append_user_message(text)`.
- `AppState::set_error(message)`.
- `AppState::top_status_line()`.
- `AppState::bottom_status_line(width)`.
- `AppState::take_pending_approval()`, `pending_approval()`, `clear_pending_approval()`.

Implementation rules:

- `AgentMessageChunk` appends to the last assistant cell if it is already assistant, otherwise creates a new assistant cell.
- `AgentThoughtChunk` appends to the last reasoning cell if it is already reasoning.
- `ToolCallDelta::Name` creates or updates a pending `ToolCallView`.
- `ToolCall` replaces the tool entry with full name/arguments/status.
- `ToolCallUpdate` appends output and updates status.
- `UsageUpdate` stores input, output, and total token counts.
- `TurnComplete` clears `running_prompt` and records `stop_reason`.
- Approval events store `PendingApproval::from_event(&event)`.
- `bottom_status_line(width)` includes `model`, shortened `cwd`, and token usage.

- [ ] **Step 4: Run reducer tests**

Run:

```bash
cargo test -p tui state_
```

Expected: state tests pass.

---

### Task 5: Implement Terminal Guard And TUI Event Stream

**Files:**
- Create/Modify: `crates/tui/src/terminal.rs`
- Create/Modify: `crates/tui/src/event.rs`

- [ ] **Step 1: Add event mapping tests**

In `crates/tui/src/event.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn maps_key_resize_and_paste_events() {
        assert!(matches!(
            map_crossterm_event(CrosstermEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))),
            Some(TuiEvent::Key(_))
        ));
        assert_eq!(map_crossterm_event(CrosstermEvent::Resize(80, 24)), Some(TuiEvent::Resize));
        assert_eq!(map_crossterm_event(CrosstermEvent::Paste("abc".to_string())), Some(TuiEvent::Paste("abc".to_string())));
    }
}
```

- [ ] **Step 2: Implement event mapping**

```rust
//! Terminal and application event types.

use crossterm::event::{Event as CrosstermEvent, KeyEvent};

/// Terminal input event consumed by the app loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Tick,
}

/// Map a crossterm event into a TUI event.
pub fn map_crossterm_event(event: CrosstermEvent) -> Option<TuiEvent> {
    match event {
        CrosstermEvent::Key(key) => Some(TuiEvent::Key(key)),
        CrosstermEvent::Paste(text) => Some(TuiEvent::Paste(text.replace('\r', "\n"))),
        CrosstermEvent::Resize(_, _) => Some(TuiEvent::Resize),
        CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => Some(TuiEvent::Resize),
        _ => None,
    }
}
```

- [ ] **Step 3: Implement terminal guard**

```rust
//! Terminal setup and restore helpers.

use std::io::{self, Stdout};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Ratatui terminal type used by the TUI.
pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Restores terminal modes when dropped.
pub struct TerminalGuard {
    alt_screen: bool,
    restored: bool,
}

impl TerminalGuard {
    /// Enter terminal UI mode and return the terminal plus restore guard.
    pub fn enter(use_alt_screen: bool) -> anyhow::Result<(TuiTerminal, Self)> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        if use_alt_screen {
            execute!(io::stdout(), EnterAlternateScreen)?;
        }
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        Ok((
            terminal,
            Self {
                alt_screen: use_alt_screen,
                restored: false,
            },
        ))
    }

    /// Restore terminal modes immediately.
    pub fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        if self.alt_screen {
            execute!(io::stdout(), LeaveAlternateScreen)?;
        }
        execute!(io::stdout(), DisableBracketedPaste)?;
        disable_raw_mode()?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
```

- [ ] **Step 4: Run tests and check**

Run:

```bash
cargo test -p tui event_
cargo check -p tui
```

Expected: tests pass and crate checks.

---

### Task 6: Implement Ratatui Render

**Files:**
- Create/Modify: `crates/tui/src/render.rs`

- [ ] **Step 1: Add render smoke test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::SessionId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn render_handles_small_terminal() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = AppState::new(SessionId("s1".to_string()), "/tmp/project".into(), "deepseek/model".to_string());

        terminal.draw(|frame| render(frame, &state, "")).expect("draw");
    }
}
```

- [ ] **Step 2: Implement renderer**

```rust
//! Ratatui rendering for the TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::state::{AppState, TranscriptCell};

/// Render the complete TUI frame.
pub fn render(frame: &mut Frame<'_>, state: &AppState, composer_text: &str) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(composer_height(composer_text)),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_transcript(frame, chunks[0], state);
    frame.render_widget(Paragraph::new(state.top_status_line()), chunks[1]);
    frame.render_widget(Paragraph::new(format!("> {composer_text}")).wrap(Wrap { trim: false }), chunks[2]);
    frame.render_widget(Paragraph::new(state.bottom_status_line(chunks[3].width as usize)), chunks[3]);
    frame.render_widget(Paragraph::new("Enter submit  Ctrl+J newline  Ctrl+C cancel/quit").style(Style::default().fg(Color::DarkGray)), chunks[4]);

    if let Some(approval) = state.pending_approval() {
        let overlay = centered_rect(area.width.min(72), 7, area);
        frame.render_widget(Clear, overlay);
        let text = vec![
            Line::from(Span::styled(approval.title().to_string(), Style::default().add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(approval.body().to_string()),
            Line::from(""),
            Line::from("[a] allow once   [r] reject"),
        ];
        frame.render_widget(Paragraph::new(text).block(Block::default().borders(Borders::ALL)), overlay);
    }
}

/// Return the composer height clamped to the first-pass UI budget.
fn composer_height(text: &str) -> u16 {
    let lines = text.lines().count().max(1) as u16;
    lines.clamp(1, 6)
}
```

Continue `render.rs` with:

- `render_transcript(frame, area, state)` that turns transcript cells and tool calls into `Line`s.
- `centered_rect(width, height, area)` helper.
- Use dim style for `TranscriptCell::Reasoning`.
- Render tool calls after transcript for Phase 1.

- [ ] **Step 3: Run render tests**

Run:

```bash
cargo test -p tui render_
```

Expected: render smoke test passes.

---

### Task 7: Implement Bootstrap And CLI List Sessions

**Files:**
- Create/Modify: `crates/tui/src/bootstrap.rs`
- Modify: `crates/tui/src/main.rs`

- [ ] **Step 1: Implement bootstrap**

```rust
//! Kernel bootstrap for the local TUI.

use std::sync::Arc;

use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::ToolRegistry;

/// Build the kernel used by the local TUI session.
pub fn build_kernel() -> anyhow::Result<Arc<Kernel>> {
    let config = config::load()?;
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins();
    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();
    Ok(kernel)
}
```

- [ ] **Step 2: Replace `main.rs` with CLI**

```rust
//! Entry point for the clawcode TUI binary.

use std::path::PathBuf;

use clap::Parser;
use protocol::{AgentKernel, SessionId};

#[derive(Debug, Parser)]
#[command(name = "claw-tui", version, about = "Local terminal UI for clawcode")]
struct Cli {
    /// List persisted sessions for the current working directory and exit.
    #[arg(long, conflicts_with = "resume")]
    list_sessions: bool,

    /// Resume a persisted session id instead of creating a new session.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Disable alternate screen mode.
    #[arg(long)]
    no_alt_screen: bool,
}

/// Run the TUI binary.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let kernel = tui::bootstrap::build_kernel()?;
    let cwd = std::env::current_dir()?;

    if cli.list_sessions {
        print_sessions(kernel.as_ref(), cwd).await?;
        return Ok(());
    }

    tui::app::run(kernel, cwd, cli.resume.map(SessionId), !cli.no_alt_screen).await
}

/// Print sessions for the current working directory.
async fn print_sessions(kernel: &dyn AgentKernel, cwd: PathBuf) -> anyhow::Result<()> {
    let page = kernel.list_sessions(Some(&cwd), None).await?;
    if page.sessions.is_empty() {
        println!("No sessions found for this working directory.");
        return Ok(());
    }
    for session in page.sessions {
        let updated = session.updated_at.as_deref().unwrap_or("-");
        let title = session.title.as_deref().unwrap_or("");
        println!("{}\t{}\t{}\t{}", session.session_id, updated, session.cwd.display(), title);
    }
    Ok(())
}
```

- [ ] **Step 3: Run check**

Run:

```bash
cargo check -p tui
```

Expected: compiles until `tui::app::run` is missing if Task 8 has not been implemented yet. After Task 8, this command must pass.

---

### Task 8: Implement App Loop And Kernel Prompt Streaming

**Files:**
- Create/Modify: `crates/tui/src/app.rs`

- [ ] **Step 1: Add fake-kernel app tests**

Add focused tests for pure helper functions in `app.rs`, not the full terminal loop:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::Composer;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use protocol::ReviewDecision;

    #[tokio::test]
    async fn ctrl_c_rejects_pending_approval_before_exit() {
        let mut state = AppState::new(SessionId("s1".to_string()), "/tmp".into(), "deepseek/model".to_string());
        state.apply_event(Event::ExecApprovalRequested {
            session_id: SessionId("s1".to_string()),
            call_id: "call-1".to_string(),
            tool_name: "shell".to_string(),
            arguments: serde_json::json!({}),
            cwd: "/tmp".into(),
        });
        let mut composer = Composer::default();

        let action = classify_ctrl_c(&state, &composer);

        assert_eq!(action, CtrlCAction::RejectApproval(ReviewDecision::RejectOnce));
    }

    #[test]
    fn ctrl_c_clears_composer_before_exit() {
        let state = AppState::new(SessionId("s1".to_string()), "/tmp".into(), "deepseek/model".to_string());
        let mut composer = Composer::default();
        composer.insert_str("draft");

        assert_eq!(classify_ctrl_c(&state, &composer), CtrlCAction::ClearComposer);
    }
}
```

- [ ] **Step 2: Implement app control helpers**

```rust
//! TUI app loop and kernel integration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use kernel::Kernel;
use protocol::{AgentKernel, Event, KernelError, ReviewDecision, SessionId, StopReason};
use tokio::sync::mpsc;

use crate::approval;
use crate::composer::{Composer, ComposerAction};
use crate::event::{TuiEvent, map_crossterm_event};
use crate::state::AppState;

/// Action selected when Ctrl-C is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtrlCAction {
    RejectApproval(ReviewDecision),
    CancelPrompt,
    ClearComposer,
    Exit,
}

/// Classify Ctrl-C based on current app state.
pub fn classify_ctrl_c(state: &AppState, composer: &Composer) -> CtrlCAction {
    if state.pending_approval().is_some() {
        return CtrlCAction::RejectApproval(ReviewDecision::RejectOnce);
    }
    if state.running_prompt() {
        return CtrlCAction::CancelPrompt;
    }
    if !composer.is_empty() {
        return CtrlCAction::ClearComposer;
    }
    CtrlCAction::Exit
}
```

Add `AppEvent`:

```rust
/// Internal app event used by the main loop.
#[derive(Debug)]
enum AppEvent {
    Terminal(TuiEvent),
    Kernel(Event),
    PromptFailed(String),
}
```

- [ ] **Step 3: Implement `run()`**

```rust
/// Run the interactive TUI.
pub async fn run(
    kernel: Arc<Kernel>,
    cwd: PathBuf,
    resume: Option<SessionId>,
    use_alt_screen: bool,
) -> anyhow::Result<()> {
    let created = if let Some(session_id) = resume {
        kernel.load_session(&session_id).await?
    } else {
        kernel.new_session(cwd.clone()).await?
    };
    let model_label = created
        .models
        .first()
        .map(|model| model.id.clone())
        .unwrap_or_else(|| "model: unknown".to_string());
    let session_id = created.session_id.clone();
    let mut state = AppState::new(created.session_id, cwd, model_label);
    for message in created.history {
        state.append_history_message(message);
    }

    let (mut terminal, mut guard) = crate::terminal::TerminalGuard::enter(use_alt_screen)?;
    let result = run_loop(kernel, session_id, &mut state, &mut terminal).await;
    guard.restore()?;
    result
}
```

If `AppState::append_history_message` is not yet present, add it in `state.rs` and convert `protocol::message::Message` role/content into `TranscriptCell::User`, `TranscriptCell::Assistant`, or `TranscriptCell::System`.

- [ ] **Step 4: Implement event loop**

Implementation requirements for `run_loop`:

- Create `EventStream::new()`.
- Create `tokio::time::interval(Duration::from_millis(250))`.
- Create `mpsc::unbounded_channel::<AppEvent>()`.
- Render once before entering loop.
- On crossterm event, call `map_crossterm_event`.
- On paste, call `composer.insert_str(&text)`.
- On key:
  - If approval overlay is active, use `approval::decision_for_key`.
  - If Ctrl-C, use `classify_ctrl_c`.
  - Otherwise pass to `composer.handle_key`.
- On submit, call `submit_prompt`.
- On kernel event, call `state.apply_event`.
- Render after every handled event.
- On exit, call `kernel.close_session(&session_id).await`.

Required helper signature:

```rust
/// Submit a prompt and forward kernel events into the app event channel.
fn submit_prompt(
    kernel: Arc<Kernel>,
    session_id: SessionId,
    text: String,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match kernel.prompt(&session_id, text).await {
            Ok(mut stream) => {
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            let _ = tx.send(AppEvent::Kernel(event));
                        }
                        Err(KernelError::Cancelled) => {
                            let _ = tx.send(AppEvent::PromptFailed("cancelled".to_string()));
                            break;
                        }
                        Err(error) => {
                            let _ = tx.send(AppEvent::PromptFailed(error.to_string()));
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(AppEvent::PromptFailed(error.to_string()));
            }
        }
    });
}
```

- [ ] **Step 5: Run app tests and check**

Run:

```bash
cargo test -p tui ctrl_c
cargo check -p tui
```

Expected: app helper tests pass and the crate compiles.

---

### Task 9: Final Verification

**Files:**
- No new files unless previous tasks expose small compile/test fixes.

- [ ] **Step 1: Run focused TUI tests**

Run:

```bash
cargo test -p tui
```

Expected: all TUI tests pass.

- [ ] **Step 2: Run workspace check**

Run:

```bash
cargo check --workspace
```

Expected: workspace compiles.

- [ ] **Step 3: Run manual list sessions path**

Run:

```bash
cargo run -p tui -- --list-sessions
```

Expected: prints either session rows or `No sessions found for this working directory.` and exits without entering raw mode.

- [ ] **Step 4: Run manual TUI smoke test**

Run:

```bash
cargo run -p tui
```

Expected:

- TUI opens.
- Runtime status line below input shows `model`, `cwd`, and `tokens: -`.
- Typing text appears in composer.
- `Ctrl+J` inserts a newline.
- `Ctrl+C` exits cleanly when no prompt is running and composer is empty.
- Terminal echo and cursor state are restored after exit.

- [ ] **Step 5: Run pre-commit before any user-approved commit**

Only after the user explicitly asks to commit, run:

```bash
pre-commit run --all-files
```

Expected: hooks pass. If hooks modify files, re-run focused tests and `cargo check --workspace` before committing.

---

## Self-Review

**Spec coverage:**

- TUI direct Kernel path: Task 7 and Task 8.
- `ratatui + crossterm`: Task 1, Task 5, Task 6.
- Session lifecycle: Task 7 and Task 8.
- Prompt streaming and kernel events: Task 4 and Task 8.
- Approval overlay and `resolve_approval`: Task 3 and Task 8.
- Runtime status below input with model/cwd/tokens: Task 4 and Task 6.
- Terminal restore: Task 5 and Task 9.
- Tests and manual verification: Task 2 through Task 9.

**Placeholder scan:** no placeholder markers or unspecified “add tests” steps are present. Code-oriented steps include concrete snippets, commands, and expected outcomes.

**Type consistency:** plan consistently uses `Composer`, `ComposerAction`, `PendingApproval`, `AppState`, `ToolCallView`, `TuiEvent`, `CtrlCAction`, `protocol::Event`, `protocol::ReviewDecision`, and `kernel::Kernel`.

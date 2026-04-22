# CLI Live Streaming Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the CLI render assistant text incrementally in real time and show tool-call progress while a turn is running.

**Architecture:** Evolve the CLI event sink from a tracing-only logger into a stateful presentation sink that writes streamed text and concise tool-status lines to the terminal while preserving tracing logs. Keep one-shot and REPL modes on the same runtime path, but stop `main.rs` from printing the final assistant text a second time after the sink already streamed it.

**Tech Stack:** Rust, Tokio, existing `kernel::events::AgentEvent` stream, `cargo test`

---

## File Map

- Modify: `crates/cli/src/runtime.rs`
  Add terminal presentation state to the CLI event sink and stream `ModelTextDelta` / tool events directly to terminal output.
- Modify: `crates/cli/src/main.rs`
  Stop double-printing final assistant text after streaming already displayed it.
- Test: `cargo test -p cli`
- Test: `cargo test`

### Task 1: Turn the CLI Event Sink Into a Live Presentation Sink

**Files:**
- Modify: `crates/cli/src/runtime.rs`
- Test: `cargo test -p cli runtime::tests::streams_text_deltas_without_waiting_for_run_finished -- --exact`
- Test: `cargo test -p cli runtime::tests::prints_tool_status_lines_on_separate_lines -- --exact`

- [ ] **Step 1: Write the failing presentation-state expectation**

Document the desired sink behavior before editing:

```rust
let sink = CliPresentationEventSink::for_test(output.clone());
sink.publish(AgentEvent::ModelTextDelta { text: "hel".into(), iteration: Some(1) }).await;
sink.publish(AgentEvent::ModelTextDelta { text: "lo".into(), iteration: Some(1) }).await;
sink.publish(AgentEvent::RunFinished { text: "hello".into(), usage }).await;
assert_eq!(rendered_output, "hello\n");
```

Expected compile failure before implementation: the sink has no presentation state and no testable terminal output abstraction.

- [ ] **Step 2: Add a small terminal writer abstraction inside `crates/cli/src/runtime.rs`**

Introduce a minimal writer wrapper so the sink can target stdout in production and an in-memory buffer in tests:

```rust
type SharedCliWriter = Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>;

/// Builds the shared writer used for live CLI presentation.
fn stdout_writer() -> SharedCliWriter {
    Arc::new(std::sync::Mutex::new(Box::new(std::io::stdout())))
}
```

If a boxed `Write` object becomes awkward, use a small enum or a generic helper, but keep the abstraction local to `runtime.rs`.

- [ ] **Step 3: Replace `TracingEventSink` with a stateful sink that tracks line state**

Add state needed for real-time presentation:

```rust
#[derive(Default)]
struct CliPresentationState {
    text_line_open: bool,
}

pub(crate) struct TracingEventSink {
    writer: SharedCliWriter,
    state: std::sync::Mutex<CliPresentationState>,
}
```

Add constructors:

```rust
impl TracingEventSink {
    /// Builds the default sink that streams live output to stdout.
    pub(crate) fn stdout() -> Self { ... }

    /// Builds a sink for tests that captures rendered output in memory.
    #[cfg(test)]
    fn for_test(writer: SharedCliWriter) -> Self { ... }
}
```

- [ ] **Step 4: Stream `ModelTextDelta` directly to terminal output**

Inside `publish(...)`, keep the existing tracing call, but add user-visible output behavior:

```rust
AgentEvent::ModelTextDelta { text, iteration } => {
    trace!(iteration, text = %text, "model text delta");
    self.write_text_delta(&text);
}
```

Implement `write_text_delta(...)` so it:

- writes text directly without buffering to final completion
- marks `text_line_open = true`
- flushes the writer

- [ ] **Step 5: Print concise tool progress lines on separate lines**

Handle tool lifecycle events with readable status lines:

```rust
AgentEvent::ToolCallRequested { name, .. } => {
    self.write_status_line(format!("[tool] {name} started"));
}
AgentEvent::ToolCallCompleted { name, .. } => {
    self.write_status_line(format!("[tool] {name} completed"));
}
```

`write_status_line(...)` must:

- insert a newline first if streamed assistant text currently has an open line
- print the status line with `\n`
- reset `text_line_open` appropriately

Do not print full tool output payloads by default.

- [ ] **Step 6: Close any open streamed text line on `RunFinished`**

On `AgentEvent::RunFinished`, preserve tracing but only print a newline if a
streamed text line is still open:

```rust
AgentEvent::RunFinished { text, usage } => {
    self.finish_text_line_if_needed();
    info!(...);
}
```

This avoids prompt collisions in REPL mode and keeps one-shot output tidy.

- [ ] **Step 7: Add focused sink tests for streamed text and tool status formatting**

Add tests in `crates/cli/src/runtime.rs` that capture rendered output:

```rust
#[tokio::test]
async fn streams_text_deltas_without_waiting_for_run_finished() {
    let output = test_writer();
    let sink = TracingEventSink::for_test(output.clone());

    sink.publish(AgentEvent::ModelTextDelta { text: "hel".into(), iteration: Some(1) }).await;
    sink.publish(AgentEvent::ModelTextDelta { text: "lo".into(), iteration: Some(1) }).await;
    sink.publish(AgentEvent::RunFinished { text: "hello".into(), usage: Usage::default() }).await;

    assert_eq!(captured(output), "hello\n");
}

#[tokio::test]
async fn prints_tool_status_lines_on_separate_lines() {
    let output = test_writer();
    let sink = TracingEventSink::for_test(output.clone());

    sink.publish(AgentEvent::ModelTextDelta { text: "answer".into(), iteration: Some(1) }).await;
    sink.publish(AgentEvent::ToolCallRequested { name: "exec_command".into(), handle_id: "h1".into(), arguments: serde_json::json!({}) }).await;
    sink.publish(AgentEvent::ToolCallCompleted { name: "exec_command".into(), handle_id: "h1".into(), output: "ok".into(), structured_output: None }).await;

    assert_eq!(captured(output), "answer\n[tool] exec_command started\n[tool] exec_command completed\n");
}
```

- [ ] **Step 8: Run the focused CLI runtime tests**

Run:

```bash
cargo test -p cli runtime::tests::streams_text_deltas_without_waiting_for_run_finished -- --exact
cargo test -p cli runtime::tests::prints_tool_status_lines_on_separate_lines -- --exact
```

Expected: PASS

- [ ] **Step 9: Commit the live event sink changes**

```bash
git add crates/cli/src/runtime.rs
git commit -m "Stream CLI text and tool progress from runtime events"
```

### Task 2: Remove Final Text Double-Printing From CLI Entry Paths

**Files:**
- Modify: `crates/cli/src/main.rs`
- Test: `cargo test -p cli`

- [ ] **Step 1: Write the failing double-print expectation**

Document the intended behavior:

```rust
// After live streaming is enabled, main.rs should not do:
println!("{result}");
```

Expected issue before implementation: one-shot mode would print both streamed text and the final buffered text.

- [ ] **Step 2: Update one-shot path in `main.rs` to avoid printing the final text again**

Refactor the one-shot path so it still executes the turn and propagates errors,
but does not print the returned final text once the sink is responsible for user-visible rendering:

```rust
if prompt.trim().is_empty() {
    run_interactive_cli(...).await?;
} else {
    let _ = runtime::run_cli_prompt(Arc::clone(&model), store, router, prompt).await?;
}
```

Keep returning/using the final string internally where tests need it, but do not print it from `main.rs`.

- [ ] **Step 3: Preserve REPL behavior after streamed turns**

Ensure interactive mode still:

- shows streamed assistant text
- prints tool status lines inline
- leaves the next `> ` prompt on a fresh line

No additional prompt-print logic should be needed if `RunFinished` already closes the line correctly.

- [ ] **Step 4: Run the full CLI test suite**

Run: `cargo test -p cli`
Expected: PASS, including existing multi-turn tests and new streaming tests.

- [ ] **Step 5: Commit the CLI entrypoint cleanup**

```bash
git add crates/cli/src/main.rs crates/cli/src/runtime.rs
git commit -m "Avoid double-printing streamed CLI responses"
```

### Task 3: Verify Workspace Integration

**Files:**
- Modify: `crates/cli/src/runtime.rs`
- Modify: `crates/cli/src/main.rs`
- Test: `cargo test`

- [ ] **Step 1: Search for stale buffered-output assumptions**

Run:

```bash
rg -n "println!\\(\"\\{result\\}\"\\)|RunFinished \\{ text|tool completed|model text delta" crates/cli -g '!target'
```

Expected: no stale path that prints final assistant text after streaming already rendered it.

- [ ] **Step 2: Make any final cleanup edits found by the search**

Keep the final behavior aligned with the spec:

- assistant text streams incrementally
- tool calls show concise started/completed lines
- one-shot mode does not duplicate output
- REPL prompts remain on fresh lines

- [ ] **Step 3: Run full workspace verification**

Run: `cargo test`
Expected: PASS across `cli`, `kernel`, `llm`, and `tools`.

- [ ] **Step 4: Commit the final cleanup if needed**

```bash
git add crates/cli/src/runtime.rs crates/cli/src/main.rs
git commit -m "Finalize live streaming CLI output"
```

## Self-Review

- Spec coverage: the plan covers incremental text rendering, tool status lines, newline handling, main-path output suppression, and full verification.
- Placeholder scan: no `TODO`, `TBD`, or vague implementation steps remain.
- Type consistency: the plan consistently uses `TracingEventSink`, `CliPresentationState`, `run_cli_turn`, and `RunFinished` line-closing behavior.

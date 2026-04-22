# CLI Live Streaming Output Design

## Goal

Extend the CLI so it renders assistant output incrementally in real time and
shows tool invocation progress while a turn is running.

The CLI should stop behaving like a buffered request/response wrapper and
instead behave like a live interactive terminal client.

## Scope

This change includes:

- live incremental assistant text rendering from `ModelTextDelta`
- live tool status rendering from tool-related runtime events
- support in both one-shot and interactive multi-turn CLI modes
- shared presentation logic inside `crates/cli/src/runtime.rs`

This change does **not** include:

- a full-screen terminal UI
- progress spinners
- persistent transcript formatting
- streaming reasoning content by default
- extra CLI flags for enabling the feature

## Current Problem

The current CLI waits for `run_cli_turn(...)` / `run_cli_prompt(...)` to finish
and then prints the final text all at once.

Although the runtime already emits streaming events such as:

- `ModelTextDelta`
- `ToolCallRequested`
- `ToolCallCompleted`

the CLI only forwards them to tracing logs. That means users do not see live
progress unless they inspect `RUST_LOG`, which is not suitable as the default
interactive experience.

## Proposed UX

### Assistant text

Assistant text should print incrementally as deltas arrive.

Example:

```text
> explain this file
The file defines a thread-oriented runtime API...
```

The text should appear as it streams, not after completion.

### Tool calls

Tool calls should render as short status lines separate from assistant text.

Example:

```text
[tool] exec_command started
[tool] exec_command completed
```

The tool output should not be dumped in full by default. The goal is to show
progress, not replicate verbose debug logs.

### Turn boundaries

If a streamed assistant response printed text without a trailing newline, the
CLI should print one newline at the end of the turn so the next prompt or tool
status does not collide with the previous line.

## Architecture

The current `TracingEventSink` should be evolved into a CLI-presenting event
sink that does two jobs:

1. preserve tracing logs for debugging
2. stream user-visible output to stdout/stderr in real time

The sink should maintain small per-turn presentation state, such as:

- whether assistant text has started printing
- whether the current line is mid-text
- whether a tool status line needs a preceding newline

This keeps rendering concerns inside the event sink instead of scattering them
across `main.rs`.

## Proposed Code Changes

### `crates/cli/src/runtime.rs`

Replace the current tracing-only sink with a stateful sink that:

- prints `ModelTextDelta` directly to stdout
- prints concise tool progress lines for:
  - `ToolCallRequested`
  - `ToolCallCompleted`
- prints a newline on `RunFinished` if the current streamed text line is still
  open

The sink should still emit tracing events through `info!` / `trace!` so
existing debug workflows continue to work.

### `crates/cli/src/main.rs`

Adjust one-shot and interactive paths so they do not print the final assistant
text a second time after streaming already displayed it.

This likely means:

- `run_cli_turn(...)` still returns final text for tests and callers
- `main.rs` suppresses the old final `println!("{result}")` path when live
  streaming is active

## Error Handling

If a tool status line or streamed text has already been printed, any subsequent
error should appear on a fresh line.

Interactive mode should continue after per-turn errors as it already does.

One-shot mode should still exit with an error status on turn failure.

## Testing

The change is complete when:

- CLI tests verify streamed text and tool status formatting at the sink/helper
  level
- one-shot mode no longer double-prints final assistant text
- interactive mode still supports multi-turn reuse
- `cargo test -p cli` passes
- `cargo test` passes

## Non-Goals

This design intentionally does not add:

- reasoning-stream display by default
- colorful styling
- cursor movement / line rewriting
- progress bars
- command-line switches to toggle streaming behavior

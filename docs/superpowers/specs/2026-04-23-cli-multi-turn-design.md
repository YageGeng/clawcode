# CLI Multi-Turn Conversation Design

## Goal

Extend the `cli` crate so it can support multi-turn conversations in a single
interactive terminal session.

The CLI should keep the current one-shot prompt mode, while adding a REPL-style
interactive mode that reuses the same thread/session state across multiple user
inputs.

## Scope

This change includes:

- interactive multi-turn mode inside `crates/cli`
- reuse of one `ThreadHandle`
- reuse of one `SessionTaskContext`
- reuse of one `ThreadRuntime`
- preservation of the existing one-shot prompt flow

This change does **not** include:

- persistent conversations across process restarts
- thread/session state saved to disk
- slash commands beyond basic exit commands
- multiple simultaneous threads
- streaming terminal UI redesign

## Current Problem

The current CLI only supports one-shot execution:

1. parse argv into one prompt
2. construct model/router/store
3. run one prompt
4. print one result
5. exit

That means the in-memory thread state is discarded immediately, so the CLI
cannot behave like a conversational interface even though the kernel runtime now
supports thread-oriented execution.

## Proposed UX

The CLI will support two modes:

### One-shot mode

If positional prompt text is provided, behavior stays unchanged:

- run one turn
- print the result
- exit

### Interactive mode

If no positional prompt text is provided, the CLI enters an input loop:

- print a simple prompt such as `> `
- read one line from stdin
- submit it as one thread turn
- print the assistant response
- continue until the user exits

Supported exit commands:

- `exit`
- `quit`

Empty input lines should be ignored rather than sent to the model.

## Architecture

Interactive mode should construct the runtime dependencies once at startup:

- `CliAgentModel`
- `InMemorySessionStore`
- `ToolRouter`
- `ThreadHandle`
- `ThreadRuntime`

Each user line is then submitted through the same `ThreadHandle`, so message
history accumulates in the same in-memory session.

The existing `run_cli_prompt(...)` helper should be generalized so it can submit
to a caller-provided thread/runtime pair instead of always creating a fresh
thread internally.

## Proposed Code Changes

### `crates/cli/src/runtime.rs`

Add helpers that separate thread construction from turn submission.

Expected additions:

- a helper to build the default CLI `ThreadHandle`
- a helper to submit one turn through an existing runtime/thread pair

This keeps one-shot mode and interactive mode sharing the same execution path.

### `crates/cli/src/main.rs`

Split startup into two paths:

- one-shot path for existing argv prompt usage
- interactive loop path when argv prompt is empty

The interactive loop should:

1. print the input prompt
2. flush stdout
3. read a line
4. trim it
5. handle `exit` / `quit`
6. skip empty lines
7. run one thread turn
8. print the assistant text

## Error Handling

Per-turn runtime errors in interactive mode should be printed and the loop
should continue, so one failed turn does not kill the whole session.

Startup errors should still terminate the process, because the CLI cannot run at
all without valid config, model creation, or tool/router setup.

## Testing

The change is complete when:

- one-shot CLI tests still pass
- a new unit test covers that interactive mode reuses one thread/store across
  multiple turns, or equivalent runtime-level helper behavior is tested
- full `cargo test -p cli` passes
- full workspace `cargo test` passes

## Non-Goals

This design intentionally does not define:

- saved chat transcripts
- resumable sessions
- command history
- readline editing
- colored output
- streaming partial assistant output

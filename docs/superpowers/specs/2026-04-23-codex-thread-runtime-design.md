# Codex-Style Thread Runtime Design

## Goal

Replace `clawcode`'s current `Agent`-centric public runtime API with a
Codex-style `Thread` concept. After this change, execution should be
thread/session-driven, while the thread handle acts only as an identity and
configuration carrier instead of a long-lived executor object.

This migration intentionally does **not** implement subagents, agent control,
interrupts, or collaborative thread management. The scope is limited to
replacing the current single-agent public model with a thread-oriented one.

## Current Problem

`clawcode` currently exposes:

- `Agent`
- `AgentRunner`
- `AgentDeps`
- `AgentRunRequest`

That API implies that the main execution primitive is an agent object that owns
stable runtime dependencies and directly executes turns. This no longer matches
the internal architecture after the context migration:

- identity lives on `session_id` / `thread_id`
- runtime context lives on `TurnContext`
- session state lives on `SessionTaskContext`

The current public API therefore preserves an outdated abstraction boundary.

## Design Principles

The replacement API should follow these rules:

1. `Thread` is the top-level public identity concept.
2. Execution is performed by a runtime/service object, not by the thread handle
   itself.
3. Thread handles stay lightweight and serializable in intent.
4. `TurnContext` remains the internal runtime context for one execution turn.
5. The migration should preserve the existing single-thread execution
   capability without introducing protocol-level `Op` handling yet.

## Proposed API

### `ThreadHandle`

Add a public thread handle type that represents one session/thread binding plus
thread-level defaults.

Expected responsibilities:

- own `session_id`
- own `thread_id`
- hold optional default `system_prompt`
- support builder-style configuration updates
- expose lightweight accessors for identity fields

Expected non-responsibilities:

- does not own `model`
- does not own `SessionTaskContext`
- does not own `ToolRouter`
- does not execute turns directly
- does not manage background lifecycle

### `ThreadRuntime`

Add a public runtime type that owns the execution dependencies needed to run a
thread turn.

Responsibilities:

- own `model`
- own `SessionTaskContext`
- own `ToolRouter`
- own `EventSink`
- own default loop configuration
- execute one request against a supplied `ThreadHandle`

Primary methods:

- `ThreadRuntime::new(...)`
- `ThreadRuntime::with_config(...)`
- `ThreadRuntime::run(&ThreadHandle, ThreadRunRequest)`
- `ThreadRuntime::run_outcome(&ThreadHandle, ThreadRunRequest)`

### `ThreadRunRequest`

Rename and re-scope the current per-turn input type so it clearly describes a
single submission to a thread.

Fields:

- `input`
- `system_prompt_override`

This remains intentionally minimal. We are not introducing protocol-level
operations in this migration.

## Internal Mapping

The new public API should map to existing internals as follows:

- `ThreadHandle.session_id` -> `RunRequest.session_id`
- `ThreadHandle.thread_id` -> `RunRequest.thread_id`
- thread default prompt + request override -> existing `system_prompt` path
- `ThreadRuntime` -> current dependency bundle + `run_task(...)`

`TurnContext` stays internal to runtime plumbing. The public thread API should
not require callers to construct `TurnContext` directly for normal use.

## Deletions

The following public runtime concepts should be removed:

- `Agent`
- `AgentRunner`
- `AgentConfig`
- `AgentDeps`
- `AgentRunRequest`

The code may keep small internal helpers during migration, but the final public
surface should stop exporting agent-oriented names.

## CLI Impact

`crates/cli` should stop constructing an `Agent`. It should instead:

1. create a `ThreadHandle`
2. create a `ThreadRuntime`
3. submit one `ThreadRunRequest`

This keeps CLI aligned with the new kernel entrypoint.

## Compatibility Strategy

This is a breaking API change. We will not keep a long-lived deprecated public
alias layer because the user explicitly wants the old `Agent` definition
removed.

Internally, the migration may temporarily reuse existing runner logic while the
names are being replaced, but callers should observe only thread-oriented API
names at the end.

## Non-Goals

This design does not include:

- subagent spawning
- thread status subscriptions
- `close` / `interrupt` operations
- codex-style `Op` protocol submission
- thread manager or agent control services
- cross-thread communication

Those belong to a later collaboration/runtime protocol layer.

## Testing

The migration is complete when:

- kernel unit/integration tests compile under thread-oriented public API
- CLI tests compile under thread-oriented public API
- full workspace `cargo test` passes
- no public `Agent*` runtime types remain exported from `kernel`

## Implementation Outline

1. Introduce `ThreadHandle`, `ThreadRuntime`, and `ThreadRunRequest`.
2. Repoint runtime task API from `Agent*` names to `Thread*` names.
3. Update kernel exports.
4. Update CLI entrypoints and tests.
5. Update kernel tests to use `ThreadRuntime`.
6. Remove public `Agent*` types and any obsolete compatibility aliases.

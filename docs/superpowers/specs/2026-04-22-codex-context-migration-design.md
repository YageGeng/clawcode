# Codex Context Migration Design

## Goal

Replace the current `clawcode` runtime/session context model with a Codex-style context architecture in one pass.

The new architecture must support:

- prompt assembly
- continuation across turns
- resume/recovery from persisted state
- parent/child agent context inheritance

This migration is intentionally not a compatibility layer. The existing `clawcode` session/runtime context model may be removed when the new architecture is in place.

## Scope

This design covers the kernel runtime in `crates/kernel/src`.

It includes:

- introducing `TurnContext`
- introducing `SessionTaskContext`
- introducing `ContextManager`
- introducing `TurnContextItem`
- replacing `SessionStore`-centric runtime flows
- replacing `AgentContext` with turn/session context objects
- adding baseline context snapshot persistence and settings-diff generation
- upgrading child-agent forking to inherit stable runtime context

It does not include:

- UI changes
- protocol changes outside kernel unless required by the new persistence format
- unrelated tool execution refactors

## Current State

The current runtime is centered around these simplified abstractions:

- `AgentContext`
  Carries agent identity plus `session_id` and `thread_id`.
- `SessionStore`
  Owns active-turn lifecycle, completed turns, and continuation queue access.
- `ThreadState`
  Stores `turns`, `active_turn`, and `continuations`.
- `Turn`
  Stores finalized transcript and usage.

This is sufficient for a basic turn/task loop, but it does not model:

- a first-class turn runtime context
- a separate session task execution context
- a persisted context baseline for prompt diffing
- a history manager that owns prompt assembly semantics
- parent/child agent inheritance beyond identity

As a result, prompt composition, turn lifecycle, persistence, and continuation state are too tightly coupled to `SessionStore`, while context evolution across turns is under-modeled.

## Desired End State

The runtime will be reorganized around four first-class context concepts.

### `TurnContext`

`TurnContext` is the complete runtime context for one turn.

It represents:

- agent identity
- session/thread scope
- prompt configuration
- model configuration
- tool configuration
- environment metadata
- policy metadata
- turn-scoped execution metadata

At minimum it should contain:

- `agent_id`
- `parent_agent_id`
- `name`
- `session_id`
- `thread_id`
- `system_prompt`
- `cwd`
- `current_date`
- `timezone`
- `model_settings`
- `tool_settings`
- `continuation_settings`
- `turn_metadata`

`TurnContext` replaces the role currently played by `AgentContext` and part of the implicit runtime configuration spread across the current execution flow.

### `SessionTaskContext`

`SessionTaskContext` is the stable execution context for a session-bound runtime task.

It owns or exposes:

- thread-level context/history state
- active turn lifecycle APIs
- continuation queue APIs
- baseline snapshot access
- persistence hooks
- stable runtime dependencies needed across turns

It replaces the runtime role currently played by `SessionStore`.

### `ContextManager`

`ContextManager` becomes the canonical owner of thread history and prompt-visible context state.

Its responsibilities are:

- maintain transcript history
- maintain active turn state
- generate prompt-ready message history
- maintain `reference_context_item`
- decide whether to inject full initial context or only settings diffs
- append/replace/discard turn state
- provide state needed for resume/reconstruction

It replaces the current `ThreadState` plus the history-related parts of `SessionStore`.

### `TurnContextItem`

`TurnContextItem` is the persisted snapshot form of the stable turn context.

It is used for:

- creating the baseline for future settings diffs
- reconstructing runtime context during resume
- preserving the latest stable turn context across compaction/recovery
- carrying inherited stable context into child-agent forks

`TurnContextItem` is not the runtime object itself. It is the durable, comparable snapshot.

## Architectural Decisions

### Decision 1: One-pass replacement instead of compatibility layering

The migration will replace the current context model directly instead of maintaining dual paths.

Reasoning:

- the current `SessionStore` abstraction does not map cleanly to Codex-style context ownership
- keeping both designs alive would multiply transition complexity
- the user explicitly wants the old `clawcode` context stack removed

### Decision 2: Preserve external entry points where they still provide value

The crate may retain external entry points such as `Agent`, `RunRequest`, `RunResult`, and `RunOutcome`, but they become thin façades over the new context stack.

Reasoning:

- this keeps the public surface smaller and more stable
- it avoids unnecessary churn where the old naming is not itself the problem
- it still allows the internal architecture to be fully replaced

### Decision 3: Keep continuation logic outside `ContextManager`

`ContextManager` owns history and prompt context, but continuation decision logic remains in `runtime/continuation`.

Reasoning:

- continuation is control flow, not prompt-state ownership
- mixing these concerns would make `ContextManager` too broad

### Decision 4: Persist one durable context snapshot per completed real turn

Each completed real turn persists a `TurnContextItem` even when that turn emits no visible context diff items.

Reasoning:

- resume must recover the latest stable baseline
- later turns need a precise baseline even when the previous turn made no visible context updates

## Module Layout

The kernel should move toward this structure:

- `agent/`
  External API and agent fork entry points.
- `context/turn.rs`
  `TurnContext` and builders.
- `context/session.rs`
  `SessionTaskContext`.
- `context/history.rs`
  `ContextManager`.
- `context/item.rs`
  `TurnContextItem`.
- `runtime/task/`
  Task-level orchestration.
- `runtime/turn/`
  One-turn execution loop.
- `runtime/continuation/`
  Continuation decision logic.
- `events/`
  Runtime event publication.
- `model/`
  Model interaction layer.
- `tools/`
  Tool routing and execution.

## Runtime Flow After Migration

### Standard turn flow

1. `Agent::run(...)` creates a `TurnContext` for the requested input.
2. `SessionTaskContext` queries `ContextManager.reference_context_item`.
3. If no baseline exists, the runtime builds and injects full initial context.
4. If a baseline exists, the runtime emits only settings/context diff items.
5. `ContextManager` produces prompt-ready history plus current user input.
6. `run_turn` executes the model/tool loop.
7. On completion:
   - transcript updates are committed to `ContextManager`
   - a new `TurnContextItem` is persisted
   - `reference_context_item` is updated
   - continuation requests are recorded in `SessionTaskContext`
8. `run_task` decides whether another turn should execute.

### Resume flow

Resume reconstructs:

- transcript/history
- active stable baseline via `TurnContextItem`
- continuation queue state when applicable

If the baseline is missing or invalid after reconstruction, the next real turn falls back to full initial context injection.

### Child-agent flow

Child agents are created by forking from a parent `TurnContext`.

The child inherits stable context:

- session/thread scope
- system prompt
- model settings
- tool settings
- environment metadata such as `cwd`, `current_date`, and `timezone`
- context baseline where valid

The child may override:

- `name`
- `system_prompt`
- selected model settings
- selected tool policy

## Old-to-New Mapping

### To delete

- `AgentContext`
- `SessionStore`
- `InMemorySessionStore`
- `ThreadState`

### To retain with changed internals

- `Agent`
- `AgentRunner`
- `RunRequest`
- `RunResult`
- `RunOutcome`
- `AgentLoopConfig`
- `ToolCallRuntimeSnapshot`
- `SessionId`
- `ThreadId`

### To redefine or demote

- `Turn`
  It may remain as a finalized transcript record, but it is no longer the durable representation of runtime context.

## Detailed Replacement Plan

### Phase 1: Introduce the new context types

Add:

- `TurnContext`
- `SessionTaskContext`
- `ContextManager`
- `TurnContextItem`

This phase defines the canonical data model and field ownership boundaries.

### Phase 2: Replace prompt/history ownership

Move these responsibilities from `SessionStore` into `ContextManager` and `SessionTaskContext`:

- begin active turn
- append active-turn messages
- finalize active turn
- discard active turn
- load model-visible history
- own continuation queue storage

`run_persisted_turn` will no longer load transcript history through `SessionStore`.

### Phase 3: Rewire runtime execution APIs

Update runtime entry points:

- `run_task` takes `SessionTaskContext` and initial `TurnContext`
- `run_persisted_turn` takes `SessionTaskContext` and `TurnContext`
- `run_turn` consumes prompt-ready working messages plus `TurnContext`

At this point, `AgentContext` is removed.

### Phase 4: Add baseline snapshot persistence and diffing

Introduce:

- `reference_context_item`
- full initial context injection
- settings diff generation against baseline
- baseline restoration during resume

This is the step that makes the migration truly Codex-style rather than a rename.

### Phase 5: Upgrade child-agent inheritance

Replace identity-only forking with context-aware forking:

- `TurnContext::fork_child(...)`

This becomes the canonical mechanism for subagent creation.

## Validation Requirements

The implementation is complete only when all of the following are true:

- one normal turn can run with full initial context injection
- a second turn on the same thread emits settings diff behavior instead of always full reinjection
- continuation-driven multi-turn tasks still work
- failed turns discard only active-turn state and preserve the last valid baseline
- resumed sessions reconstruct the latest valid `TurnContextItem`
- child agents inherit stable runtime context from their parent

## Risks

### Risk 1: Runtime breakage from removing `SessionStore`

Current runtime logic assumes `SessionStore` owns several atomic operations.

Mitigation:

- migrate responsibility in clear phases
- keep behavior-focused tests around begin/append/finalize/discard semantics

### Risk 2: `TurnContext` becoming an unbounded bag of fields

Mitigation:

- keep transcript/history out of `TurnContext`
- keep only stable turn execution configuration and metadata there

### Risk 3: `ContextManager` taking on control-flow responsibilities

Mitigation:

- continuation remains in `runtime/continuation`
- `ContextManager` owns only history/prompt/baseline state

### Risk 4: Resume semantics becoming ambiguous

Mitigation:

- persist one `TurnContextItem` per completed real turn
- define explicit fallback to full context reinjection when baseline is missing or invalid

## Non-Goals

- recreating all Codex features immediately if they are unrelated to context ownership
- changing tool execution semantics unless required by the new context flow
- changing the event model except where context lifecycle needs additional visibility

## Recommendation

Implement the migration as a one-pass internal replacement while preserving high-level public runtime entry points as thin façades.

This yields:

- a clean Codex-style context architecture
- support for prompt assembly, recovery, and multi-agent inheritance
- lower long-term maintenance cost than keeping the current `SessionStore`-centric design alive

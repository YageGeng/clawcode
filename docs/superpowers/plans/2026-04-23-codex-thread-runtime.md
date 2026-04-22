# Codex-Style Thread Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the public `Agent` runtime API with a Codex-style thread-oriented API built around `ThreadHandle`, `ThreadRuntime`, and `ThreadRunRequest`.

**Architecture:** Keep the existing `run_task(...)` and context stack intact, but move the public runtime boundary from agent-owned execution to thread/session-owned execution. `ThreadHandle` carries stable identity and thread defaults, while `ThreadRuntime` owns dependencies and executes turns against a supplied thread handle.

**Tech Stack:** Rust, Tokio, existing `kernel` runtime modules, `cargo test`

---

## File Map

- Modify: `crates/kernel/src/runtime/task/api.rs`
  Replace `Agent*` public types with `Thread*` types while keeping the existing `run_task(...)` plumbing.
- Modify: `crates/kernel/src/runtime/task/mod.rs`
  Re-export the new `Thread*` API instead of `Agent*`.
- Modify: `crates/kernel/src/runtime/mod.rs`
  Re-export the new thread runtime names from the top-level runtime module.
- Modify: `crates/kernel/src/lib.rs`
  Re-export `ThreadHandle`, `ThreadRuntime`, and `ThreadRunRequest` from the crate root.
- Modify: `crates/cli/src/runtime.rs`
  Stop constructing `Agent`; build `ThreadHandle` + `ThreadRuntime` instead.
- Modify: `crates/kernel/tests/agent_loop.rs`
  Rename imports and update runtime construction to use `ThreadRuntime`.
- Modify: `crates/cli/src/main.rs`
  Keep CLI wiring aligned with the new kernel exports if imports or helper names change.
- Test: `cargo test -p kernel`
- Test: `cargo test -p cli`
- Test: `cargo test`

### Task 1: Replace Public Agent Runtime Types

**Files:**
- Modify: `crates/kernel/src/runtime/task/api.rs`
- Modify: `crates/kernel/src/runtime/task/mod.rs`
- Modify: `crates/kernel/src/runtime/mod.rs`
- Modify: `crates/kernel/src/lib.rs`
- Test: `cargo test -p kernel --no-run`

- [ ] **Step 1: Write the failing compile expectation**

Document the expected public API shape before editing:

```rust
use kernel::{
    ThreadHandle,
    ThreadRuntime,
    ThreadRunRequest,
};
```

Expected compile failure before implementation: unresolved imports for the new `Thread*` names.

- [ ] **Step 2: Run compile check to verify the old public surface is still agent-oriented**

Run: `cargo test -p kernel --no-run`
Expected: PASS before edits, confirming the baseline is stable before the rename.

- [ ] **Step 3: Rewrite the runtime task API to expose thread-oriented types**

Replace the public surface in `crates/kernel/src/runtime/task/api.rs` with thread-oriented names while preserving the current logic flow:

```rust
/// Shared dependencies that stay stable for one thread runtime lifecycle.
pub struct ThreadRuntimeDeps<M, E> {
    pub model: Arc<M>,
    pub store: Arc<SessionTaskContext>,
    pub tools: Arc<ToolRouter>,
    pub events: Arc<E>,
}

/// Thread-level defaults applied to submissions routed through one handle.
#[derive(Debug, Clone, Default)]
pub struct ThreadConfig {
    pub system_prompt: Option<String>,
}

/// Lightweight handle that identifies one session/thread binding.
#[derive(Debug, Clone)]
pub struct ThreadHandle {
    session_id: SessionId,
    thread_id: ThreadId,
    config: ThreadConfig,
}

/// One submission routed through a thread handle.
#[derive(Debug, Clone)]
pub struct ThreadRunRequest {
    pub input: String,
    pub system_prompt_override: Option<String>,
}

/// Runtime entrypoint that executes work against a supplied thread handle.
pub struct ThreadRuntime<M, E> {
    deps: ThreadRuntimeDeps<M, E>,
    config: AgentLoopConfig,
}
```

Keep `RunRequest`, `RunResult`, `RunOutcome`, and `RunFailure` unchanged unless a thread-oriented rename is strictly required for consistency in downstream tests.

- [ ] **Step 4: Wire `ThreadRuntime::run(...)` and `run_outcome(...)` to existing task execution**

Use the existing `run_task(...)` path instead of introducing new execution logic:

```rust
pub async fn run(&self, thread: &ThreadHandle, request: ThreadRunRequest) -> Result<RunResult> {
    match self.run_outcome(thread, request).await? {
        RunOutcome::Success(result) => Ok(result),
        RunOutcome::Failure(failure) => Err(failure.error),
    }
}

pub async fn run_outcome(
    &self,
    thread: &ThreadHandle,
    request: ThreadRunRequest,
) -> Result<RunOutcome> {
    let run_request = RunRequest::new(
        thread.session_id().clone(),
        thread.thread_id().clone(),
        request.input,
    );
    let system_prompt = request
        .system_prompt_override
        .or_else(|| thread.system_prompt().cloned());

    run_task(
        self.deps.model.as_ref(),
        self.deps.store.as_ref(),
        self.deps.tools.as_ref(),
        self.deps.events.as_ref(),
        &self.config,
        system_prompt,
        run_request,
    )
    .await
}
```

- [ ] **Step 5: Update exports to remove public `Agent*` runtime names**

Adjust re-exports so the crate surface matches the new thread runtime:

```rust
pub use task::{
    RunFailure, RunOutcome, RunRequest, RunResult, ThreadConfig, ThreadHandle,
    ThreadRunRequest, ThreadRuntime, ThreadRuntimeDeps,
};
```

Also update `crates/kernel/src/lib.rs` accordingly:

```rust
pub use runtime::{
    AgentLoopConfig, RunFailure, RunOutcome, RunRequest, RunResult, ThreadConfig,
    ThreadHandle, ThreadRunRequest, ThreadRuntime, ThreadRuntimeDeps,
};
```

- [ ] **Step 6: Run compile check to verify the new public API builds**

Run: `cargo test -p kernel --no-run`
Expected: PASS with no unresolved `Agent*` exports from `kernel::runtime` or `kernel`.

- [ ] **Step 7: Commit the public runtime rename**

```bash
git add crates/kernel/src/runtime/task/api.rs crates/kernel/src/runtime/task/mod.rs crates/kernel/src/runtime/mod.rs crates/kernel/src/lib.rs
git commit -m "Replace public agent runtime API with thread API"
```

### Task 2: Migrate CLI to Thread-Oriented Entry Points

**Files:**
- Modify: `crates/cli/src/runtime.rs`
- Modify: `crates/cli/src/main.rs`
- Test: `cargo test -p cli`

- [ ] **Step 1: Write the failing call-site expectation**

Document the desired CLI construction pattern:

```rust
let thread = ThreadHandle::new(SessionId::new(), ThreadId::new())
    .with_system_prompt("...");
let runtime = ThreadRuntime::new(model, store, router, Arc::new(TracingEventSink));
let result = runtime.run(&thread, ThreadRunRequest::new(prompt)).await?;
```

Expected compile failure before edits: `Agent`, `AgentDeps`, or `AgentRunRequest` no longer resolve after Task 1.

- [ ] **Step 2: Update CLI runtime wiring**

Change `run_cli_prompt(...)` to build and use the new thread-oriented API:

```rust
pub async fn run_cli_prompt<M>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
{
    let thread = ThreadHandle::new(SessionId::new(), ThreadId::new()).with_system_prompt(
        "You are a helpful agent. Use `apply_patch` for file edits and use `exec_command` or `write_stdin` only when command execution is required. Keep file changes inside the workspace, avoid paths containing `..`, and answer directly only when no tool action is needed.",
    );
    let runtime = ThreadRuntime::new(model, store, router, Arc::new(TracingEventSink));
    let result = runtime.run(&thread, ThreadRunRequest::new(prompt)).await?;
    Ok(result.text)
}
```

Retain the existing function comment and event sink behavior.

- [ ] **Step 3: Update imports and tests in CLI**

Swap `Agent` imports for thread-oriented names:

```rust
use kernel::{
    Result, SessionTaskContext, ThreadHandle, ThreadRunRequest, ThreadRuntime,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    model::AgentModel,
    session::{SessionId, ThreadId},
    tools::router::ToolRouter,
};
```

Keep the stub model test, but route it through `ThreadRuntime`.

- [ ] **Step 4: Run CLI tests**

Run: `cargo test -p cli`
Expected: PASS, including the runtime smoke test in `crates/cli/src/runtime.rs`.

- [ ] **Step 5: Commit the CLI migration**

```bash
git add crates/cli/src/runtime.rs crates/cli/src/main.rs
git commit -m "Switch CLI to thread runtime entrypoints"
```

### Task 3: Migrate Kernel Tests and Remove Remaining Agent-Oriented Runtime Usage

**Files:**
- Modify: `crates/kernel/tests/agent_loop.rs`
- Test: `cargo test -p kernel`

- [ ] **Step 1: Update kernel test imports to the new public API**

Replace agent-oriented imports with thread-oriented names:

```rust
use kernel::{
    ThreadHandle, ThreadRunRequest, ThreadRuntime,
    runtime::{AgentLoopConfig, RunOutcome},
};
```

Keep existing model fakes, store setup, and assertions intact.

- [ ] **Step 2: Replace `AgentRunner::new(...)` call sites with `ThreadRuntime::new(...)`**

Translate existing test setup one-for-one:

```rust
let runtime = ThreadRuntime::new(model, store, router, sink).with_config(AgentLoopConfig {
    max_iterations: Some(4),
    ..Default::default()
});
let thread = ThreadHandle::new(session_id.clone(), thread_id.clone());
let result = runtime
    .run(&thread, ThreadRunRequest::new("say hello"))
    .await
    .unwrap();
```

When a test already constructs `RunRequest` directly, keep that path only if it
is exercising internal runtime request behavior. Otherwise prefer the public
thread handle API.

- [ ] **Step 3: Replace any direct `Agent` construction in tests**

If any tests still instantiate the deleted `Agent` type, convert them to the
same pattern used in Task 2:

```rust
let thread = ThreadHandle::new(SessionId::new(), ThreadId::new())
    .with_system_prompt("test system prompt");
let runtime = ThreadRuntime::new(model, store, router, sink);
let outcome = runtime
    .run_outcome(&thread, ThreadRunRequest::new("input"))
    .await
    .unwrap();
```

- [ ] **Step 4: Run kernel tests**

Run: `cargo test -p kernel`
Expected: PASS, including `agent_loop.rs`, context tests, and session tests.

- [ ] **Step 5: Commit the test migration**

```bash
git add crates/kernel/tests/agent_loop.rs
git commit -m "Migrate kernel runtime tests to thread API"
```

### Task 4: Remove Residual Public Agent Runtime Names and Verify the Workspace

**Files:**
- Modify: `crates/kernel/src/runtime/task/api.rs`
- Modify: `crates/kernel/src/runtime/task/mod.rs`
- Modify: `crates/kernel/src/runtime/mod.rs`
- Modify: `crates/kernel/src/lib.rs`
- Modify: `crates/cli/src/runtime.rs`
- Test: `cargo test`

- [ ] **Step 1: Search for remaining public `Agent*` runtime symbols**

Run:

```bash
rg -n "AgentRunner|AgentRunRequest|AgentDeps|pub use runtime::\\{|pub use task::\\{" crates/kernel crates/cli -g '!target'
```

Expected: only model/event names such as `AgentModel` or `AgentEvent` remain; no public runtime executor types named `Agent*` should survive.

- [ ] **Step 2: Remove residual compatibility code if the search finds any**

The final public API should look like this in `crates/kernel/src/runtime/mod.rs`:

```rust
pub use task::{
    RunFailure, RunOutcome, RunRequest, RunResult, ThreadConfig, ThreadHandle,
    ThreadRunRequest, ThreadRuntime, ThreadRuntimeDeps,
};
```

And like this in `crates/kernel/src/lib.rs`:

```rust
pub use runtime::{
    AgentLoopConfig, RunFailure, RunOutcome, RunRequest, RunResult, ThreadConfig,
    ThreadHandle, ThreadRunRequest, ThreadRuntime, ThreadRuntimeDeps,
};
```

- [ ] **Step 3: Run full workspace verification**

Run: `cargo test`
Expected: PASS across `kernel`, `cli`, `llm`, and `tools`.

- [ ] **Step 4: Commit the cleanup and verification**

```bash
git add crates/kernel/src/runtime/task/api.rs crates/kernel/src/runtime/task/mod.rs crates/kernel/src/runtime/mod.rs crates/kernel/src/lib.rs crates/cli/src/runtime.rs
git commit -m "Remove residual agent-oriented runtime exports"
```

## Self-Review

- Spec coverage: the plan covers thread handle introduction, thread runtime introduction, CLI migration, test migration, and removal of public `Agent*` runtime names.
- Placeholder scan: no `TODO`, `TBD`, or “implement later” placeholders remain.
- Type consistency: the plan consistently uses `ThreadHandle`, `ThreadRuntime`, `ThreadRunRequest`, `ThreadConfig`, and `ThreadRuntimeDeps`.

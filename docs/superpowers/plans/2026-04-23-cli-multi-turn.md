# CLI Multi-Turn Conversation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add REPL-style multi-turn conversations to the CLI while preserving the existing one-shot prompt mode.

**Architecture:** Reuse one `ThreadHandle`, one `SessionTaskContext`, and one `ThreadRuntime` for the lifetime of an interactive CLI session. Refactor CLI runtime helpers so one-shot mode and interactive mode both submit turns through the same thread-oriented helper path.

**Tech Stack:** Rust, Tokio, existing `kernel` thread runtime API, `cargo test`

---

## File Map

- Modify: `crates/cli/src/runtime.rs`
  Add reusable thread construction and single-turn submission helpers that work for both one-shot and interactive CLI flows.
- Modify: `crates/cli/src/main.rs`
  Add interactive REPL flow and keep the existing one-shot path.
- Test: `cargo test -p cli`
- Test: `cargo test`

### Task 1: Refactor CLI Runtime Helpers Around Reusable Thread Submission

**Files:**
- Modify: `crates/cli/src/runtime.rs`
- Test: `cargo test -p cli runtime::tests::runs_prompt_through_kernel_agent_runtime -- --exact`

- [ ] **Step 1: Write the failing API expectation**

Document the helper split expected after this task:

```rust
let thread = runtime::build_cli_thread_handle();
let runtime = ThreadRuntime::new(model, store, router, Arc::new(TracingEventSink));
let text = runtime::run_cli_turn(&runtime, &thread, "hello".to_string()).await?;
```

Expected compile failure before edits: `build_cli_thread_handle` and `run_cli_turn` do not exist.

- [ ] **Step 2: Add a helper that builds the default CLI thread handle**

Implement a reusable helper in `crates/cli/src/runtime.rs`:

```rust
/// Builds the default CLI thread handle so one-shot and REPL modes share the same defaults.
pub fn build_cli_thread_handle() -> ThreadHandle {
    ThreadHandle::new(SessionId::new(), ThreadId::new()).with_system_prompt(
        "You are a helpful agent. Use `apply_patch` for file edits and use `exec_command` or `write_stdin` only when command execution is required. Keep file changes inside the workspace, avoid paths containing `..`, and answer directly only when no tool action is needed.",
    )
}
```

- [ ] **Step 3: Add a helper that submits one turn through an existing thread/runtime pair**

Implement a reusable helper in `crates/cli/src/runtime.rs`:

```rust
/// Runs one CLI turn through an existing thread/runtime pair.
pub async fn run_cli_turn<M>(
    runtime: &ThreadRuntime<M, TracingEventSink>,
    thread: &ThreadHandle,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
{
    let result = runtime.run(thread, ThreadRunRequest::new(prompt)).await?;
    Ok(result.text)
}
```

If using `TracingEventSink` directly in the signature becomes awkward, factor the helper generically over the event sink:

```rust
pub async fn run_cli_turn<M, E>(
    runtime: &ThreadRuntime<M, E>,
    thread: &ThreadHandle,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
```

- [ ] **Step 4: Reimplement `run_cli_prompt(...)` in terms of the new helpers**

Refactor the existing entry helper instead of deleting it:

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
    let thread = build_cli_thread_handle();
    let runtime = ThreadRuntime::new(model, store, router, Arc::new(TracingEventSink));
    run_cli_turn(&runtime, &thread, prompt).await
}
```

This preserves the existing one-shot helper while making it reuse the shared turn path.

- [ ] **Step 5: Add or update a unit test that proves a reused thread preserves context across two turns**

Extend the runtime test module in `crates/cli/src/runtime.rs` with a model double that returns two responses and inspects the second request:

```rust
#[tokio::test]
async fn reuses_one_thread_for_multiple_cli_turns() {
    let model = Arc::new(RecordingTwoTurnModel::new());
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(ToolRouter::new(
        Arc::new(kernel::tools::ToolRegistry::default()),
        Vec::new(),
    ));
    let runtime = ThreadRuntime::new(
        Arc::clone(&model),
        Arc::clone(&store),
        router,
        Arc::new(TracingEventSink),
    );
    let thread = build_cli_thread_handle();

    let first = run_cli_turn(&runtime, &thread, "hello".to_string()).await.unwrap();
    let second = run_cli_turn(&runtime, &thread, "follow up".to_string()).await.unwrap();

    assert_eq!(first, "first reply");
    assert_eq!(second, "second reply");

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.len() >= 3);
}
```

- [ ] **Step 6: Run the focused CLI runtime test**

Run: `cargo test -p cli runtime::tests::runs_prompt_through_kernel_agent_runtime -- --exact`
Expected: PASS

- [ ] **Step 7: Run the new multi-turn helper test**

Run: `cargo test -p cli runtime::tests::reuses_one_thread_for_multiple_cli_turns -- --exact`
Expected: PASS

- [ ] **Step 8: Commit the runtime helper refactor**

```bash
git add crates/cli/src/runtime.rs
git commit -m "Refactor CLI runtime helpers for reusable thread turns"
```

### Task 2: Add Interactive REPL Mode to CLI Main

**Files:**
- Modify: `crates/cli/src/main.rs`
- Test: `cargo test -p cli`

- [ ] **Step 1: Write the failing mode-selection expectation**

Document the two desired paths in `main.rs`:

```rust
if prompt.trim().is_empty() {
    run_interactive_cli(...).await?;
} else {
    let result = runtime::run_cli_prompt(..., prompt).await?;
    println!("{result}");
}
```

Expected compile failure before edits: `run_interactive_cli` does not exist.

- [ ] **Step 2: Add a small helper that runs the interactive input loop**

Implement a helper in `crates/cli/src/main.rs` so the loop is isolated from startup code:

```rust
/// Runs the interactive CLI loop on a single in-memory thread until the user exits.
async fn run_interactive_cli(
    model: Arc<CliAgentModel>,
    store: Arc<InMemorySessionStore>,
    router: Arc<tools::router::ToolRouter>,
) -> Result<(), Box<dyn std::error::Error>> {
    let thread = runtime::build_cli_thread_handle();
    let runtime = kernel::ThreadRuntime::new(
        Arc::clone(&model),
        store,
        router,
        Arc::new(runtime::TracingEventSink),
    );
    let mut line = String::new();

    loop {
        print!("> ");
        std::io::Write::flush(&mut std::io::stdout())?;
        line.clear();
        if std::io::stdin().read_line(&mut line)? == 0 {
            break;
        }

        let prompt = line.trim();
        if prompt.eq_ignore_ascii_case("exit") || prompt.eq_ignore_ascii_case("quit") {
            break;
        }
        if prompt.is_empty() {
            continue;
        }

        match runtime::run_cli_turn(&runtime, &thread, prompt.to_string()).await {
            Ok(text) => println!("{text}"),
            Err(err) => eprintln!("error: {err}"),
        }
    }

    Ok(())
}
```

If `TracingEventSink` is private, first make it visible within the crate by changing it to `pub(crate) struct TracingEventSink;` in `crates/cli/src/runtime.rs`.

- [ ] **Step 3: Update `main()` to branch between one-shot mode and interactive mode**

Refactor the main flow so empty argv prompt enters REPL instead of printing usage:

```rust
let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");

let model = Arc::new(build_agent_model(&config)?);
let store = Arc::new(InMemorySessionStore::default());
let router = Arc::new(create_default_tool_router().await);

if prompt.trim().is_empty() {
    run_interactive_cli(Arc::clone(&model), Arc::clone(&store), Arc::clone(&router)).await?;
} else {
    let result = runtime::run_cli_prompt(Arc::clone(&model), store, router, prompt).await?;
    println!("{result}");
}
```

Keep startup config validation and `model.close().await` behavior intact after the selected mode finishes.

- [ ] **Step 4: Update usage-related tests to match the new behavior**

The old “missing prompt prints usage and exits” expectation must be replaced or narrowed because no prompt now means interactive mode. Update the CLI tests so they validate the new control flow instead of the removed usage behavior.

For example, replace the old usage test with a smaller pure-function test if needed:

```rust
#[test]
fn usage_message_remains_available_for_documentation_only() {
    assert!(usage_message().contains("cargo run -p cli"));
}
```

Do not keep an integration assertion that empty argv must exit with code 2.

- [ ] **Step 5: Run the full CLI test suite**

Run: `cargo test -p cli`
Expected: PASS, including runtime tests and updated CLI mode tests.

- [ ] **Step 6: Commit the interactive CLI mode**

```bash
git add crates/cli/src/main.rs crates/cli/src/runtime.rs crates/cli/tests/usage.rs
git commit -m "Add interactive multi-turn mode to CLI"
```

### Task 3: Verify Workspace Integration

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/src/runtime.rs`
- Test: `cargo test`

- [ ] **Step 1: Search for stale one-shot-only assumptions in CLI**

Run:

```bash
rg -n "usage_when_prompt_is_missing|without prompt|prompt is missing|exit\\(2\\)" crates/cli -g '!target'
```

Expected: no stale tests or code paths that require empty argv to fail fast.

- [ ] **Step 2: Make any final cleanup edits found by the search**

Keep the CLI behavior aligned with the spec:

- empty argv enters interactive mode
- one-shot argv still works
- per-turn REPL failures do not terminate the session

- [ ] **Step 3: Run full workspace verification**

Run: `cargo test`
Expected: PASS across `cli`, `kernel`, `llm`, and `tools`.

- [ ] **Step 4: Commit the final cleanup if needed**

```bash
git add crates/cli/src/main.rs crates/cli/src/runtime.rs crates/cli/tests/usage.rs
git commit -m "Finalize CLI multi-turn conversation support"
```

## Self-Review

- Spec coverage: the plan covers reusable CLI thread helpers, interactive mode in `main.rs`, per-turn error handling, test updates, and full verification.
- Placeholder scan: no `TODO`, `TBD`, or vague future steps remain.
- Type consistency: the plan consistently uses `ThreadHandle`, `ThreadRuntime`, `ThreadRunRequest`, `build_cli_thread_handle`, and `run_cli_turn`.

# Auto Context Compaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在每次 LLM 请求前按模型 `context_tokens` 与用户配置比例自动触发现有上下文压缩。

**Architecture:** 配置层增加 `compaction.auto` 与 `compaction.trigger_ratio`。Kernel turn 执行前根据当前模型配置构造自动压缩策略，超过阈值时复用 `ContextManager::compact()`，先持久化 checkpoint 再替换 live history。自动压缩失败只记录 warning 并暂停本 turn 后续自动压缩。

**Tech Stack:** Rust, typed-builder, serde/TOML, tokio async tests, existing kernel/store/protocol abstractions.

---

### Task 1: 配置字段与默认值

**Files:**
- Modify: `crates/config/src/config.rs`

- [ ] **Step 1: Write failing config tests**

在 `crates/config/src/config.rs` 的 tests 中新增断言：

```rust
/// AppConfig defaults automatic compaction to disabled with a 90% trigger ratio.
#[test]
fn app_config_default_auto_compaction_is_disabled_at_ninety_percent() {
    let cfg = AppConfig::default();

    assert!(!cfg.compaction.auto);
    assert_eq!(cfg.compaction.trigger_ratio, 0.9);
}

/// AppConfig reads automatic compaction settings from TOML.
#[test]
fn app_config_reads_auto_compaction_settings() {
    let cfg: AppConfig = toml::from_str(
        r#"
[compaction]
auto = true
trigger_ratio = 0.75
"#,
    )
    .expect("parse app config");

    assert!(cfg.compaction.auto);
    assert_eq!(cfg.compaction.trigger_ratio, 0.75);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p config compaction`

Expected: FAIL because `CompactionConfig` has no `auto` or `trigger_ratio` fields.

- [ ] **Step 3: Add config fields**

Add fields and defaults:

```rust
/// Whether context compaction should run automatically before model requests.
#[serde(default)]
pub auto: bool,
/// Fraction of the current model context window that triggers automatic compaction.
#[serde(default = "default_compaction_trigger_ratio")]
pub trigger_ratio: f64,
```

Add:

```rust
/// Default context window fraction that triggers automatic compaction.
fn default_compaction_trigger_ratio() -> f64 {
    0.9
}
```

Because `f64` does not implement `Eq`, remove `Eq` from `CompactionConfig` and `AppConfig` derives while keeping `PartialEq`.

- [ ] **Step 4: Run config tests**

Run: `cargo test -p config compaction`

Expected: PASS.

### Task 2: 自动压缩策略与请求前触发

**Files:**
- Modify: `crates/kernel/src/turn.rs`

- [ ] **Step 1: Write failing turn tests**

Add tests proving:

```rust
#[tokio::test]
async fn execute_turn_auto_compacts_before_stream_request_when_threshold_is_reached() {
    // Build AppConfig with compaction.auto=true, trigger_ratio=0.5, context_tokens=10.
    // Seed context with older history and retained tail.
    // Use an LLM whose completion() returns "summary" and whose stream() captures request.chat_history.
    // Assert captured stream history starts with the summary marker and old history is absent.
}

#[tokio::test]
async fn execute_turn_continues_when_auto_compaction_fails() {
    // Build same config, but completion() returns an error.
    // Assert execute_turn still completes and stream() was called with the original history.
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p kernel auto_compaction -- --nocapture`

Expected: FAIL because no automatic compaction path exists.

- [ ] **Step 3: Implement auto compaction policy**

Add an internal `AutoCompactionPolicy` in `turn.rs` with:

```rust
#[derive(Debug, Clone, Copy)]
struct AutoCompactionPolicy {
    context_tokens: u64,
    trigger_ratio: f64,
}
```

Add methods:

- `from_turn_context(ctx: &TurnContext) -> Option<Self>`
- `estimated_request_tokens(&self, context: &dyn ContextManager, preamble: &str) -> usize`
- `should_compact(&self, estimated_tokens: usize) -> bool`

Use `ctx.provider_id`, `ctx.llm.model_id()`, and `ctx.app_config.providers` to find model `context_tokens`.

- [ ] **Step 4: Persist checkpoint before replacing history**

Add `TurnContext::persist_compaction_checkpoint(&self, output: &CompactionOutput) -> anyhow::Result<()>` that writes `PersistedPayload::Compaction`.

- [ ] **Step 5: Trigger before request construction**

Inside `execute_turn`, after `drain_inter_agent_inputs_for_turn()` and before `let history = context.history().to_vec();`, call a new `TurnContext::auto_compact_if_needed(...)`. Track a `auto_compaction_suspended` boolean so `None`, error, or ineffective compaction does not retry every tool loop in the same turn.

- [ ] **Step 6: Run kernel tests**

Run: `cargo test -p kernel auto_compaction -- --nocapture`

Expected: PASS.

### Task 3: Full verification

**Files:**
- Modify: `crates/config/src/config.rs`
- Modify: `crates/kernel/src/turn.rs`
- No schema migration required.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

Expected: no output.

- [ ] **Step 2: Test affected crates**

Run: `cargo test -p config -p kernel`

Expected: PASS.

- [ ] **Step 3: Lint affected crates**

Run: `cargo clippy -p config -p kernel --all-targets -- -D warnings`

Expected: PASS.

### Self-Review

- Spec coverage: 配置、触发时机、checkpoint、失败不阻断、缺失 `context_tokens` 跳过均有任务覆盖。
- Placeholder scan: 无 TBD/TODO/占位步骤。
- Type consistency: `trigger_ratio` 使用 `f64`；`auto` 使用 `bool`；自动压缩策略只在 kernel turn 内部使用。

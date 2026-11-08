# Compaction pi Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align clawcode context compaction, branch summaries, and their extension/ACP/WebUI surfaces with pi `a44127d91`.

**Architecture:** Kernel-internal behavior changes first (cut points, summary request shape, Run-internal threshold check), then interface changes (per-model overrides in config, extension events in protocol/extension, a new `MessageContent::BranchSummary` that flows through replay, provider, ACP, and WebUI, plus client-requested branch summaries). Every summary request still goes through `Kernel::generate_compaction_summary`.

**Tech Stack:** Rust workspace (`kernel`, `protocol`, `config`, `provider`, `extension`, `acp`, `app` crates; tokio; typed-builder; serde), React/TypeScript WebUI in `web/`.

**Spec:** [`docs/superpowers/specs/2026-11-08-compaction-pi-alignment-design.md`](../specs/2026-11-08-compaction-pi-alignment-design.md)

## Global Constraints

- All new or modified code carries English comments for non-trivial logic; every new function has a function-level doc comment.
- Structs with more than 3 fields use `typed-builder`; `Option` fields carry `#[builder(default)]`.
- Clone `Arc` fields as `Arc::clone(&self.field)`.
- Tracing macros are fully qualified (`tracing::warn!`), use plain format arguments, no structured fields; never log summary or prompt bodies.
- Test code lives only in crate `tests/` directories or `#[cfg(test)] mod tests`.
- No new Cargo dependencies.
- Frontend changes have no unit tests; WebUI acceptance runs against the production `app` backend.
- Do **not** commit per task. All work ships as one commit on `main` together with the spec and this plan, and only after the user explicitly asks. Before that commit: run pre-commit hooks and apply `.gitmessage` exactly.
- Per-task check after the task's own tests pass: `cargo fmt --all` and `cargo clippy -p <touched crates> --tests -- -Dwarnings`.

## Review Focus

1. A long Tool loop on a small window where summary + retained tail still exceeds the threshold: each Turn compacts at most once and never loops; a Turn with nothing compactable produces no summary request and no `warn` log. Test added to Task 4.
2. User presses Stop while a Run-internal compaction is generating: Run ends Cancelled and no compaction entry is written. Test added to Task 4.
3. Opening a session that already contains old `branch_summary` entries (no `details`, or extension `details` of arbitrary shape, `fromHook: true`): the session loads, the summary enters context, and file lists are empty. Test added to Task 8.
4. Navigating with `summarize = true` to a descendant of the current leaf (nothing abandoned; navigating to an ancestor does abandon entries and is summarized, as in pi): navigation succeeds and no branch summary entry is written. Test added to Task 10.
5. A summary request whose serialized input already fills the window: the requested `max_tokens` clamps to 1 instead of underflowing, so the provider fails cleanly and nothing is persisted. Unit test added to Task 2.

---

### Task 1: Cut-point rules

**Files:**
- Modify: `crates/kernel/src/runtime/compaction/context.rs:169-279` (`CompactionPreparation::from_history`) and its `#[cfg(test)] mod tests`
- Modify: `crates/kernel/src/runtime/compaction.rs:1-11` (re-export)
- Test: `crates/kernel/src/runtime/compaction/context.rs` (`mod tests`), `crates/kernel/tests/compaction.rs` fixture, plus any kernel test asserting `keep_recent_tokens = 0` behavior (`crates/kernel/tests/slash_commands.rs:411`, `crates/kernel/tests/lifecycle_order.rs:1988`, `crates/kernel/tests/compaction.rs:1314,2113,2172`)

**Interfaces:**
- Produces: `pub(in crate::runtime) trait CompactionBoundary { fn starts_turn(&self) -> bool; fn is_cut_point(&self) -> bool; }` implemented for `AgentMessage` in `context.rs`, re-exported from `compaction.rs`. Task 8 adds `BranchSummary` to both methods.

- [ ] **Step 1: Write the failing unit tests in `context.rs` `mod tests`** (reuse the existing `user()`, `assistant(n)`, `tool_result(n)`, `extension(n, visible)`, `keep_recent(n)` helpers)

```rust
/// Trailing Tool results larger than the budget keep their Assistant call instead of everything.
#[test]
fn oversized_trailing_tool_results_fall_back_to_last_cut_point() {
    let history = [user(), assistant(40), user(), assistant(16), tool_result(400)];
    let preparation = CompactionPreparation::from_history(&history, keep_recent(60));
    assert_eq!(preparation.summarized.len(), 2);
    assert_eq!(preparation.turn_prefix.len(), 1);
    assert!(matches!(preparation.retained_tail[0].content, MessageContent::Assistant { .. }));
    assert_eq!(preparation.retained_tail.len(), 2);
}

/// A zero budget still keeps the newest cut point, matching pi.
#[test]
fn zero_keep_recent_keeps_the_last_cut_point() {
    let history = [user(), assistant(40), user(), assistant(40)];
    let preparation = CompactionPreparation::from_history(&history, keep_recent(0));
    assert_eq!(preparation.summarized.len(), 2);
    assert_eq!(preparation.turn_prefix.len(), 1);
    assert_eq!(preparation.retained_tail.len(), 1);
}

/// A visible Extension message starts a Turn, so cutting there is not a split Turn.
#[test]
fn visible_extension_starts_a_turn() {
    let history = [user(), assistant(400), extension(400, true), assistant(40)];
    let preparation = CompactionPreparation::from_history(&history, keep_recent(60));
    assert!(preparation.turn_prefix.is_empty());
    assert_eq!(preparation.summarized.len(), 2);
}
```

Update `hidden_extension_is_not_a_compaction_cut_point`: the retained tail now starts with `MessageContent::Assistant`, `turn_prefix.len() == 1`, and the ToolResult count stays 2.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p kernel --lib runtime::compaction::context::tests`
Expected: the three new tests and the updated hidden-extension test FAIL.

- [ ] **Step 3: Implement `CompactionBoundary` and the new selection loop**

`starts_turn`: `User`, `ExpandedUser`, `BashExecution`, `Extension` with `include_in_context`. `is_cut_point`: `starts_turn() || Assistant`. Selection: delete the `keep_recent_tokens == 0` branch; walk `compactable` newest→oldest, `continue` on `estimated_tokens() == 0`, and once the running total reaches `keep_recent_tokens` pick `valid_cut_points.iter().find(|cut| *cut >= index)` else `valid_cut_points.last()`; when the budget is never reached keep the current default (first cut point). Replace the two inline `matches!` turn-start checks with `starts_turn()`.

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p kernel --lib runtime::compaction::context::tests`
Expected: PASS.

- [ ] **Step 5: Rework the compaction fixtures**

In `crates/kernel/tests/compaction.rs`:

- `CompactionFixture::new` (currently `keep_recent = 0`, one historical Turn) can no longer summarize the whole history. Change only `new` so that it calls `new_with_scripted_history` with two historical Turns (`"historical question"`/`"historical answer"` then `"recent question"`/`"recent answer"`) and `keep_recent_tokens = 8` (the estimate of the second Turn: 4 + 4), so the cut falls on the second user message and nothing is split. `new_with_keep_recent` and `new_with_scripted_history` keep their current behavior. The summary request moves from index 1 to index 2 (for example `requests.get(1)` at `compaction.rs:698`).
- `OverflowFixture` runs one prompt on an empty session (`compaction.rs:1940`), so after this task its overflow has nothing to compact. Give `with_keep_recent` a leading scripted `"historical answer"` response and make `run()` first complete a `"historical question"` Run before sending `"overflow request"`. Request counts and indexes in the overflow tests shift by one (for example `requests.len() == 3` becomes 4 and `requests[2]` becomes `requests[3]`).
- Add `first_prompt_overflow_fails_without_compaction`: a fresh session whose first prompt overflows ends with `TurnOutcome::Failed`, sends exactly one request, and emits no `CompactionStart`.

- [ ] **Step 6: Run the kernel suite and update expectations**

Run: `cargo test -p kernel`
For each failure caused by the new cut-point semantics, update the assertion, never production code, and keep the test's intent: tests still using `keep_recent_tokens = 0` with a single Turn now produce a split-Turn summary prefixed by `No prior history.\n\n---\n\n**Turn Context (split turn):**\n\n` and retain the last Assistant; tests whose intent needs whole-Turn summarization (for example `second_compaction_uses_previous_summary_and_new_messages`) use a history and budget whose cut lands on a user message. Expected after updates: PASS.

---

### Task 2: Summary output budget, truncation rejection, thinking level

**Files:**
- Modify: `crates/kernel/src/runtime/compaction.rs:618-837` (`generate_compaction_summary`)
- Modify: `crates/kernel/src/runtime/compaction/prompt.rs` (`SummaryProtocol`)
- Test: `crates/kernel/src/runtime/compaction.rs` new `#[cfg(test)] mod tests`, `crates/kernel/tests/compaction.rs`

**Interfaces:**
- Produces: `fn summary_max_tokens(profile: &ModelProfile, input_tokens: u64) -> u64` (private leaf function in `compaction.rs`); `impl SummaryProtocol<'_> { pub(in crate::runtime) fn failure_label(&self) -> &'static str }` returning `"Summarization"`, `"Turn prefix summarization"`, `"Branch summarization"`.

- [ ] **Step 1: Write failing tests**

Unit tests in `compaction.rs` `mod tests` (build profiles with `ModelProfile::builder()`):

```rust
#[test]
fn summary_max_tokens_follows_pi_context_clamp() {
    assert_eq!(summary_max_tokens(&profile(100_000, 8_000), 1_000), 8_000);
    assert_eq!(summary_max_tokens(&profile(200_000, 64_000), 150_000), 32_768);
    assert_eq!(summary_max_tokens(&profile(1_000_000, 128_000), 100_000), 128_000);
    // Review Focus 5: input already fills the window.
    assert_eq!(summary_max_tokens(&profile(32_000, 8_000), 30_000), 1);
}
```

Integration tests in `crates/kernel/tests/compaction.rs`:

```rust
/// A summary stopped by the output cap is incomplete and never becomes a checkpoint.
#[tokio::test]
async fn truncated_summary_is_rejected_and_not_persisted() {
    // CompactionFixture::new with one response: TextDelta("partial") + finished(StopReason::MaxTokens)
    // compact_session(..) is Err; error text contains
    // "Summarization failed: generation hit the token cap and the summary is incomplete";
    // session_tree has no "compaction" entry; session_messages == messages_before;
    // exactly one summary request was sent (no retry).
}

/// Summary requests use the model output limit and the Session's thinking level.
#[tokio::test]
async fn summary_request_uses_output_limit_and_session_thinking_level() {
    // with_history_and_summary; before compacting call
    // kernel.set_session_thinking_level(&id, ThinkingLevel::High) (the default is Off, which would
    // make the assertion vacuous); last request: options.max_tokens == Some(8_000) and
    // options.thinking_level == ThinkingLevel::High
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p kernel --lib runtime::compaction::tests && cargo test -p kernel --test compaction truncated_summary summary_request_uses`
Expected: FAIL (function missing; MaxTokens accepted; thinking level is Off). The fixture's `max_tokens` already equals 8_000 because its `max_output_tokens` caps `0.8 × reserve`; that assertion guards against regressions.

- [ ] **Step 3: Implement**

`summary_max_tokens` follows spec 1.2 exactly with saturating arithmetic (margin `max(4096, ceil(input * 0.15))`, the 32_768 fallback, final `min(max_output_tokens, max(1, room))`). In `generate_compaction_summary`: drop the `0.8`/`0.5` caps; take `let (model, thinking_level) = session.model.snapshot()?;` instead of `session.model.active()?`; after building `request_messages`, set `max_tokens = Some(summary_max_tokens(model.profile(), sum of estimated_tokens()))` for all three protocols; `thinking_level(thinking_level)`. In the stop-reason match, `StopReason::MaxTokens` returns `ModelError::Protocol(format!("{} failed: generation hit the token cap and the summary is incomplete", protocol.failure_label()))` (keep the label before `protocol` is consumed). Update the "Summaries never reason" comment.

- [ ] **Step 4: Run tests**

Run: `cargo test -p kernel --lib runtime::compaction::tests && cargo test -p kernel --test compaction`
Expected: PASS.

---

### Task 3: Summary serialization and split-Turn prompt

**Files:**
- Modify: `crates/kernel/src/runtime/compaction/prompt.rs` (`SummaryProtocol`, `SummaryPrompt::render`, `TURN_PREFIX_SUMMARIZATION_PROMPT`)
- Modify: `crates/kernel/src/runtime/compaction.rs:416-428` (prefix generation call)
- Test: `crates/kernel/tests/compaction.rs`

**Interfaces:**
- Consumes: Task 2 `failure_label`.
- Produces: `SummaryProtocol::TurnPrefix { custom_instructions: Option<&'a str> }`. Task 9 adds a `Branch` template to the same renderer.

- [ ] **Step 1: Write failing tests**

```rust
/// Summary input omits reasoning and formats Tool calls as key=value arguments.
#[tokio::test]
async fn summary_input_omits_thinking_and_formats_tool_arguments() {
    // new_with_scripted_history(history: [ReasoningDelta("secret reasoning") + ToolCall read
    // {"path": "input.txt"} finished ToolUse, then an EndTurn answer], summaries: [one prefix summary],
    // prepare_history = true, keep_recent_tokens = 1). The cut lands on the final Assistant, so the
    // only summary request is the split-Turn prefix; its User message text:
    // !contains("secret reasoning"), !contains("[Assistant thinking]"),
    // contains("[Assistant tool calls]: read(path=\"input.txt\")")
}
```

Update `split_turn_compaction_preserves_prefix_context`: the prefix prompt starts with `"# Conversation\n"`, contains `"\n\n# Instructions\nThe messages above are earlier context from an ongoing conversation."`, and no longer contains `"This is the PREFIX of a turn"`. Add:

```rust
/// Custom compaction instructions also steer the split-Turn prefix summary.
#[tokio::test]
async fn split_turn_prefix_receives_custom_instructions() {
    // new_with_keep_recent(.., 1); run "/compact keep the API names";
    // prefix request text ends with "\n\nAdditional focus: keep the API names"
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p kernel --test compaction summary_input_omits split_turn`
Expected: FAIL.

- [ ] **Step 3: Implement**

`render`: remove the Reasoning branch; Tool calls render `name(k1=<json>, k2=<json>)` by iterating the argument object (`key=serde_json::to_string(value)`), non-object arguments render as `name(<json>)`; key order is not guaranteed. Replace `TURN_PREFIX_SUMMARIZATION_PROMPT` with spec appendix A.1 verbatim. `SummaryTemplate::TurnPrefix` renders `# Conversation\n{conversation}\n\n# Instructions\n{prompt}`; `Checkpoint` keeps `<conversation>` framing. Both append `\n\nAdditional focus: {trimmed}` when instructions are non-empty. `generate_compaction_summary` passes `custom_instructions` for `TurnPrefix`; `compact_with_identity` passes `instruction.as_deref()`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p kernel --test compaction`
Expected: PASS.

---

### Task 4: Run-internal threshold compaction

**Files:**
- Modify: `crates/kernel/src/runtime/run/turn.rs:1185-1194` (`next_action` continuation branch), add `RunScope::compact_before_next_turn`
- Modify: `crates/kernel/src/runtime/compaction.rs:245-259` (log level by reason)
- Modify (test support): `crates/kernel/tests/subagent/fixture.rs:364-392` (`with_factories` gains a `compaction: CompactionPolicy` parameter; existing constructors pass `CompactionPolicy::default()`; new `TestKernel::with_compaction`)
- Test: `crates/kernel/tests/compaction.rs`, `crates/kernel/tests/protocol_alignment/queue.rs`

**Interfaces:**
- Consumes: `Kernel::perform_compaction`, `ContextUsageEstimate::from_history`, Task 1 selection.
- Produces: `async fn compact_before_next_turn(&self, turn: &TurnContext, turn_loop: &mut TurnLoop<'_>, settled: &SettledAssistant) -> Result<Option<RunCompletion>, KernelError>` on `RunScope`. Task 5 swaps its policy source for the resolved policy.
- Produces (test support in `crates/kernel/tests/compaction.rs`): `async fn threshold_kernel_with(root: &Path, model: &Arc<ScriptedModel>, keep_recent_tokens: u64, session_id: &SessionId, extensions: StaticExtensionFactory) -> Kernel`; `threshold_kernel` delegates with `StaticExtensionFactory::default()`. Tasks 6, 7, and 8 register extension handlers through it.

- [ ] **Step 1: Write failing tests** (in `compaction.rs`, using `threshold_kernel`/`threshold_kernel_with` and a Tool-use Turn whose `Finished` usage is `total_tokens = 84_000` with `StopReason::ToolUse` and a `read` call on `input.txt`; create that file in the session cwd)

```rust
/// A Tool Turn over the threshold compacts before the next model request of the same Run.
#[tokio::test]
async fn tool_turn_over_threshold_compacts_before_next_turn() {
    // responses: [large tool turn, summary, final answer]
    // CompactionStart{Threshold} sits after the first TurnEnd and before the second TurnStart;
    // requests[2].messages[1] is MessageContent::CompactionSummary; Run succeeds with 2 Turns.
}

#[tokio::test]
async fn tool_turn_below_threshold_does_not_compact() { /* small usage: 2 requests, no compaction */ }

#[tokio::test]
async fn run_internal_summary_failure_keeps_the_run_going() {
    // [large tool turn, Err(non-retryable), final answer]: Run Succeeded, CompactionEnd Failed emitted
}

#[tokio::test]
async fn run_internal_extension_cancel_keeps_the_run_going_without_warn() {
    // register a session_before_compact handler returning cancel: true; CapturedLogs has no
    // "continuing Run without" warn line; Run Succeeded
}

#[tokio::test]
async fn run_internal_cancellation_ends_the_run_cancelled() {
    // summary response Err(ModelError::Cancelled): AgentSettled outcome Cancelled,
    // no compaction entry (Review Focus 2)
}

#[tokio::test]
async fn run_internal_compaction_is_not_repeated_after_the_run() {
    // [large tool turn, summary, small final answer]: exactly one CompactionStart in the Run
}

#[tokio::test]
async fn tool_loop_without_compactable_context_never_loops() {
    // keep_recent_tokens = 1_000_000, three large tool turns then an answer:
    // no summary request, no CompactionStart, no "no model context to compact" warn (Review Focus 1)
}
```

In `protocol_alignment/queue.rs`, using `TestKernel::with_compaction(route, policy)` (128k window; `policy` enabled, reserve 16_384, `keep_recent_tokens = 1` so the held Turn's user message is compactable — the default 20_000 budget would leave nothing to compact) with a route: the held first Turn finishes `EndTurn` with `input_tokens = 130_000` (build the `ModelFinal` inline; the shared `finished()` reports zero usage); a request whose system text starts with `"You are a context summarization assistant"` answers `"summary"`; others answer `"answer"`:

```rust
/// A silently over-window answer with a queued follow-up compacts before the follow-up Turn.
#[tokio::test]
async fn compact_only_runs_before_a_queued_follow_up() {
    // queue "follow" while held, release; a CompactionEnd{Overflow} precedes the TurnStart of "follow"
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p kernel --test compaction tool_turn run_internal tool_loop && cargo test -p kernel --test protocol_alignment compact_only_runs`
Expected: FAIL.

- [ ] **Step 3: Implement `compact_before_next_turn`**

Follow spec §2: reason `Overflow` for `CompactOnly`, else `Threshold` when `ContextUsageEstimate::from_history` (via `session.transcript.inspect_history`) `should_compact` against `self.session.model.active()?.profile().context_tokens` and `kernel.compaction_policy` (Task 5 replaces with the resolved policy); call `perform_compaction` without exclusion. Result mapping: `Ok(_)` → `Ok(None)`; `ExtensionBlocked` → `tracing::debug!`, `Ok(None)`; `error.is_cancellation() || self.cancellation.is_cancelled()` → `Ok(Some(turn_loop.finish(self.run_id, turn.turn_id.clone(), AgentOutcome::Cancelled, None)))`; other → `tracing::warn!("continuing Run without its {:?} compaction before the next Turn: {}", reason, error)`, `Ok(None)`. In `next_action`, the `continue_for_tools || next_queued_item.is_some()` branch returns `self.compact_before_next_turn(turn, turn_loop, &settled).await`.

In `compact_with_identity`, the "no model context to compact" line logs `tracing::warn!` only for `CompactionReason::Manual`, otherwise `tracing::debug!`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p kernel`
Expected: PASS.

---

### Task 5: Per-model compaction overrides

**Files:**
- Modify: `crates/config/src/config.rs:17` (replace re-export), `:143` and `:318-340` (validation), add errors
- Create: none (types go in `crates/config/src/config.rs` next to `AppConfig`)
- Modify: `crates/config/src/lib.rs` (exports), `crates/provider/src/factory/mod.rs:185-195,309-350`
- Modify: `crates/kernel/src/runtime.rs:226-227,244-272,291,327`, `crates/kernel/src/runtime/compaction.rs` (pre-prompt, `compact_with_identity`), `crates/kernel/src/runtime/run/turn.rs:831-835,1197-1203` and Task 4 method
- Modify: `crates/app/src/lib.rs:203`, `README.md`, `README_zh.md` (compaction config section)
- Test: `crates/config/tests/loading.rs`, `crates/provider/tests/upstream_factory.rs`, `crates/kernel/tests/compaction.rs`

**Interfaces:**
- Produces (config crate, exported): `CompactionConfig { #[serde(flatten)] pub policy: CompactionPolicy, #[serde(default)] pub model_overrides: BTreeMap<String, CompactionModelOverride> }` with `Default`, `From<CompactionPolicy>`; `CompactionModelOverride { reserve_tokens: Option<u64>, keep_recent_tokens: Option<u64> }`; `CompactionConfig::resolve(&self, provider_id: &str, model_id: &str) -> CompactionPolicy`; `CompactionConfig::for_profile(&self, profile: &ModelProfile) -> CompactionPolicy`; `ConfigValidationError::MalformedCompactionOverride { key: String }` and `UnknownCompactionOverride { key: String }`.
- Kernel/KernelFactory field `compaction_policy` becomes `CompactionConfig`; KernelFactory attribute `#[builder(default, setter(into))]`, so `.compaction_policy(CompactionPolicy::…)` in existing tests still compiles. The field keeps its old name on purpose to avoid churning every test builder call. `CompactionConfig` derives `Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize` (whatever `AppConfig` field types need; `CompactionModelOverride` the same); it is no longer `Copy`, so `runtime.rs:291` passes `self.compaction_policy.clone()`.

- [ ] **Step 1: Write failing tests**

`crates/config/tests/loading.rs`:

```rust
#[test]
fn compaction_model_overrides_resolve_by_exact_provider_model_key() {
    // ConfigFixture::valid() + [compaction.model_overrides."deepseek/deepseek-v4-flash"]
    // reserve_tokens = 32768; resolve("deepseek","deepseek-v4-flash") -> reserve 32768, keep 20000;
    // resolve("deepseek","other") -> reserve 16384; enabled always the global value
}

#[test]
fn malformed_compaction_override_key_is_rejected() {
    // key "deepseek-v4-flash" -> validate() Err(MalformedCompactionOverride { key })
}

#[test]
fn request_budget_is_checked_against_the_overridden_reserve() {
    // request_max_tokens = 20000 with override reserve 32768 -> validate() Ok
}
```

Update `defaults_match_pi_retry_and_compaction` to read `config.compaction.policy.*`.

`crates/provider/tests/upstream_factory.rs`: `override_reserve_bounds_default_request_budget` — override reserve 4_000 for the factory model with `max_output_tokens = 64000` and no `request_max_tokens` → `profile().request_max_tokens == 4_000`.

`crates/kernel/tests/compaction.rs` (KernelFactory over `StaticModelFactory`, whose catalog has `fixture/compaction`):

```rust
#[tokio::test]
async fn unknown_compaction_override_fails_kernel_build() { /* key "fixture/missing" -> build Err containing "fixture/missing" */ }
#[tokio::test]
async fn override_reserve_at_or_above_window_fails_kernel_build() { /* reserve 100_000 for fixture/compaction */ }
#[tokio::test]
async fn override_reserve_changes_the_threshold() {
    // global reserve 16_384, override reserve 90_000 for fixture/compaction:
    // a 15_000-token answer now compacts (threshold 10_000)
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p config --test loading compaction && cargo test -p provider --test upstream_factory override_reserve && cargo test -p kernel --test compaction override`
Expected: FAIL (types missing).

- [ ] **Step 3: Implement config types and validation**

`resolve` merges an override found by `format!("{provider_id}/{model_id}")` over `policy`; `enabled` comes from `policy`. Load-time `validate` rejects keys not of the non-empty `provider/model` form (same check `active_model` uses) and checks `RequestBudgetExceedsReserve` with `resolve(provider.id, model.id).reserve_tokens`.

- [ ] **Step 4: Wire provider factory and kernel**

Provider factory passes `app.compaction.resolve(provider.id.as_str(), &model.id).reserve_tokens` to `profile_for`. Kernel build (`runtime.rs:244-272`): every override key must match a `ModelCatalog::profiles()` entry (`UnknownCompactionOverride`); run the existing reserve/budget checks for the active profile and every profile that has an override, using `for_profile`. Replace each `self.compaction_policy`/`kernel.compaction_policy` read with `kernel.compaction_policy.for_profile(<profile in hand>)`: pre-prompt uses `profile`; `compact_with_identity` uses `session.model.active()?.profile()`; `turn.rs:833` uses `turn.model.profile()`; `turn.rs:1201` uses `turn.model.profile()`; Task 4 uses the active profile. App passes `snapshot.compaction` (whole config). Update both READMEs' compaction section with a `model_overrides` example matching spec §3.

- [ ] **Step 5: Run tests**

Run: `cargo test -p config && cargo test -p provider && cargo test -p kernel && cargo test -p app`
Expected: PASS.

---

### Task 6: Extension preparation data and custom instructions

**Files:**
- Modify: `crates/protocol/src/extension/events/session.rs:76-89` (event field), add `CompactionPreparation` there; export in `crates/protocol/src/lib.rs`
- Modify: `crates/kernel/src/runtime/compaction/context.rs` (store the policy on the internal preparation; `From` impl)
- Modify: `crates/kernel/src/runtime/compaction.rs:277-292`
- Test: `crates/kernel/tests/compaction.rs` (through Task 4's `threshold_kernel_with`)

**Interfaces:**
- Produces: `protocol::CompactionPreparation` (typed-builder, camelCase serde) with fields `messages_to_summarize: Vec<AgentMessage>`, `turn_prefix_messages: Vec<AgentMessage>`, `retained_tail: Vec<AgentMessage>`, `is_split_turn: bool`, `tokens_before: u64`, `previous_summary: Option<String>` (`#[builder(default)]`), `read_files: Vec<String>`, `modified_files: Vec<String>`, `settings: CompactionPolicy`; `SessionBeforeCompactEvent.preparation: CompactionPreparation`; `impl From<&context::CompactionPreparation> for protocol::CompactionPreparation` (`is_split_turn = !turn_prefix.is_empty()`), annotated `// ponytail: clones the partition once per compaction; borrow if extension events become hot`.

- [ ] **Step 1: Write failing tests** (register a `SessionBeforeCompactPoint` handler that records the event and returns default)

```rust
#[tokio::test]
async fn before_compact_event_carries_preparation_and_instructions() {
    // run two Turns, then "/compact keep names": recorded event has
    // custom_instructions == Some("keep names"), preparation.retained_tail non-empty,
    // preparation.messages_to_summarize contains the first user text,
    // preparation.settings == the kernel's resolved policy
}

#[tokio::test]
async fn extension_compaction_built_from_preparation_is_persisted() {
    // handler returns CompactionData { summary: "ext", retained_tail: event.preparation.retained_tail, .. };
    // session_messages() == [CompactionSummary("ext"), ..retained_tail]
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p kernel --test compaction before_compact extension_compaction_built`
Expected: FAIL.

- [ ] **Step 3: Implement** the protocol type, the internal `policy: CompactionPolicy` field filled by `from_history`, the `From` impl, and `.preparation((&preparation).into()).custom_instructions(instruction.clone())` in `compact_with_identity`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p protocol && cargo test -p extension && cargo test -p kernel`
Expected: PASS.

---

### Task 7: `session_compact_failed` observer point

**Files:**
- Modify: `crates/protocol/src/extension/events/session.rs` (event), `crates/protocol/src/lib.rs`
- Modify: `crates/extension/src/point.rs:62-82,247` (`define_point!`, names), `crates/extension/src/registry.rs:13-15,88-89`, `crates/extension/src/runtime/session.rs` (emitter), `extensions/src/available/hook_examples/session.rs` (observer)
- Modify: `crates/kernel/src/runtime/compaction.rs` (failure branch, extension cancel, manual `NoContextToCompact`), `crates/kernel/src/runtime/run/turn.rs:1052-1076` (overflow second failure)
- Test: `crates/extension/tests/runtime.rs:506`, `crates/kernel/tests/compaction.rs`

**Interfaces:**
- Produces: `protocol::SessionCompactFailedEvent { reason: CompactionReason, error_message: Option<String>, aborted: bool, will_retry: bool, from_extension: bool }` (typed-builder); `SessionCompactFailedPoint` named `"session_compact_failed"` with result `()`; `ExtensionRuntime::emit_session_compact_failed(&self, event: &SessionCompactFailedEvent, context: &ExtensionContext)` (observer, like `emit_session_compact`).

- [ ] **Step 1: Write failing tests**

In `public_point_catalog_contains_only_pi_non_ui_hooks`, insert `"session_compact_failed"` right after `"session_compact"`.

In `crates/kernel/tests/compaction.rs`, register a recording `SessionCompactFailedPoint` handler through `threshold_kernel_with` (manual cases call `kernel.compact_session` after a prepared Run); give `OverflowFixture` the same optional `StaticExtensionFactory` parameter for the overflow cases:

```rust
#[tokio::test] async fn failed_summary_reports_compact_failed() { /* aborted false, error_message Some, from_extension false */ }
#[tokio::test] async fn cancelled_summary_reports_aborted_compact_failed() { /* Err(ModelError::Cancelled): aborted true */ }
#[tokio::test] async fn extension_cancel_reports_aborted_compact_failed() { /* before_compact cancel: aborted true */ }
#[tokio::test] async fn manual_empty_compaction_reports_compact_failed() { /* compact_rejects_session_without_model_context setup: one event, reason Manual */ }
#[tokio::test] async fn automatic_empty_compaction_reports_nothing() { /* threshold_without_compactable_context setup: zero events */ }
#[tokio::test] async fn second_overflow_reports_recovery_failure_text() {
    // OverflowFixture [overflow, summary, overflow]: one event, reason Overflow, will_retry false,
    // error_message == "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model."
}
#[tokio::test] async fn second_truncation_reports_truncated_recovery_text() {
    // [MaxTokens with output below request budget, summary, same again]:
    // error_message == "Truncated response recovery failed after one compact-and-retry attempt."
}
#[tokio::test] async fn overflow_without_compactable_context_reports_nothing() { /* keep 1_000_000: zero events */ }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p extension --test runtime public_point_catalog && cargo test -p kernel --test compaction reports`
Expected: FAIL.

- [ ] **Step 3: Implement the point and emissions**

Wire the point exactly like `SessionCompactPoint`. In `compact_with_identity`: build `extension_context` before the `NoContextToCompact` check and emit there only for `Manual`; emit after the extension-cancel `warn` (`aborted: true`); in the failure branch after `lifecycle.fail`, emit with `aborted = matches!(error, KernelError::Model(ModelError::Cancelled))`, `error_message = (!aborted).then(|| error.to_string())`, and `from_extension`. In `next_action`: capture `let already_attempted = turn_loop.overflow_recovery_attempted;` before the recovery block; after it, when `settled.overflow_recovery == Some(CompactAndRetry) && already_attempted`, emit with the spec A.4 text chosen by `settled.stop_reason` (`Error` → overflow text, `MaxTokens` → truncation text) via `turn.extension_context`. No agent event is emitted for that case.

- [ ] **Step 4: Run tests**

Run: `cargo test -p extension && cargo test -p kernel && cargo test -p extensions`
Expected: PASS.

---

### Task 8: `BranchSummary` message in context, replay, provider, ACP

**Files:**
- Modify: `crates/protocol/src/message.rs:355-382,514-574,612` (struct, variant, exhaustive matches), `crates/protocol/src/model.rs:127-130`, `crates/protocol/src/identity.rs:73` (origin constant), `crates/protocol/src/lib.rs`
- Modify: `crates/kernel/src/provider.rs:591-596`, `crates/kernel/src/runtime/session/replay.rs:150-160,345-352`, `crates/kernel/src/runtime/plan/note.rs`, `crates/kernel/src/runtime/command/session_stats.rs`, `crates/kernel/src/runtime/compaction/context.rs` (`EstimatedTokens`, `CompactionBoundary`), `crates/kernel/src/runtime/compaction/prompt.rs` (render)
- Modify: `crates/acp/src/mapping.rs:257-262,795-806`
- Test: `crates/protocol/tests/messages.rs`, `crates/kernel/tests/compaction.rs`, `crates/kernel/tests/provider_runtime.rs`, `crates/acp/tests/mapping.rs`

**Interfaces:**
- Produces: `protocol::BranchSummaryMessage { entry_id: EntryId, from_id: String, summary: String, read_files: Vec<String>, modified_files: Vec<String> }` (typed-builder, camelCase, list fields `#[serde(default)] #[builder(default)]`); `MessageContent::BranchSummary { #[serde(flatten)] branch: BranchSummaryMessage }` (serde tag value `"branch_summary"`); `BranchSummaryMessage::model_text(&self) -> String` = `"The following is a summary of a branch that this conversation came back from:\n\n<summary>\n" + summary + "</summary>"`; `ProductIdentity::ACP_BRANCH_SUMMARY_ORIGIN = "branch_summary"`.
- Produces: kernel-local `pub(in crate::runtime) trait SummaryEntry { fn compaction_summary_message(&self) -> Result<AgentMessage, KernelError>; fn branch_summary_message(&self) -> Result<AgentMessage, KernelError>; }` implemented for `store::SessionEntry` in `replay.rs`. `compaction_summary_message` is the summary-message construction currently inlined in `history_from_store`'s `EntryKind::Compaction` arm (`replay.rs:284-330`), extracted unchanged; that arm keeps its retained-tail reset and `ModeNote` handling and calls the method. Task 9 reuses both methods.

- [ ] **Step 1: Write failing tests**

`protocol/tests/messages.rs`: `branch_summary_round_trips_and_wraps_model_text` — serialize, assert `"type": "branch_summary"`, `"fromId"`, round-trip equality, and exact `model_text()`.

`kernel/tests/compaction.rs` (kernel from `threshold_kernel_with` with a `SessionBeforeTreePoint` handler returning `TreeSummary` "ext branch summary" like `HostTreeSummary`; navigate with summary, then run a prompt):

```rust
#[tokio::test]
async fn branch_summary_enters_the_next_model_request() {
    // ScriptedModel receives AgentMessages before provider conversion, so assert the next request's
    // messages contain MessageContent::BranchSummary with summary "ext branch summary";
    // session_messages() contains it with message id and turn id "branch-summary-{entry_id}"
}
#[tokio::test]
async fn reopened_session_rebuilds_the_same_branch_summary_context() { /* close/reopen: session_messages equal */ }
#[tokio::test]
async fn branch_summary_in_retained_tail_is_not_duplicated_after_compaction() { /* navigate, run, compact, reopen: one BranchSummary */ }
#[tokio::test]
async fn legacy_branch_summary_entry_loads_with_empty_file_lists() {
    // append a BranchSummary entry with payload {"fromId":"root","summary":"old","details":{"source":"x"},"fromHook":true};
    // reopen succeeds, read_files and modified_files are empty (Review Focus 3)
}
```

`kernel/tests/provider_runtime.rs`: `branch_summary_reaches_provider_as_user_text` — copy `model_options_and_compaction_summary_reaches_provider`, set `request.messages[0].content` to a `BranchSummary` with summary `"branch body"`, and assert the serialized provider `messages` contain `"The following is a summary of a branch that this conversation came back from:"` and `"branch body"`.

`acp/tests/mapping.rs`: `branch_summary_maps_to_user_message_with_origin` — a `MessageEnd` carrying `BranchSummary` maps to `user_message` whose text is the summary and whose `_meta.clawcode.origin == "branch_summary"`.

Add to the Task 1 unit tests: a `BranchSummary` message `starts_turn()` and `is_cut_point()`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p protocol --test messages branch_summary && cargo test -p kernel --test compaction branch_summary legacy_branch && cargo test -p kernel --test provider_runtime branch_summary && cargo test -p acp --test mapping branch_summary`
Expected: FAIL (compile errors on the missing variant count as failure).

- [ ] **Step 3: Implement protocol and every exhaustive match**

`role()` → `None`; `text_content()` → `Some(summary.clone())`; image adaptation `continue`; provider boundary `Message::from(branch.model_text())`; `EstimatedTokens` = summary chars / 4 (ceil); `CompactionBoundary` true for both; `SummaryPrompt::render` emits `[User]: {model_text}`; plan note and session stats treat it like `CompactionSummary`.

- [ ] **Step 4: Implement replay**

Extract `compaction_summary_message` first and rerun `cargo test -p kernel --test compaction` to confirm the extraction changes nothing. `branch_summary_message`: read `summary` and `fromId` from the payload; take `details.readFiles`/`details.modifiedFiles` only when `fromHook` is not true and both are string arrays, else empty; ids `branch-summary-{entry_id}` for message and turn; all three timings from `entry.timestamp_ms`. `history_from_store` pushes it through `notes.push`; `session_replay` emits it as a `SessionReplayItem::Message` like message entries.

- [ ] **Step 5: Implement ACP mapping**

Message mapping (`:257`) builds a `wire::SessionUpdate::UserMessage` with the summary text (pattern of `user_bash_message`); metadata builder (`:795`) inserts `ACP_ORIGIN_METADATA = ACP_BRANCH_SUMMARY_ORIGIN`.

- [ ] **Step 6: Run tests**

Run: `cargo test -p protocol && cargo test -p kernel && cargo test -p acp`
Expected: PASS.

---

### Task 9: Branch summary generation and `[branch_summary]` config

**Files:**
- Modify: `crates/config/src/config.rs` (new `BranchSummaryConfig`, `AppConfig.branch_summary`), `crates/config/src/lib.rs`
- Modify: `crates/kernel/src/runtime.rs` (KernelFactory/Kernel field), `crates/app/src/lib.rs:203`
- Modify: `crates/kernel/src/runtime/session/tree.rs:16,95-106,193-280` (selection, prompt, payload)
- Modify: `crates/kernel/src/runtime/compaction/prompt.rs` (`SummaryProtocol::Branch`, `SummaryTemplate::Branch`, `BRANCH_SUMMARY_PROMPT`, preamble)
- Modify: `crates/kernel/src/runtime/compaction/context.rs` (`CompactionFileOperations` visibility and two methods), `crates/kernel/src/runtime/compaction.rs:477-491` (use the shared suffix)
- Modify: `README.md`, `README_zh.md`
- Test: `crates/config/tests/loading.rs`, `crates/kernel/src/runtime/session/tree.rs` (new `#[cfg(test)] mod tests`), `crates/kernel/tests/compaction.rs`

**Interfaces:**
- Consumes: Task 8 `BranchSummaryMessage` and `SummaryEntry`, Task 2 output budget and truncation.
- Produces: `config::BranchSummaryConfig { pub reserve_tokens: u64 }` (`Default` 16_384, serde default); Kernel builder field `branch_summary: BranchSummaryConfig` (`#[builder(default)]`); `SummaryProtocol::Branch { custom_instructions: Option<&'a str>, replace_instructions: bool }`; `CompactionFileOperations` becomes `pub(in crate::runtime)` with `fn extend_branch_summary_files(&mut self, branch: &BranchSummaryMessage)` and `fn summary_suffix(&self) -> String` (the `<read-files>`/`<modified-files>` text now built inline at `compaction.rs:482-491`); `TreeNavigationPlan::summary_input(&self, budget: u64) -> Result<(Vec<AgentMessage>, CompactionFileOperations), KernelError>` replacing `messages()`.

- [ ] **Step 1: Write failing tests**

`config/tests/loading.rs`: `branch_summary_reserve_defaults_to_pi_value` (16_384) and loads an explicit `[branch_summary] reserve_tokens = 4096`.

Selection unit tests in a new `tree.rs` `#[cfg(test)] mod tests`, building `TreeNavigationPlan::builder()` directly with hand-made `store::SessionEntry` values (message entries from serialized `AgentMessage`s, a Compaction entry from `CompactionData`, BranchSummary entries from JSON payloads):

```rust
#[test] fn summary_input_keeps_newest_messages_within_budget() {
    // entries [user "old"(40 chars), assistant(40), user "new"(8), assistant(8)], budget 8:
    // returned messages are the last two, in chronological order
}
#[test] fn zero_budget_means_unlimited() { /* budget 0 -> all four messages */ }
#[test] fn oversized_summary_entry_fits_under_ninety_percent() {
    // [branch summary entry with a 48-char summary (12 tokens), user 8 chars (2), assistant 8 chars (2)]:
    // budget 15 -> 4 + 12 > 15 but 4 < 13.5, so the summary is included (3 messages);
    // budget 4 -> 4 + 12 > 4 and 4 >= 3.6, so it is excluded (2 messages)
}
#[test] fn summary_input_skips_tool_results_and_invisible_messages() {
    // ToolResult, SlashCommand, hidden Extension entries are absent from the returned messages
}
#[test] fn summary_input_absorbs_nested_branch_summary_files() {
    // nested BranchSummary with details {readFiles:["a.rs"]} (fromHook false) plus a read call on input.txt:
    // returned file operations read_files() == ["a.rs", "input.txt"]; a fromHook=true summary adds nothing
}
```

Integration tests in `kernel/tests/compaction.rs` (navigate with `summarize = true` from a two-Turn branch to the first user entry):

```rust
#[tokio::test] async fn branch_summary_prompt_uses_pi_structure() {
    // request system text == SUMMARIZATION_SYSTEM_PROMPT; user text starts with "<conversation>\n"
    // and contains "Create a structured summary of this conversation branch"; persisted summary starts
    // with "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n"
}
#[tokio::test] async fn branch_summary_uses_configured_reserve() {
    // branch_summary.reserve_tokens = 99_990 (budget 10 on the 100k fixture window):
    // the oldest user text is absent from the prompt
}
#[tokio::test] async fn branch_summary_records_cumulative_files() {
    // abandoned branch calls read input.txt and write out.txt; nested summary details add a.rs:
    // payload details {readFiles:[a.rs,input.txt], modifiedFiles:[out.txt]} and summary text ends with the tags
}
#[tokio::test] async fn truncated_branch_summary_fails_navigation() {
    // MaxTokens -> navigation Err containing "Branch summarization failed:", leaf unchanged
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p config --test loading branch_summary && cargo test -p kernel --lib runtime::session::tree::tests && cargo test -p kernel --test compaction branch_summary truncated_branch`
Expected: FAIL.

- [ ] **Step 3: Implement config and kernel wiring**

Add `BranchSummaryConfig`, `AppConfig.branch_summary`, kernel builder fields, app wiring, README sections.

- [ ] **Step 4: Implement selection, prompt, and payload**

`summary_input`: first pass absorbs nested `BranchSummary` file lists; second pass walks entries newest→oldest converting Message entries (skip ToolResult and `estimated_tokens() == 0`), Compaction entries (`SummaryEntry::compaction_summary_message`), and BranchSummary entries (`SummaryEntry::branch_summary_message`), extracting Tool-call file operations before the budget check; when `budget > 0` and the next message would exceed it, include it only if it is a summary message and the running total is below 90% of the budget, then stop. Budget = `active_profile.context_tokens.saturating_sub(self.branch_summary.reserve_tokens)`. `SummaryTemplate::Branch` renders `<conversation>…</conversation>\n\n` + `BRANCH_SUMMARY_PROMPT` (spec A.2) with `Additional focus:` or full replacement when `replace_instructions`; system prompt `SUMMARIZATION_SYSTEM_PROMPT`. Stored summary = preamble + generated text + `summary_suffix()`; payload `details` = `{readFiles, modifiedFiles}` for generated summaries (extension `details` unchanged) and `usage` = generated usage. Empty selection writes no entry. Delete `BRANCH_SUMMARY_INSTRUCTION`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p config && cargo test -p kernel && cargo test -p app`
Expected: PASS.

---

### Task 10: Client-requested branch summaries

**Files:**
- Modify: `crates/protocol/src/acp.rs:22-29`
- Modify: `crates/kernel/src/runtime/session/tree.rs:109-170,170-190,205-218` (request type, event instructions, default instructions)
- Modify: `crates/kernel/src/runtime/extension/host.rs:756-772`, `crates/acp/src/extension.rs:171-204`
- Test: `crates/protocol/tests/` (ACP params), `crates/kernel/tests/compaction.rs`, `crates/acp/tests/extension.rs`

**Interfaces:**
- Produces: `AcpNavigateParameters { session_id, entry_id: Option<EntryId>, #[serde(default)] summarize: bool, #[serde(default)] custom_instructions: Option<String> }`; `kernel::TreeNavigationRequest { pub target: EntryId, pub summarize: bool, pub custom_instructions: Option<String> }` (exported); `Kernel::navigate_session_with_summary(&self, session_id: &SessionId, request: TreeNavigationRequest) -> Result<SessionTreeSnapshot, KernelError>`.

- [ ] **Step 1: Write failing tests**

```rust
// protocol: navigate_parameters_default_to_no_summary — {"sessionId","entryId"} deserializes with summarize false, custom_instructions None.

// kernel/tests/compaction.rs
#[tokio::test] async fn navigation_instructions_reach_hook_and_prompt() {
    // TreeNavigationRequest { summarize: true, custom_instructions: Some("focus api") }:
    // SessionBeforeTreeEvent.custom_instructions == Some("focus api"); prompt contains "Additional focus: focus api"
}
#[tokio::test] async fn summarizing_navigation_to_a_descendant_writes_no_summary() {
    // navigate without summary back to the first assistant entry, then navigate with
    // summarize: true to the original leaf (a descendant, so nothing is abandoned):
    // Ok, no "branch_summary" entry and no summary request (Review Focus 4)
}

// acp/tests/extension.rs
// navigate_with_summary_writes_branch_summary — ACP navigate {summarize: true, customInstructions: "x"} creates a branch_summary entry
// root_navigation_with_summary_is_invalid_params — entryId null + summarize true -> invalid params error
```

Update the existing callers of the old signature to pass a `TreeNavigationRequest`: `branch_summary_opens_a_top_level_run_span` (`kernel/tests/compaction.rs:2420`), `kernel/tests/extensions_host.rs:1161`, `extension/host.rs:765`, and `session/tree.rs:118`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p protocol navigate_parameters && cargo test -p kernel --test compaction navigation_instructions summarizing_navigation && cargo test -p acp --test extension navigate_with_summary root_navigation`
Expected: FAIL.

- [ ] **Step 3: Implement**

`navigate_session` keeps calling with `summarize: false, custom_instructions: None`. The tree event uses `.custom_instructions(request.custom_instructions.clone())`; generation uses the hook's instructions when present, otherwise the request's. Extension host passes `None`. ACP Navigate and Branch: `entry_id == None && summarize` → `invalid_parameters`; `Some(target)` → `navigate_session_with_summary`; `None` → `navigate_session(&session_id, None)`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p protocol && cargo test -p kernel && cargo test -p acp`
Expected: PASS.

---

### Task 11: WebUI branch summary card and navigation choice

**Files:**
- Modify: `web/src/domain/model.ts` (entity, row variant, row key), `web/src/workspace/updateDecoder.ts:121`, `web/src/workspace/sessionState.ts` (upsert action), `web/src/features/conversation/Conversation.tsx` (row render)
- Create: `web/src/features/conversation/BranchSummaryCard.tsx`
- Modify: `web/src/theme/workbench.css` (card styles following the compaction card), `web/src/features/inspector/SessionTree.tsx`, `web/src/workspace/controller.ts:338-352`

**Interfaces:**
- Consumes: ACP `user_message` with `_meta.clawcode.origin === "branch_summary"` (Task 8); ACP navigate params `summarize`, `customInstructions` (Task 10).
- Produces: `BranchSummaryEntity = Readonly<{ messageId: MessageId; summary: string; timestampMs: TimestampMs }>`; transcript row `{ type: "branchSummary"; messageId: MessageId; order: EventOrder }` with key `branchSummary:${messageId}`; action `branch-summary/upserted`; `WorkspaceController.navigate(entryId, options?: { summarize: boolean; customInstructions?: string })` and the same for `branch`.

- [ ] **Step 1: Implement decoding and the card**

The decoder handles the `branch_summary` origin before the generic `user_message` branch and dispatches `branch-summary/upserted` instead of a user message. `BranchSummaryCard` is a collapsed `<details>` titled "分支摘要" rendering the summary with the existing markdown renderer.

- [ ] **Step 2: Implement the navigation choice**

In `SessionTree`, Navigate and Branch open an inline panel when `target !== tree.leafId`, with "不生成摘要", "生成摘要", and "带指令生成摘要" (adds a textarea); confirming calls the controller with `{ summarize, customInstructions }`. Targets equal to the leaf navigate directly. The existing `busy` status covers summary generation.

- [ ] **Step 3: Static checks**

Run: `npm --prefix web run check && npm --prefix web run build`
Expected: no TypeScript or ESLint errors.

- [ ] **Step 4: Acceptance against the production backend**

Start the production `app` backend with its configured provider, open the WebUI, run two prompts, navigate to the first user entry with "带指令生成摘要", and confirm: the branch summary card appears at the new position after reload, collapsing/expanding works, and the next prompt's answer reflects the summary. Repeat with "不生成摘要" and confirm no card appears.

---

### Task 12: Final verification

**Files:** none new.

- [ ] **Step 1: Full test suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 2: Pre-commit hooks**

Run: `pre-commit run --all-files`
Expected: fmt, clippy (`--tests --examples -- -Dwarnings`), and rig-boundary pass.

- [ ] **Step 3: Spec coverage spot check**

Confirm each spec section maps to a passing test listed above; confirm `README.md`/`README_zh.md` document `model_overrides` and `[branch_summary]`.

- [ ] **Step 4: Hand off for commit**

Report results to the user and wait. Commit only on explicit request: one commit on `main` containing code, the spec, and this plan, message following `.gitmessage`.

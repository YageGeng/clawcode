# ACP 5.0 Upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade to `agent-client-protocol` / `agent-client-protocol-http` 5.0, keep per-frame batch commits through a negotiated `_meta` carrier instead of JSON-RPC batches, enable h2c, and replace `_clawcode/session/rename` with standard `session/set_title`.

**Architecture:** Task 1 gets the workspace compiling on 5.0 with the batching relay degraded to single frames. Task 2 restores batching as a private, negotiated `session/update` carrier inside `BatchComponent`; Task 3 teaches the WebUI to declare and expand it. Tasks 4–5 add `Kernel::set_session_title` and the ACP `session/set_title` handler, then delete the private rename. Task 6 turns on axum `http2`. Task 7 is acceptance.

**Tech Stack:** Rust 2024 workspace, agent-client-protocol 5.0 (schema 1.15.1, `unstable_protocol_v2`), axum 0.8, tokio, React/TypeScript WebUI.

**Spec:** `docs/superpowers/specs/2026-11-02-acp-5-upgrade-design.md`

## Global Constraints

- Versions: `agent-client-protocol = "5"`, `agent-client-protocol-http = { version = "5", default-features = false }`, `axum = { version = "0.8", features = ["http2"] }` in root `[workspace.dependencies]`; sub-crates inherit with `{ workspace = true }` and add only local features.
- Product namespace comes from `protocol::ProductIdentity::ACP_NAMESPACE` (`"clawcode"`); never hard-code it in Rust.
- Capability: `initialize` request `_meta.clawcode.sessionUpdateBatch = { "version": 1 }`; carrier key `_meta.clawcode.sessionUpdateBatch` holds an array of complete standard `session/update` params; only version `1` enables batching.
- `[app.recovery] max_batch_size` keeps its meaning (outer + nested ≤ limit); `claw.toml` is unchanged.
- All new/modified code: English comments, a function-level comment on every new function; `tracing::*!` fully qualified with formatted messages; `Arc::clone(&x)`; structs with >3 fields use `typed-builder`.
- Operations that share context are methods on a struct (CLAUDE.md rule 15); test code lives only in `tests/` or `#[cfg(test)] mod tests`.
- Frontend changes need no unit tests; verify with `npm run check` in `web/`.
- **Commits:** never commit without the user's explicit permission. Before each commit run `pre-commit run --all-files` and use the `.gitmessage` format (`<type>(<scope>): <summary>`, title only).

## Review Focus

1. The outer update already carries other `_meta.clawcode` keys (e.g. `sessionRecovery`); packing must keep them next to the batch key. Test: Task 2 `carrier_preserves_existing_product_meta`.
2. `max_batch_size = 1` must never form a carrier. Test: Task 2 `batch_limit_of_one_never_packs`.
3. A client that sends `initialize` with no `_meta` at all (Zed) gets standard frames. Test: Task 2 `initialize_meta_toggles_batching`.
4. A title longer than 120 characters is rejected with `-32602`, not stored truncated. Test: Task 5 `session_set_title_rejects_invalid_titles`.
5. `session/set_title` for an unknown session returns a JSON-RPC error and the connection keeps serving requests. Test: Task 5 `session_set_title_unknown_session_keeps_connection`.

---

### Task 1: Compile on ACP 5.0

**Files:**
- Modify: `Cargo.toml:128-129`, `Cargo.lock`
- Modify: `crates/acp/src/{config,content,input,mapping,projection,recovery,terminal,tool_call,elicitation,coalescing,server,extension,trace,batching}.rs`
- Modify: `crates/acp/tests/{elicitation,v2_alignment,mapping,extension,disconnect,tracing}.rs` (paths only)

**Interfaces:**
- Produces: the crate compiles on 5.0; `forward_outbound` emits only `TransportFrame::Single` (coalesced) — Task 2 replaces it.

- [ ] **Step 1: Bump versions** to `"5"` in root `Cargo.toml`; run `cargo update -p agent-client-protocol -p agent-client-protocol-http`.
- [ ] **Step 2: Apply the renames** from spec §2.2: `agent_client_protocol::schema::v2` → `agent_client_protocol::v2::schema` (keep `schema::ProtocolVersion`), `V2ConnectionTo` → `v2::ConnectionTo` (import as `v2::ConnectionTo as V2ConnectionTo` to keep call sites), `Responder<T>` → `agent_client_protocol::v2::Responder<T>` in `server.rs`, `trace.rs`, `tests/elicitation.rs`.
- [ ] **Step 3: Adapt `extension.rs` trait impls** to `fn parse_message(method: &str, params: &serde_json::Value) -> Result<Self, agent_client_protocol::Error>` and `fn from_json(method: &str, value: serde_json::Value) -> Result<Self, agent_client_protocol::Error>` (replaces `from_value`).
- [ ] **Step 4: Degrade `batching.rs`**: destructure `Channel { rx, tx, .. }`; after `coalesce_updates`, send each message as `TransportFrame::Single` in order; delete `TransportBatch` imports. Rewrite tests that matched `TransportFrame::Batch` to expect the equivalent sequence of single frames (`batches_queued_session_updates` → `queued_session_updates_stay_in_order`; same for `splits_session_updates_at_batch_limit`, `preserves_order_across_non_session_frames`).
- [ ] **Step 5: Audit dropped responders (#462)**: in every `on_receive_request` closure in `server.rs`, `extension.rs`, `elicitation.rs`, confirm each path either calls `respond`, returns `Err`, or moves the responder into a task that responds. Note any intentionally unanswered path in the task report; none is expected.
- [ ] **Step 6: Verify**

Run: `cargo test -p acp && cargo test -p app --test logging && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all pass. `logging.rs` passing confirms the `agent_client_protocol::jsonrpc::incoming_actor` target is unchanged.

- [ ] **Step 7: Commit** (with permission): `build(acp): upgrade agent-client-protocol to 5.0`

---

### Task 2: Negotiated `_meta` batch carrier

**Files:**
- Modify: `crates/acp/src/batching.rs` (relay functions become structs; tests in its `mod tests`)

**Interfaces:**
- Consumes: Task 1 single-frame relay.
- Produces (crate-private, same file):

```rust
/// Records whether this connection's client accepts batched Session updates.
struct InboundRelay { accepts_batches: Arc<AtomicBool> }
impl InboundRelay {
    async fn forward(self, source: mpsc::UnboundedReceiver<TransportFrame>, sink: mpsc::UnboundedSender<TransportFrame>) -> Result<(), agent_client_protocol::Error>;
    /// Re-evaluates the capability from one `initialize` request; other frames are ignored.
    fn observe(&self, frame: &TransportFrame);
}
/// Drains queued same-Session updates into one private carrier when negotiated.
struct OutboundRelay { max_batch_size: NonZeroUsize, accepts_batches: Arc<AtomicBool> }
impl OutboundRelay {
    async fn forward(self, source: mpsc::UnboundedReceiver<TransportFrame>, sink: mpsc::UnboundedSender<TransportFrame>) -> Result<(), agent_client_protocol::Error>;
    /// Turns one coalesced same-Session run into a carrier or single frames.
    fn frames(&self, messages: Vec<RawJsonRpcMessage>) -> Vec<TransportFrame>;
}
/// Returns the Session id of a standard `session/update` notification.
fn session_update_id(message: &RawJsonRpcMessage) -> Option<&str>;
const BATCH_KEY: &str = "sessionUpdateBatch";
```

`into_channel_and_future` creates one `Arc<AtomicBool>` (initially `false`) and hands `Arc::clone` to each relay.

- [ ] **Step 1: Write the failing tests** in `batching.rs` `mod tests`. Add a helper `update(session: &str, index: usize) -> TransportFrame` (a `tool_call_update` with `toolCallId = format!("tool-{index}")`, so coalescing leaves it alone) and `relay(frames, max_batch_size, accepts: bool)`. Tests and their assertions:
  - `unnegotiated_updates_stay_single_frames`: 3 × `update("s1", i)`, `accepts = false` → 3 single frames, none has `params._meta.clawcode.sessionUpdateBatch`.
  - `negotiated_updates_share_one_carrier`: same input, `accepts = true` → 1 frame; outer `toolCallId == "tool-1"`; batch array has 2 items with `toolCallId` `tool-2`, `tool-3`, each `sessionId == "s1"`.
  - `carrier_respects_batch_limit`: 5 updates, limit 2 → carrier(1,2), carrier(3,4), single(5) without batch key.
  - `batch_limit_of_one_never_packs`: 3 updates, limit 1, `accepts = true` → 3 single frames.
  - `session_change_ends_carrier`: s1,s1,s2,s1 → carrier(s1:1,2), single(s2:3), single(s1:4).
  - `non_update_frame_ends_carrier`: s1,s1,`notification("custom/event", 0)`,s1 → carrier(1,2), the custom frame, single(4).
  - `coalescing_runs_before_packing`: `stream_chunk(41, "Hel")`, `stream_chunk(42, "lo")` with `accepts = true` → 1 single frame, text `"Hello"`, no batch key.
  - `carrier_preserves_existing_product_meta`: updates carrying `_meta.clawcode.sessionRecovery = {"projectionIndex": i}` → outer keeps index 0 next to the batch key; nested items keep index 1 and 2.
  - `non_object_product_meta_falls_back_to_single_frames`: first update `_meta.clawcode = "x"` → all single frames.
  - `initialize_meta_toggles_batching`: `InboundRelay::observe` on `initialize` with `{"_meta":{"clawcode":{"sessionUpdateBatch":{"version":1}}}}` → flag `true`; then `initialize` with no `_meta` → `false`; `version: 2` → `false`; `sessionUpdateBatch: "yes"` → `false`.
  - `inbound_frames_pass_through_unchanged` (extend the existing test): a `TransportFrame::Malformed` frame and a normal request arrive unchanged and in order.
  - `component_batches_only_after_negotiated_initialize`: wrap one end of `Channel::duplex()` in `BatchComponent::new(inner, 8)`, drive `into_channel_and_future`; send a negotiated `initialize` from the outer side, read it on the inner side, then queue 3 updates on the inner side before reading the outer side → exactly one carrier. Repeat without the capability → 3 single frames.
- [ ] **Step 2: Run** `cargo test -p acp batching` → new tests FAIL (carrier not produced / functions missing).
- [ ] **Step 3: Implement the relays.** Drain rule (unchanged `now_or_never`, no timers): stop at `max_batch_size`, at a non-update frame, at a different `session_update_id`, or when the source closes; the stopping frame starts the next round. `frames` runs `coalesce_updates` first, then packs only when `accepts_batches.load(Ordering::Acquire)` and more than one message remains. `observe` stores with `Ordering::Release` and runs before the frame is forwarded. Packing (the only non-obvious algorithm):

```rust
// Outer = first message; nested = remaining params objects, in order.
// _meta and _meta.<ACP_NAMESPACE> are created when absent; if either exists
// but is not an object, return every message as a single frame instead.
product.insert(BATCH_KEY.to_string(), serde_json::Value::Array(rest_params));
```

- [ ] **Step 4: Run** `cargo test -p acp batching` → PASS; then `cargo test -p acp` → PASS.
- [ ] **Step 5: Commit** (with permission): `feat(acp): batch session updates through negotiated _meta carriers`

---

### Task 3: WebUI declares and expands carriers

**Files:**
- Modify: `web/src/acp/connection.ts:172-196` (`receive`)
- Modify: `web/src/acp/protocol.ts` (new `AcpProtocol` method)
- Modify: `web/src/workspace/controller.ts:584-592` (`initialize`), `:859-893` (`applyNotifications`)

**Interfaces:**
- Consumes: Task 2 wire format.
- Produces: `AcpProtocol.expandSessionUpdateBatch(update: SessionUpdateNotification, namespace: string): readonly SessionUpdateNotification[]`.

- [ ] **Step 1: `connection.ts`**: when the parsed frame is an array, call `this.callbacks.diagnostic("Ignored JSON-RPC batch frame; ACP v2 forbids batches")` and return; otherwise decode the single record (drop the "batch member" labels and the empty-batch error).
- [ ] **Step 2: `protocol.ts`**: implement `expandSessionUpdateBatch` per spec §4.4. Return `[update]` when `productMeta(update._meta, namespace)?.sessionUpdateBatch` is absent; otherwise validate (non-empty array, every item a record with `sessionId === update.sessionId` and no nested batch key) and return `[outer without the key, ...items]`. Throw `new Error("ACP session update batch is invalid")` on any violation.
- [ ] **Step 3: `controller.ts`**: in `applyNotifications`, replace the single push with a loop over `AcpProtocol.expandSessionUpdateBatch(update, this.bootstrap.product.slug)` (keep the `missingSessionIds` filter per item). Errors propagate to the existing catch in the `notifications` callback (close + reconnect). In `initialize`, add `sessionUpdateBatch: { version: 1 }` beside `sessionRecovery`.
- [ ] **Step 4: Verify** — Run: `cd web && npm run check` → exit 0, no ESLint warnings.
- [ ] **Step 5: Commit** (with permission): `feat(web): expand negotiated session update batches`

---

### Task 4: `Kernel::set_session_title`

**Files:**
- Modify: `crates/kernel/src/runtime/session.rs` (next to `rename_session`, line ~562)
- Create: `crates/kernel/tests/protocol_alignment/title.rs`; Modify: `crates/kernel/tests/protocol_alignment/main.rs` (`mod title;`)
- Modify (test fixture only, if needed): `crates/kernel/tests/subagent/fixture.rs` — add `RecordingSinks::events(&self, session: &SessionId) -> Vec<AgentEvent>`

**Interfaces:**
- Produces: `pub async fn set_session_title(&self, session_id: &SessionId, title: SessionTitle) -> Result<(), KernelError>` on `Kernel`.

- [ ] **Step 1: Write the failing tests** in `title.rs`, using `TestKernel`:
  - `set_session_title_emits_one_event_when_idle`: create a session; `rename_session(.., "Quiet")` → `payloads(&session)` has no `SessionTitleChanged`; `set_session_title(.., "Manual")` → exactly one `SessionTitleChanged { title: "Manual" }`.
  - `set_session_title_uses_active_turn_while_running`: `TestKernel::new(|_| Route::Hang)`, start a run with input `"hold"` (pattern: `queue.rs::admission_order_and_stop_preserve_each_input_outcome`), wait until its `AgentEventPayload::TurnStart` is recorded, then `set_session_title(.., "Live")` → exactly one `SessionTitleChanged`, and its `metadata.turn_id` equals the running turn's id.
  - `set_title_is_not_replaced_by_default_title`: `set_session_title(.., "Manual")`, then run input `"First line"` → `session_tree(..).name == Some("Manual")` and no `SessionTitleChanged { title: "First line" }`.
- [ ] **Step 2: Run** `cargo test -p kernel --test protocol_alignment title` → FAIL (`set_session_title` not found).
- [ ] **Step 3: Implement** `set_session_title`: call `rename_session`, then emit `AgentEventPayload::SessionTitleChanged { title }` through `EventEmitter { clock, sink, session }`. Choose the turn id and sink exactly as `queue.rs:379-391` does: the active run's `turn_id` and sink when present, otherwise a fresh `TurnId` from `id_generator` (`IdKind::Turn`) and `self.session_sink(session_id)`. Leave `rename_session` and `/name` (`builtin.rs`) unchanged.
- [ ] **Step 4: Run** `cargo test -p kernel` → PASS (`lifecycle_order.rs` still asserts `["set_name", "sync"]`).
- [ ] **Step 5: Commit** (with permission): `feat(kernel): emit title change when a client sets a session title`

---

### Task 5: Standard `session/set_title`; remove private rename

**Files:**
- Modify: `crates/acp/Cargo.toml:39` (add feature `unstable_session_set_title`)
- Modify: `crates/acp/src/server.rs:879` (capability), handler chain (new handler beside `DeleteSessionRequest`, ~1352), `:832` (remove from advertised list)
- Modify: `crates/acp/src/extension.rs:13,325-338`; `crates/protocol/src/identity.rs:144,194,221`; `crates/protocol/src/acp.rs:104`; `crates/protocol/src/lib.rs:24`; `crates/protocol/tests/identity.rs:16,51-52`
- Modify: `web/src/acp/protocol.ts` (`methods.sessionSetTitle`), `web/src/acp/extensions.ts:11,37`, `web/src/workspace/controller.ts:150-156`
- Test: `crates/acp/tests/extension.rs`, `crates/acp/tests/v2_alignment.rs`

**Interfaces:**
- Consumes: Task 4 `Kernel::set_session_title`.
- Produces: wire method `session/set_title` (`wire::SetSessionTitleRequest` → `wire::SetSessionTitleResponse`), capability `capabilities.session.title = {}`.

- [ ] **Step 1: Write the failing tests.**
  - `extension.rs` `native_session_set_title_is_advertised_and_normalized` (template: `native_session_delete_is_advertised_and_idempotent`): `initialized.capabilities.session.and_then(|s| s.title).is_some()`; create a session; `session/set_title` with `"  Renamed  "` → response `title == "Renamed"`; `session/list` shows `"Renamed"`.
  - `extension.rs` `session_set_title_rejects_invalid_titles`: `"   "` and a 121-character title each → error code `-32602`; the stored title is unchanged.
  - `extension.rs` `session_set_title_unknown_session_keeps_connection`: `sessionId: "missing"` → JSON-RPC error; a following `session/list` on the same connection succeeds.
  - `v2_alignment.rs` `set_title_publishes_session_info_update` (via `with_session`): send `wire::SetSessionTitleRequest::new(session_id, "Renamed")`, then `next_matching(updates, |v| v["sessionUpdate"] == "session_info_update" && v["title"] == "Renamed")`.
  - `crates/protocol/tests/identity.rs`: delete the `SessionRename` assertions.
- [ ] **Step 2: Run** `cargo test -p acp --test extension --test v2_alignment` → new tests FAIL (method not found / capability missing).
- [ ] **Step 3: Implement**: enable the feature; add `.title(wire::SessionTitleCapabilities::new())` to the `SessionCapabilities` chain; register the handler following the `DeleteSessionRequest` handler: `SessionId::try_from` and `SessionTitle::try_from(request.title)` map failures to `Error::invalid_params().data(error.to_string())`; `kernel.set_session_title(...).await.map_err(KernelFailure)?`; respond `wire::SetSessionTitleResponse::new(title.as_str())`. Delete every private-rename item listed in **Files**.
- [ ] **Step 4: WebUI**: add `sessionSetTitle: "session/set_title"` to `AcpProtocol.methods`; `renameSession` requests it with `{ sessionId, title }` and dispatches `result.title`; remove `sessionRename` from `extensions.ts`.
- [ ] **Step 5: Verify** — Run: `cargo test -p acp -p protocol && (cd web && npm run check)` → PASS; `grep -rn "session/rename\|SessionRename" crates web/src` → no matches.
- [ ] **Step 6: Commit** (with permission): `feat(acp): adopt standard session/set_title`

---

### Task 6: h2c

**Files:**
- Modify: `Cargo.toml:99` (axum features), `crates/app/Cargo.toml` dev-dependencies `# http` group (add `reqwest = { workspace = true, features = ["http2"] }` before `tower`)
- Test: `crates/app/tests/web.rs`
- Modify: `README.md:360-362`, `README_zh.md:321-322`

**Interfaces:** none.

- [ ] **Step 1: Write the failing test** `health_endpoint_accepts_h2c_prior_knowledge`: bind `tokio::net::TcpListener` on `127.0.0.1:0`, spawn `axum::serve(listener, WebFixture::missing_dist().router.clone())`, request `/health` with `reqwest::Client::builder().http2_prior_knowledge()` → `status == 200` and `version() == reqwest::Version::HTTP_2`.
- [ ] **Step 2: Run** `cargo test -p app --test web health_endpoint_accepts_h2c_prior_knowledge` → FAIL (h2 preface rejected).
- [ ] **Step 3: Enable** `features = ["http2"]` on root axum; `main.rs` stays unchanged.
- [ ] **Step 4: Run** `cargo test -p app` → PASS.
- [ ] **Step 5: Docs**: in both READMEs' HTTP transport paragraph, add one sentence: the server accepts HTTP/1.1 and h2c (prior knowledge); WebSocket upgrades require HTTP/1.1.
- [ ] **Step 6: Commit** (with permission): `feat(app): accept h2c on the HTTP transport`

---

### Task 7: Acceptance

**Files:** none (results go in the task report).

- [ ] **Step 1: Full suite** — Run: `cargo test --workspace && pre-commit run --all-files && (cd web && npm run check && npm run build)` → all pass.
- [ ] **Step 2: Protocol smoke** against the production `app` binary: `curl -s --http2-prior-knowledge -o /dev/null -w '%{http_version}\n' http://127.0.0.1:3000/health` prints `2`; the WebUI connects over WebSocket on the same port.
- [ ] **Step 3: WebUI e2e** on the production `app` backend with its configured provider (spec §7.2, items 1–5): streaming first token, incremental and full replay (session with >512 updates, no Inspector diagnostics), parallel sessions, rename in one tab seen in a second tab and after reload, permission/elicitation confirm and cancel.
- [ ] **Step 4: Performance comparison** (spec §7.3): full replay of the same large session on `main` and on this branch; record WebSocket frame count and time to final render. Expected: same order of magnitude, no visible regression.

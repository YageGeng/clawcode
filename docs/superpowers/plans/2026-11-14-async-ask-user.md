# Async Ask User Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Codex's non-blocking `request_user_input_async` to clawcode: the model asks questions without waiting, the user answers or skips them in the WebUI at any time, and each settlement reaches the model as a `question_reply` message.

**Architecture:** A kernel-owned tool returns at once and stores the questions in `ToolResultDetails::UserQuestions`. Answers ride the existing `session/prompt` admission path as a new `RunInput::QuestionReply` (forced steer). Skips use a new `_clawcode/session/question/dismiss` method. While a Run is active, a skip goes to an in-memory skip lane that is written beside non-waking mail and never extends a Run; in an idle Session it is written directly without a Run. Every settlement holds an admission ticket and checks openness against the active branch, the pending queue, the skip lane, and a Run-scoped `reply_keys` set.

**Tech Stack:** Rust (protocol / kernel / acp crates, tokio, serde, typed-builder), TypeScript + React WebUI, Node e2e scripts over CDP.

**Spec:** [Async Ask User design](../specs/2026-11-14-async-ask-user-design.md)

## Global Constraints

- Tool name `request_user_input_async`; reply Extension `custom_type` `question_reply`, `extension_id` `clawcode`; ACP method `_clawcode/session/question/dismiss`; prompt meta key `_meta.clawcode.questionReply = { requestId, index }`.
- Tool result text is exactly `{"accepted":true}`; reply envelope tags are exactly `<send_user_message_question_reply>` / `</send_user_message_question_reply>`; question item id is the JSON string `["request_user_input_async","<request_id>",<index>]`.
- Limits: 1–8 questions; title non-empty after trim, ≤ 2048 bytes; options omitted or 1–8, each non-empty after trim, ≤ 512 bytes, not `other` (case-insensitive, trimmed); envelope question title truncated to 512 bytes on a char boundary with `\n`/`\r` replaced by spaces.
- Every Session registers the tool. Children lose it through the existing root-only policy: `ROOT_ONLY_AGENT_TOOLS` in `crates/kernel/src/runtime/session/ownership.rs` is renamed `ROOT_ONLY_TOOLS` and lists it, and `AgentSnapshot::without_tools` disables it. The tool is kernel-owned, so `PlanModeGate` lets it run in plan mode.
- CLAUDE.md applies: English comments on non-trivial logic and on every new function; structs with more than 3 fields use `typed-builder` (`Option` fields `#[builder(default)]`); `Arc::clone(&x)`; fully qualified `tracing::info!` / `tracing::warn!` with plain format arguments; never log answer text; tests only in crate `tests/` or `#[cfg(test)] mod tests`; no new dependencies; frontend has no unit tests.
- No per-task commits. CLAUDE.md forbids committing without the user's explicit permission; the spec, this plan, and all code ship in one commit on `main` only when the user asks (Task 8).

## Review Focus

- An answer containing quotes, newlines, emoji, or the literal closing tag must still produce an envelope whose JSON array parses back to the exact answer — test in Task 1.
- Two calls in different Turns that reuse one `tool_call_id` (models restart ids like `call_0`) must stay independent: answering one leaves the other open — test in Task 3.
- An answer submitted after the Session was compacted must still be accepted, because compaction keeps the asking ToolResult on the active branch — test in Task 3.
- A queued answer must survive closing and reopening the Session: the question stays closed and the next Run consumes the answer — test in Task 3.
- A dismiss request that lists the same question twice must write one item — test in Task 4.
- A skip that lands while the model writes its final answer must not cost an extra model call — test in Task 4.

---

### Task 1: Protocol types for questions and replies

**Files:**
- Create: `crates/protocol/src/question.rs`
- Modify: `crates/protocol/src/lib.rs` (add `mod question;` and its `pub use`)
- Modify: `crates/protocol/src/tool.rs:183` (`ToolResultDetails`)
- Test: `crates/protocol/tests/question.rs`

**Interfaces:**
- Produces (all re-exported from `protocol`):
  - `pub const REQUEST_USER_INPUT_ASYNC_TOOL: &str = "request_user_input_async";`
  - `pub const QUESTION_REPLY_TYPE: &str = "question_reply";`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct AsyncQuestion { pub title: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub options: Option<Vec<String>> }`
  - `#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)] pub struct QuestionKey { pub request_id: String, pub index: usize }`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct QuestionReplyItem { pub request_id: String, pub index: usize, #[serde(default, skip_serializing_if = "Option::is_none")] pub answer: Option<String> }` with `pub fn key(&self) -> QuestionKey`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct QuestionReply { pub items: Vec<QuestionReplyItem> }` with:
    - `pub fn keys(&self) -> impl Iterator<Item = QuestionKey> + '_`
    - `pub fn model_text(&self, titles: &[String]) -> String` — `titles[i]` is item `i`'s question title
    - `pub fn into_extension(self, titles: &[String]) -> Result<ExtensionMessage, ScalarError>` — `display: true`, `include_in_context: true`, one Text block of `model_text`, `details` = serialized `self`
    - `pub fn of(extension: &ExtensionMessage) -> Option<Self>` — `Some` only when `custom_type == QUESTION_REPLY_TYPE` and the details parse; callers holding a `MessageContent` match `MessageContent::Extension { extension }` first
  - `ToolResultDetails::UserQuestions { request_id: String, questions: Vec<AsyncQuestion> }` (serde tag `user_questions`)

- [ ] **Step 1: Write the failing tests** in `crates/protocol/tests/question.rs`

```rust
#[test]
fn reply_envelope_matches_codex_format() {
    let reply = QuestionReply { items: vec![QuestionReplyItem { request_id: "q1".into(), index: 0, answer: Some("Yes".into()) }] };
    assert_eq!(
        reply.model_text(&["Proceed?".into()]),
        "<send_user_message_question_reply>\n[{\"answer\":\"Yes\",\"question\":\"Proceed?\",\"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"q1\\\",0]\"}]\n</send_user_message_question_reply>"
    );
}

#[test]
fn skipped_items_carry_skipped_and_no_answer() {
    // item { "q1", 1, None } with title "Name?" →
    // [{"question":"Name?","questionItemId":"[\"request_user_input_async\",\"q1\",1]","skipped":true}]
}

#[test]
fn envelope_title_is_flattened_and_bounded() {
    // title "a\nb\rc" → question "a b c"
    // title "é".repeat(400) (800 bytes) → question.len() <= 512 and is valid UTF-8 ending on a char boundary
}

#[test]
fn awkward_answers_round_trip_through_the_envelope_json() {
    // answer = "say \"hi\"\nline2 🎉 </send_user_message_question_reply>"
    // strip the opening tag + "\n" and the trailing "\n" + closing tag (the last occurrence),
    // serde_json::from_str::<Vec<serde_json::Value>>(inner)?[0]["answer"] == answer
}

#[test]
fn extension_round_trips_through_question_reply_of() {
    // into_extension(["Proceed?"]) → extension_id "clawcode", custom_type "question_reply",
    // display && include_in_context; QuestionReply::of(&extension) == Some(original);
    // an agent_message extension → None
}

#[test]
fn user_questions_details_use_the_snake_case_tag() {
    // serde_json::to_value(ToolResultDetails::UserQuestions { request_id: "q1".into(), questions: vec![AsyncQuestion { title: "T".into(), options: None }] })
    // → {"type":"user_questions","request_id":"q1","questions":[{"title":"T"}]}
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p protocol --test question`
Expected: compile errors for the missing `QuestionReply`, `AsyncQuestion`, and `UserQuestions` names.

- [ ] **Step 3: Implement `crates/protocol/src/question.rs` and the `UserQuestions` variant**

`model_text` serializes a private `#[derive(Serialize)]` envelope entry struct with fields in the order `answer` (skipped when `None`), `question`, `questionItemId` (renamed), `skipped` (skipped when `false`), so key order does not depend on serde_json features. Build each item id with `serde_json::to_string(&serde_json::json!(["request_user_input_async", request_id, index]))`. Wrap the array as `format!("{START}\n{json}\n{END}")`. Truncate titles with `str::floor_char_boundary(512)` before replacing `\n`/`\r`. A missing title becomes an empty string.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p protocol --test question && cargo check --workspace --all-targets`
Expected: all 6 tests pass and the workspace compiles. `RunInput` is untouched until Task 3.

---

### Task 2: `request_user_input_async` kernel tool

**Files:**
- Create: `crates/kernel/src/runtime/question/mod.rs` (module root, `register`)
- Create: `crates/kernel/src/runtime/question/tool.rs` (`RequestUserInputAsyncTool`)
- Modify: `crates/kernel/src/runtime.rs` (declare `mod question;` beside `mod plan;`)
- Modify: `crates/kernel/src/runtime/session/assembly.rs:198-207` (register after `register_todo_write`)
- Modify: `crates/kernel/src/runtime/session/ownership.rs:16-20,156` (`ROOT_ONLY_AGENT_TOOLS` → `ROOT_ONLY_TOOLS: [&str; 2]`, doc comment updated)
- Test: `crates/kernel/tests/protocol_alignment/ask_user.rs` (new; add `mod ask_user;` to `crates/kernel/tests/protocol_alignment/main.rs`)
- Test: `crates/kernel/tests/subagent/ownership.rs`

**Interfaces:**
- Consumes: Task 1 `AsyncQuestion`, `ToolResultDetails::UserQuestions`, `REQUEST_USER_INPUT_ASYNC_TOOL`.
- Produces: `pub(in crate::runtime) fn register(runtime: &Arc<Session>, kernel: &Kernel) -> Result<(), KernelError>`, which registers in every Session. `RequestUserInputAsyncTool { id_generator: Arc<dyn IdGenerator> }` implements `AgentTool`, built from `Arc::clone(&kernel.id_generator)`.

- [ ] **Step 1: Write the failing tests**

In `ask_user.rs`, add a local `fn ask(questions: serde_json::Value) -> Route` that wraps `super::fixture::calls(&[("call-q", "request_user_input_async", json!({ "questions": questions }))])`, and a `fn question_details(test, session) -> (String, Vec<AsyncQuestion>)` that reads the persisted ToolResult `details`.

```rust
#[tokio::test]
async fn ask_returns_immediately_and_persists_questions() {
    // Route: "ask" → ask([{"title":"Which db?","options":["sqlite","postgres"]}]); anything else → Route::answer("continuing")
    // run_text("ask") completes; model request_count == 2;
    // ToolResult blocks == [Text {"{\"accepted\":true}"}], is_error == false,
    // details == UserQuestions { request_id: non-empty, questions: [AsyncQuestion { title: "Which db?", options: Some(["sqlite","postgres"]) }] }
}

#[tokio::test]
async fn invalid_questions_are_refused_to_the_model() {
    // for each arguments value, the ToolResult is_error == true and details == None:
    // [], 9 questions, [{"title":"  "}], [{"title": "x".repeat(2049)}], [{"title":"T","options":[]}],
    // [{"title":"T","options":["Other"]}], [{"title":"T","options":[" other "]}],
    // [{"title":"T","options":["x".repeat(513)]}], [{"title":"T","options": 9 strings}], {"questions":[...],"extra":1}
}

#[tokio::test]
async fn plan_mode_runs_the_tool() {
    // set_session_mode(Plan) before run_text("ask"); the ToolResult is_error == false
}
```

In `crates/kernel/tests/subagent/ownership.rs`, extend the existing disabled-policy assertion at lines 211-219 so its name list includes `"request_user_input_async"`, and add:

```rust
#[tokio::test]
async fn child_sessions_lack_request_user_input_async() {
    // spawn(&test, &root, json!([["call-1", {"task_name": "reader", "message": "Summarize."}]]))
    // root requests' tools contain "request_user_input_async";
    // the request whose last text is "Summarize." has no tool named "request_user_input_async"
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p kernel --test protocol_alignment ask_user && cargo test -p kernel --test subagent ownership`
Expected: FAIL. The model reports an unknown tool, and the child's disabled list lacks `request_user_input_async`.

- [ ] **Step 3: Implement the tool**

- `definition()`: description copied verbatim from the spec §1.1. The parameter schema copies Codex's field descriptions verbatim:
  - `questions`: "One or more self-contained questions to present together, in display order.", with `minItems: 1` and `maxItems: 8`;
  - `title`: "The complete question shown to the user, including any context needed to answer it.";
  - `options`: "Suggested answers, in display order. Put the recommended answer first; the first option is preselected by default. The user can select one option or enter a free-text answer. Do not include an Other option or a free-text placeholder; the UI provides free-text input automatically. Omit options for a free-text-only question.", with `minItems: 1` and `maxItems: 8`;
  - `additionalProperties: false` at both levels; `title` required.
- `validate()`: deserializes into a private `#[serde(deny_unknown_fields)] struct Arguments { questions: Vec<AsyncQuestion> }` and applies the Global Constraints limits, returning `ToolError::InvalidArguments { tool, message }`.
- `execute()`: calls `context.ensure_active()?`, then sets `request_id = self.id_generator.next(IdKind::Request)`, then logs `tracing::info!("asked {} questions {} in session {}", n, request_id, context.session_id)`, then returns the ToolResult described in the spec §1.1.
- `ownership.rs`: rename the constant to `ROOT_ONLY_TOOLS: [&str; 2] = [protocol::SPAWN_AGENT_TOOL, protocol::REQUEST_USER_INPUT_ASYNC_TOOL]`, reword its doc comment ("Tools only `/root` may call …"), and update its one use at line 156.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p kernel --test protocol_alignment ask_user && cargo test -p kernel --test subagent`
Expected: PASS, including every existing subagent test that uses `ROOT_ONLY_TOOLS`.

---

### Task 3: Answers through prompt admission

**Files:**
- Create: `crates/kernel/src/runtime/question/open.rs` (openness checks on `Session`)
- Modify: `crates/protocol/src/session.rs:13-31` (`RunInput::QuestionReply`; `slash_command_text()` returns `None` for it)
- Modify: `crates/kernel/src/runtime.rs:60` (`KernelError::QuestionClosed`)
- Modify: `crates/kernel/src/runtime/run/queue.rs:95-160,249-260` (`reply_keys`, `claim_next_or_close`)
- Modify: `crates/kernel/src/runtime/run/lifecycle.rs:498-510` (`ActiveRunLease::drop` clears `reply_keys`)
- Modify: `crates/kernel/src/runtime/run/admission.rs:150-260`
- Modify: `crates/kernel/src/runtime/run.rs:130-145,196-207` (ticket release order, expansion arm)
- Modify: `crates/kernel/src/runtime/run/input.rs` (rename variant `Mail` → `Extension`)
- Modify: `crates/kernel/src/runtime/run/queue/operations.rs:70-80` (expansion arm)
- Test: `crates/kernel/tests/protocol_alignment/ask_user.rs`
- Test: `crates/protocol/tests/question.rs` (`question_reply_input_is_never_command_text`)

**Interfaces:**
- Consumes: Task 1 `QuestionReply`, `QuestionKey`, `QuestionReply::of`, `into_extension`; Task 2's tool for fixtures.
- Produces:
  - `KernelError::QuestionClosed { request_id: String, index: usize }` with message `"question {request_id}#{index} is no longer open"`
  - `impl Session { pub(in crate::runtime) fn question_titles(&self, keys: &[QuestionKey]) -> Result<Vec<Option<String>>, KernelError> }`: `Some(title)` for open keys, `None` for closed ones. It reads `execution.settled_question_keys()` first, then `transcript.active_branch()`.
  - `impl Session { pub(in crate::runtime) fn ensure_questions_open(&self, reply: &QuestionReply) -> Result<(), KernelError> }`
  - `impl Session { pub(in crate::runtime) fn expand_question_reply(&self, reply: QuestionReply) -> Result<PromptInputExpansion, KernelError> }`: titles default to `""` when a key is closed.
  - `impl SessionExecution { pub(in crate::runtime) fn reserve_reply_keys(&self, reply: &QuestionReply) -> Result<(), KernelError>; pub(in crate::runtime) fn settled_question_keys(&self) -> Result<BTreeSet<QuestionKey>, KernelError> }`: the second returns, under one queue lock, the union of `reply_keys`, every queued item's reply keys, and (from Task 4) the skip lane's keys.
  - `RunInput::QuestionReply(QuestionReply)`
  - `PromptInputExpansion::Extension { extension, text_projection }` (renamed from `Mail`; `from_mail` keeps its name).

- [ ] **Step 1: Write the failing tests** in `ask_user.rs`

Add a helper `fn reply_prompt(test, session, request_id, index, text) -> (PromptRequest, InsertionReceiver)` that mirrors `client_prompt` in `crates/kernel/tests/protocol_alignment/queue.rs:214`, with `input: RunInput::QuestionReply(QuestionReply { items: vec![QuestionReplyItem { request_id, index, answer: Some(text) }] })` and `when_busy: None`.

```rust
// crates/protocol/tests/question.rs
#[test]
fn question_reply_input_is_never_command_text() {
    assert_eq!(RunInput::QuestionReply(QuestionReply { items: vec![] }).slash_command_text(), None);
}

#[tokio::test]
async fn idle_answer_starts_a_run_with_the_reply_envelope() {
    // after the ask Run settles: kernel.prompt(reply_prompt(.., 0, "postgres")) → Ok(PromptOutcome::Ran(_));
    // the last model request's last message is an Extension with custom_type "question_reply" whose text equals
    // QuestionReply{..}.model_text(&["Which db?".into()]); the insertion receiver reports inserted
}

#[tokio::test]
async fn busy_answer_is_steered_before_the_next_model_call() {
    // Route: after the tool result → Route::Wait(release, [answer "working"]); kernel when_busy default FollowUp
    // while waiting: prompt(reply) → Ok(PromptOutcome::Queued(_)) and pending_messages().steering.len() == 1;
    // release → the next model request's last message is the question_reply Extension
}

#[tokio::test]
async fn closed_questions_reject_answers() {
    // each case → Err(KernelError::QuestionClosed { request_id, index }) and the receiver reports rejection:
    // (a) answer twice while idle; (b) while busy, answer twice (first queued); (c) unknown request_id; (d) index 1 of a 1-question call
}

#[tokio::test]
async fn in_flight_answers_close_the_question() {
    // MessageStartPoint hook (pattern: BlockingInsertion in insertion.rs:225) blocks when
    // QuestionReply::of(&event.message.content).is_some():
    // (a) idle: the first answer starts a Run and blocks in message_start → the second answer → QuestionClosed
    // (b) busy: a queued answer is claimed and blocks in message_start → the second answer → QuestionClosed
}

#[tokio::test]
async fn stopping_returns_a_queued_answer_and_reopens_the_question() {
    // busy; queue the answer; cancel_session → receiver rejected with PromptCancelled;
    // after the Run ends, the same answer is accepted (Ran)
}

#[tokio::test]
async fn switching_branch_closes_the_question() {
    // after ask: navigate_session(&session, None) → answer → QuestionClosed
}

#[tokio::test]
async fn reused_tool_call_ids_stay_independent() {
    // two ask Runs whose tool calls both use id "call-q": answer the first request_id → Ran;
    // answer the second request_id → Ran (not QuestionClosed)
}

#[tokio::test]
async fn answer_after_compaction_is_accepted() {
    // TestKernel::with_compaction(..); after ask, compact the Session with the manual compaction call used in
    // crates/kernel/tests/compaction.rs; the answer → Ran
}

#[tokio::test]
async fn queued_answer_survives_reopen() {
    // busy; queue the answer with the receiver dropped (submitter gone); close_session, then reopen with a local copy
    // of the private `reopen` helper in plan_mode.rs:71; answering again → QuestionClosed;
    // some model request of the next Run contains the queued question_reply
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p kernel --test protocol_alignment ask_user && cargo test -p protocol --test question`
Expected: compile errors for the missing `RunInput::QuestionReply`.

- [ ] **Step 3: Add `RunInput::QuestionReply` and implement openness and `reply_keys`**

Add the `RunInput` variant, and make each `match RunInput` the compiler reports call `session.expand_question_reply(reply)?` (Step 4 lists the two that matter).

Add `reply_keys: BTreeSet<QuestionKey>` to `PendingQueue`.
- `claim_next_or_close` extends it with the popped item's keys inside the same lock.
- `ActiveRunLease::drop` clears it, after its `close_queue()` call.
- Neither `close_queue` nor `open_queue` touches it: `cancel()` calls `close_queue()` mid-Run, and the first Turn's input persistence checks cancellation without the lock (spec §1.4).

`question_titles` iterates `self.transcript.active_branch()?`, not `history()`, which compaction trims. It follows `Kernel::reconcile_lost_exec` in `crates/kernel/src/runtime/exec/mod.rs:145-175`: first filter by `entry.kind == EntryKind::Message` and the payload pointer (`/content/details/type == "user_questions"`, or `/content/extension/custom_type == "question_reply"`), then deserialize the `AgentMessage`. It collects asked titles by `(request_id, index)` and removes keys covered by any branch reply or by `settled_question_keys()`. Read `settled_question_keys()` before the branch, as the spec §1.4 requires.

- [ ] **Step 4: Implement the admission path**

- **`admit_prompt`**: right after `request.ticket.wait().await?` and the session lookup, `if let RunInput::QuestionReply(reply) = &request.run.input { session.ensure_questions_open(reply)?; }`. Placement becomes `WhenBusy::Steer` for that input and ignores `request.when_busy`. Log acceptance with `tracing::info!("admitted answer to question {}#{} for session {}", ..)` and rejection with `tracing::info!("rejected answer to closed question {}#{} for session {}", ..)`.
- **`run.rs`**:
  - move `ticket.release()` to after `install_run(..)?`;
  - just before releasing, call `session.execution.reserve_reply_keys(reply)?` when the input is `QuestionReply`;
  - the expansion arm calls `session.expand_question_reply(reply)?`.
- **`queue/operations.rs`**: the expansion arm also calls `session.expand_question_reply(reply)?`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p kernel --test protocol_alignment && cargo test -p kernel --test subagent`
Expected: PASS, including the existing `queue` and `insertion` suites, which cover the moved ticket release.

---

### Task 4: Skips through `Kernel::dismiss_questions`

**Files:**
- Create: `crates/kernel/src/runtime/question/dismiss.rs`
- Modify: `crates/kernel/src/runtime/subagent/mail.rs:171-215` (extract the extension-writing body)
- Modify: `crates/kernel/src/runtime/run/queue.rs:141-170,249-260` (skip lane beside `mail`)
- Modify: `crates/kernel/src/runtime/run/turn.rs:459-466` (drain before each model request) and `:1407-1450` (write back at Run close, beside `write_back_mail`)
- Test: `crates/kernel/tests/protocol_alignment/ask_user.rs`

**Interfaces:**
- Consumes: Task 3 `question_titles`, `settled_question_keys`, `admission_ticket`.
- Produces:
  - `impl Kernel { pub async fn dismiss_questions(&self, session_id: &SessionId, keys: Vec<QuestionKey>) -> Result<usize, KernelError> }`, which returns the number of questions written (0 when none are open).
  - `PendingQueue.skips: VecDeque<ExtensionMessage>`, with a `ponytail:` comment like the one on `mail` (memory only; a crash before write-back reopens the questions).
  - `impl SessionExecution { pub(in crate::runtime) fn queue_skip(&self, extension: ExtensionMessage) -> Result<Option<ExtensionMessage>, KernelError>; pub(in crate::runtime) fn take_skips(&self) -> Result<Vec<ExtensionMessage>, KernelError> }`. `queue_skip` hands the message back when the queue is closed, like `queue_mail`. `take_skips` drains the lane and adds its keys to `reply_keys` in the same critical section.
  - `impl Session { pub(in crate::runtime) async fn write_extensions(&self, kernel: &Kernel, emitter: &EventEmitter, turn_id: &TurnId, extensions: Vec<ExtensionMessage>) -> Result<Vec<AgentMessage>, KernelError> }`. `write_mail` converts its mail and delegates to it.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn idle_skip_writes_without_starting_a_run() {
    // after ask: dismiss_questions([key 0]) → Ok(1); model request_count unchanged;
    // the branch's last message is a question_reply whose details items == [{request_id, index: 0, answer: None}];
    // the next run_text("next") request contains that message before the "next" user text
}

#[tokio::test]
async fn busy_skip_is_written_before_the_next_model_request() {
    // Route: the second request → Route::Wait(release, calls([an exec_command "pwd" call])) so the Run continues;
    // during the wait: dismiss → Ok(1); pending_messages() has no item for it;
    // the third model request contains the skip question_reply
}

#[tokio::test]
async fn skip_during_the_final_turn_costs_no_model_call() {
    // Route: the second request → Route::Wait(release, [answer "done"]) (final answer, no tool call);
    // during the wait: dismiss → Ok(1); release; the Run ends with request_count == 2 and
    // the branch's last message is the skip question_reply (written back at Run close)
}

#[tokio::test]
async fn dismiss_is_idempotent() {
    // dismiss twice → Ok(1) then Ok(0); exactly one question_reply on the branch;
    // dismiss([key0, key0]) on a fresh ask → Ok(1) and one item
}

#[tokio::test]
async fn answer_and_skip_race_settles_once() {
    // spawn the answer prompt and dismiss_questions concurrently; exactly one of
    // { answer Ran/Queued, dismiss Ok(1) } succeeds, the other is QuestionClosed / Ok(0);
    // branch ∪ queue holds exactly one question_reply item for the key
}

#[tokio::test]
async fn stopped_run_writes_back_a_pending_skip() {
    // busy: dismiss → Ok(1); cancel_session; after the Run ends the branch holds the skip question_reply
    // and answering that question → QuestionClosed
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p kernel --test protocol_alignment ask_user`
Expected: FAIL with "no method named `dismiss_questions`".

- [ ] **Step 3: Implement `dismiss_questions`**

1. Allocate a ticket, `wait()`, deduplicate `keys`, and keep only open keys via `question_titles`.
2. Return `Ok(0)` when none remain.
3. Build the extension with `reply.into_extension(&titles)`, using the titles from step 1, and offer it to `queue_skip`.
4. When `queue_skip` hands it back (no open queue), take `acquire_operation()` and call `write_extensions`, using a fresh turn id and the session sink, exactly as `deliver_mail` does.
5. Drop the ticket only after step 3 or 4 completes.
6. Log `tracing::info!("dismissed {} questions in session {}", n, session_id)`.
7. In `turn.rs`:
   - `model_request` calls `take_skips()` right after `take_mail()` and writes any result with `write_extensions`, adding the written messages to `turn_loop.produced_messages`;
   - Run settlement calls `take_skips()` beside `write_back_mail` and writes the remainder, even when the Run was cancelled.
8. Extend `settled_question_keys` with the lane's keys.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p kernel --test protocol_alignment && cargo test -p kernel --test subagent`
Expected: PASS, including the existing mail tests that now use `write_extensions`.

---

### Task 5: Children inherit answers as user text

**Files:**
- Modify: `crates/kernel/src/runtime/subagent/fork.rs:59-110` (`ForkedHistory::from`) and its `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: Task 1 `QUESTION_REPLY_TYPE`.

- [ ] **Step 1: Write the failing test** in the existing `mod tests` of `fork.rs`

```rust
#[test]
fn question_replies_become_user_text() {
    // history: [Extension question_reply with Text block "ENVELOPE", Extension agent_message]
    // ForkedHistory::from(..) keeps exactly one message: MessageContent::User { blocks: [Text "ENVELOPE"] }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p kernel --lib fork::tests::question_replies_become_user_text`
Expected: FAIL, because the reply is filtered out and the history is empty.

- [ ] **Step 3: Add the match arm** `MessageContent::Extension { extension } if extension.custom_type == QUESTION_REPLY_TYPE => user_text(<text blocks joined>)`, placed before the `_ => return None` arm.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p kernel --lib fork::tests`
Expected: PASS.

---

### Task 6: ACP wiring

**Files:**
- Modify: `crates/protocol/src/identity.rs:127-290` (`AcpExtensionMethod::QuestionDismiss` → `"_clawcode/session/question/dismiss"`, `is_query` false, `parse` list)
- Modify: `crates/protocol/src/acp.rs` (`AcpQuestionDismissParameters`)
- Modify: `crates/acp/src/server.rs:860-893` (advertised methods), `:1530-1565` (prompt input), `:1934-1975` (`PromptOptions`), `KernelFailure` mapping
- Modify: `crates/acp/src/extension.rs:290-340` (dispatch arm)
- Test: `crates/acp/tests/ask_user.rs` (harness copied from `crates/acp/tests/plan_mode.rs`: `with_client`, `next_matching`, `drain`, `wait_idle`)

**Interfaces:**
- Consumes: Task 3 admission, Task 4 `Kernel::dismiss_questions`, Task 1 types.
- Produces:
  - `#[derive(Debug, Deserialize)] #[serde(rename_all = "camelCase", deny_unknown_fields)] pub struct AcpQuestionDismissParameters { pub session_id: SessionId, pub items: Vec<AcpQuestionTarget> }`
  - `#[serde(rename_all = "camelCase", deny_unknown_fields)] pub struct AcpQuestionTarget { pub request_id: String, pub index: usize }`, with `impl From<AcpQuestionTarget> for QuestionKey`
  - `PromptOptions.question_reply: Option<QuestionKey>`
  - The response to dismiss is `{}`. `KernelError::QuestionClosed` maps to `invalid_params().data(json!({"reason":"questionClosed","requestId":..,"index":..}))`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn question_reply_prompt_reaches_the_model_as_a_reply() {
    // the scripted model asks; read requestId from the tool_call_update rawOutput.details (type "user_questions");
    // session/prompt [text "postgres"] with _meta.clawcode.questionReply {requestId, index: 0} → stopReason end_turn;
    // the model saw the question_reply envelope; updates include a session update for the extension message
}

#[tokio::test]
async fn invalid_question_reply_meta_is_rejected() {
    // each → ErrorCode::InvalidParams: questionReply + whenBusy; two text blocks; blank text; requestId ""; index -1;
    // unknown field in questionReply; closed question → data.reason == "questionClosed"
    // after the closed-question rejection, no session update reports a turn end or failure (no terminal fallback)
}

#[tokio::test]
async fn dismiss_method_round_trips() {
    // UntypedMessage::new("_clawcode/session/question/dismiss", {sessionId, items:[{requestId, index:0}]}) → {};
    // a second identical call → {}; unknown field → InvalidParams
}

#[tokio::test]
async fn initialize_advertises_the_dismiss_method() {
    // json["_meta"]["clawcode"]["methods"] contains "_clawcode/session/question/dismiss"
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p acp --test ask_user`
Expected: FAIL. The meta field is ignored or rejected as unknown, and the method is not found.

- [ ] **Step 3: Implement**

- **`PromptOptions::try_from`**: parses `questionReply` (requestId non-empty and ≤ 128 chars, `index` a `u64` that fits in `usize`, unknown keys rejected) and rejects it together with `whenBusy`.
- **Prompt handler**: when `options.question_reply` is set, requires `request.prompt.len() == 1` and a Text block whose trimmed text is non-empty, then builds `RunInput::QuestionReply`. It checks the block count before `PromptInput::try_from` (which joins multiple text blocks into one) and before allocating the ticket.
- **Prompt task error arm**: in the spawned `kernel.prompt(..)` match (`server.rs:1570-1612`), treat `Err(KernelError::QuestionClosed { .. })` like `AuthRequired`. That means no `tracing::error!` and no `publish_terminal_fallback`, because no Run started.
- **Dismiss**: the extension arm deserializes `AcpQuestionDismissParameters` and awaits `kernel.dismiss_questions`.
- **Advertising**: add the method to the advertised list.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p acp && cargo test -p protocol`
Expected: PASS, including `v2_alignment` (advertised methods) and the identity parse tests.

---

### Task 7: WebUI

**Files:**
- Modify: `web/src/acp/extensions.ts` (`questionDismiss: \`${prefix}/session/question/dismiss\``)
- Modify: `web/src/domain/model.ts` (types and `Questions` helpers)
- Modify: `web/src/workspace/updateDecoder.ts:170-205` (decode `userQuestions` from `rawOutput`, like `subagentSessionId`)
- Modify: `web/src/workspace/selectors.ts` (`openQuestions`)
- Modify: `web/src/workspace/controller.ts:26,215-325` (answer, dismiss, supersede, returned answers)
- Create: `web/src/features/composer/QuestionPanel.tsx`
- Modify: `web/src/features/composer/Composer.tsx:247-249` (mount the panel before `PlanPanel`)
- Modify: `web/src/features/composer/PendingQueue.tsx:18-37,107` (answer rows, no promote button)
- Modify: `web/src/features/conversation/ToolCallCard.tsx` (question status body)
- Create: `web/src/features/conversation/QuestionReplyCard.tsx`
- Modify: `web/src/features/conversation/Conversation.tsx:82` (dispatch `question_reply` to the card)
- Modify: `web/src/theme/workbench.css` (styles beside `.plan-panel` / `.pending-item`)

**Interfaces:**
- Consumes: Task 6 wire shapes. The tool `rawOutput.details` (live) or `rawOutput.content.details` (replay) is `{type:"user_questions", request_id, questions:[{title, options?}]}`. The reply Extension `details` is `{items:[{request_id, index, answer?}]}`.
- Produces:
  - `type AsyncQuestion = { title: string; options?: readonly string[] }`
  - `type QuestionKey = { requestId: string; index: number }`
  - `ToolCallEntity.userQuestions?: { requestId: string; questions: readonly AsyncQuestion[] }`
  - `const Questions = { TOOL: "request_user_input_async", REPLY_TYPE: "question_reply", keyOf(key): string /* `${requestId}:${index}` */, replyItems(extension): { key: QuestionKey; answer?: string }[] }`
  - `openQuestions(session: SessionWorkspaceState, inFlight: ReadonlySet<string>): { key: QuestionKey; toolCallId: ToolCallId; title: string; options: readonly string[] }[]`: the asked questions in transcript order, minus keys covered by `extensions` replies, by `pending.steering`/`followUp` items whose message content is a `question_reply` Extension, and by `inFlight`
  - `controller.answerQuestion(sessionId, key, text): void`: `session/prompt` with `_meta.clawcode = { submissionId, questionReply: key }` and no `whenBusy`. `Submission` gains `question?: QuestionKey`.
  - `controller.dismissQuestions(sessionId, keys): Promise<void>`

- [ ] **Step 1: Implement the wire layer and state** (`extensions.ts`, `model.ts`, `updateDecoder.ts`, `selectors.ts`). Keep in-flight answer and skip keys in the controller and expose them to the selector.

- [ ] **Step 2: Implement the controller behavior**
  - **Prompt sending**: extract the request, settlement, and rejection handling that `submit` performs today (`controller.ts:215-290`) into one private method. `submit` and `answerQuestion` both call it, so disconnected, unknown, and rejected outcomes behave identically.
  - **`answerQuestion`**: adds the key to the in-flight set and removes it when the prompt settles.
  - **`dismissQuestions`**: adds the keys to the in-flight set and removes them when the request settles. While a Run is active, the skip stays invisible in transcript and queue until the next model request, and the in-flight entry covers that gap in this tab.
  - **`returnInput`**: if `submission.question` is set, writes the text into the panel draft `sessionStorage` key `${slug}:question-draft:${sessionId}:${requestId}:${index}` instead of the composer draft.
  - **`QuestionClosed` rejection** (`rpc.data.reason === "questionClosed"`): dispatches the notice title `该问题已在其他地方回答或跳过`.
  - **`submit`**: when the text does not start with `/` or `!` and `openQuestions` is non-empty, awaits `dismissQuestions` first. On failure it dispatches the notice title `未能跳过待回答的问题` and still sends. `submit` keeps a per-session promise chain: each submission dispatches its prompt only after the previous submission's dismiss settled and its prompt was dispatched. The chain waits for dispatch, not for the prompt response, so ordering holds without waiting for insertion. Synchronous validation still throws before anything is chained, so `Composer` keeps its current error handling.

- [ ] **Step 3: Implement the UI**
  - **`QuestionPanel`**: shows `第 {i} 题 / 共 {n} 题` with previous/next buttons and the title rendered through `MarkdownContent`. Options are radio inputs with the first selected; the last radio is `其他` and reveals a textarea. Buttons are `发送` and `跳过`. It never calls `focus()` on mount. The textarea persists to and restores from the same `question-draft` `sessionStorage` key that `returnInput` writes.
  - **`ToolCallCard`**: when `userQuestions` is set, lists each title with the status `待回答` / `已回答：{answer}` / `已跳过`, plus a `回答` button that selects that question in the panel.
  - **`QuestionReplyCard`**: renders each answered item as `> {title}\n\n{answer}` through `MarkdownContent`; an all-skipped reply renders as one muted line `已跳过 {n} 个问题`.
  - **`PendingQueue`**: shows queued answers as `回答：{title 前 40 字} → {answer}` and hides their promote button. Skips never enter the pending queue.

- [ ] **Step 4: Verify**

Run: `cd web && npm run check && npm run build`
Expected: both exit 0 with no ESLint warnings.

---

### Task 8: End-to-end check, full verification, and the gated commit

**Files:**
- Create: `crates/app/tests/web/ask_user.e2e.mjs` (structure copied from `crates/app/tests/web/plan_mode.e2e.mjs`: production `target/debug/clawcode`, repository `claw.toml`, throwaway data dir and project, `cdp_page.mjs`)

- [ ] **Step 1: Write the e2e script** with three positional modes:
  - **`answer`**: prompt `调用 request_user_input_async 询问我偏好的数据库，选项 sqlite 和 postgres，然后在等待期间继续回复"继续工作"。`. Assert that the panel appears while focus stays on `textarea[aria-label="消息"]`, choose `postgres`, click `发送`, and assert that a reply card with `postgres` appears and the tool card shows `已回答：postgres`.
  - **`skip`**: as `answer`, but click `跳过`, reload the page, and assert that the tool card shows `已跳过` and the panel is gone.
  - **`supersede`**: with a question open, wait until the Run is idle, send the normal message `换个话题`, and assert a `已跳过 1 个问题` card appears before the user message. Waiting for idle makes the order deterministic, per spec §2.3.

- [ ] **Step 2: Run it against the production backend**

Run: `cargo build -p app && node crates/app/tests/web/ask_user.e2e.mjs answer && node crates/app/tests/web/ask_user.e2e.mjs skip && node crates/app/tests/web/ask_user.e2e.mjs supersede`
Expected: each run prints its success line and exits 0.

- [ ] **Step 3: Full verification**

Run: `cargo test --workspace --no-fail-fast -- --skip cancelled_write_to_unread_tty_returns 2>&1 | grep -E "^test result|FAILED"` and `pre-commit run --all-files`
Expected: every suite is `ok` except the 4 `exec_snapshot` tests already failing on `main` before this work (`snapshot_respects_env_policy_and_is_private`, `snapshot_does_not_override_forced_env`, `snapshot_restores_functions_and_aliases`, `snapshot_is_generated_on_first_exec_only`); fmt, clippy, and rig-boundary pass.

- [ ] **Step 4: Ask the user for commit permission.** Only after an explicit yes: read `.gitmessage`, then `git add` the spec, this plan, and all changed files, and run `git commit` with the title `feat(question): add async request_user_input_async with WebUI answers`. Do not push.

---

## Implementation Notes

Execution followed the user's mid-run direction to apply CLAUDE.md rule 15 strictly (operations on context-holding structs, standard traits over helper methods). The resulting interface names differ from the Interfaces blocks above:

| Planned | Implemented |
|---|---|
| `QuestionReplyItem::key()` | `From<&QuestionReplyItem> for QuestionKey` |
| `QuestionReply::of(&ExtensionMessage)` | `TryFrom<&ExtensionMessage>` and `TryFrom<&MessageContent> for QuestionReply` (`QuestionReplyError`) |
| `QuestionReply::{model_text, into_extension}(titles)` | `TitledQuestionReply { reply, titles }`: `Display` + `TryFrom<TitledQuestionReply> for ExtensionMessage` |
| `Session::{question_titles, ensure_questions_open}` | `QuestionLedger`: `TryFrom<&Session>`, `is_open`, `ensure_open`, `titles` |
| `Session::expand_question_reply` | `PromptInputExpansion::from_question_reply(&reply, &ledger)` |
| tool `questions(call)` helper | `TryFrom<&ToolCall> for Arguments` |
| `AcpQuestionTarget` + `From` | `QuestionKey` with camelCase serde is the wire shape |
| `crates/acp/tests/ask_user.rs` | tests in `crates/acp/tests/v2_alignment.rs` (reused harness) |

Every deviation is recorded as a ruling in the execution ledger and summarized in the final report.

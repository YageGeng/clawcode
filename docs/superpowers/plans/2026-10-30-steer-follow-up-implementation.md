# Steer / follow-up 切换实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 会话忙时的 `session/prompt` 按到达顺序接纳、经 input hook 后进入 Kernel 队列（默认 follow-up，可配置默认 steer），在插入时应答；停止或 Run 异常结束时撤回有存活等待者的队列项；follow-up 项可原子地转为 steer；WebUI 按提交标识跟踪每次提交的完整结果。

**Architecture:** Kernel 队列增加开放状态、等待者（插入通知 / 回执）与「已认领」位置，插入与撤回共用同一边界；客户端 prompt 按会话接纳序号串行完成 input hook 与入队或等锁；转换只追加一条可重放的入队记录；队列快照经 `QueueChanged` 在会话锁内串行广播。ACP 解析 `whenBusy` / `submissionId` 并在分发循环中领取接纳序号。WebUI 由 controller 管理请求生命周期，输入框不等待应答。

**Tech Stack:** Rust（kernel / protocol / config / acp / app crates，tokio，typed-builder），agent-client-protocol 3.6，React + TypeScript WebUI。

**Spec:** `docs/superpowers/specs/2026-10-30-steer-follow-up-design.md`

## 执行结果（2026-10-30）

K1–K4、A1、W1–W2 已在当前工作区完成。未创建 worktree，未执行 commit；各「检查点」仅保留提交信息作为任务标签。测试示例按行为合并为 Kernel 队列专项 18 个测试、ACP v2 对齐套件中的相关回归，以及真实 app 浏览器场景；没有逐个照抄示例空测试函数。失败证据包含缺失 API 的编译失败、缺失项移除返回错误，以及排队失败错误地结束其他 Run 的断言失败，修复后均通过。

最终验证：workspace 953 passed / 9 ignored，Clippy、fmt、WebUI check/build、elicitation 3 个测试通过；生产 app + 当前配置 DeepSeek + headless Chrome 12/12 场景通过，无控制台错误。详细覆盖、环境与边界见 [验收报告](../reports/2026-10-30-steer-follow-up-verification.md)。

## Global Constraints

- 沿用「接受即插入」：`session/prompt` 只在消息插入（或 Handled）时应答；插入前被撤回或取消返回 `-32800`。
- 任一条消息只有一种结果：插入、退还、被拒绝或结果未知。
- `claw.toml`：`[kernel] when_busy = "follow_up"`（默认）或 `"steer"`；bootstrap 同值；ACP 线上 `_meta.clawcode.whenBusy` 取 `"followUp"` / `"steer"`，其他值 `-32602`。
- `_meta.clawcode.submissionId`：可选，非空且不超过 128 个字符，否则 `-32602`；不写入消息历史。
- 转换方法 `_clawcode/session/pending_message/promote`，参数 `{ sessionId, queueId }`，返回 `{ "promoted": bool }`。
- 新增撤回原因 `QueueCancellationReason::Withdrawn`；转换只追加一条 `QueueEnqueued`，存储格式与记录类型不变。
- 撤回只针对 `Receipt` 等待者与 `is_listening()` 为真的 `Insertion` 等待者；其余项留在队列。
- CLAUDE.md：超过 3 个字段的结构体用 `typed-builder`；转换优先 `From` / `TryFrom`；`Arc::clone(&x)`；日志用完全限定 `tracing::` 宏与普通格式参数，不记录消息正文；新增函数与非平凡逻辑写英文注释；测试只放在 `tests/` 或 `#[cfg(test)] mod tests`；函数参数不超过 6 个（clippy）。
- 命令一律加 `rtk` 前缀；未经用户明确许可不得 `git commit`（各任务的「检查点」只记录提交信息）。

## Review Focus

- 认领后未插入就退出（Turn 上限、preflight 失败、事件发送失败）：认领项回到队列或按存活规则撤回，不会凭空消失（Task K1 `claimed_item_returns_to_queue_on_every_unconsumed_exit`）。
- 转换写入一半时进程退出：恢复后仍是一个完整项（Task K4 `promote_record_replays_as_a_move`）。
- 同一连接不等应答连续发送：插入顺序与发送顺序一致（Task A1 `rapid_prompts_insert_in_send_order`）。
- 扩展把普通输入改写成 `!echo hello` 或 `/compact`：只作为普通输入交给模型，不执行 shell 或命令（Task K2 `transformed_command_text_stays_plain_input`）。
- 确认入队后断线、终态应答丢失：重连核对后显示结果未知并保留原始输入（Task W2 端到端第 5 条）。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/config/src/kernel.rs` | `QueuePlacement` 与 `KernelConfig.when_busy` |
| `crates/protocol/src/queue.rs` | `QueuedMessage.submission_id` |
| `crates/protocol/src/event.rs` | `AgentEventPayload::QueueChanged` |
| `crates/kernel/src/runtime/queue.rs` | `QueueWaiter`、`QueueRequest`、入队、撤回、转换、重放「移动」、快照事件 |
| `crates/kernel/src/runtime/lifecycle.rs` | 队列锁内的开放 / 关闭 / 认领 / 结算，接纳状态 |
| `crates/kernel/src/runtime/admission.rs`（新增） | `AdmissionTicket`、`PromptRequest`、`PromptOutcome`、`Kernel::prompt` |
| `crates/kernel/src/runtime/run.rs` | 打开队列、认领后插入边界、异常结束撤回、`input_processed` |
| `crates/kernel/src/runtime/submit.rs` | `WhenBusy` 转换、`submit` 去掉救回循环 |
| `crates/kernel/src/runtime/insertion.rs` | `InsertionNotice::is_listening` |
| `crates/kernel/src/runtime/extension/host.rs` | `send_user_message` 非阻塞回退 |
| `crates/kernel/src/runtime/subagent/background.rs` | 回报被撤回时记录「未送达」 |
| `crates/acp/src/server.rs`、`extension.rs`、`mapping.rs`、`crates/protocol/src/identity.rs` | 解析、接纳序号、转换方法、退役 followUp、事件透传 |
| `crates/app/src/web.rs`、`lib.rs` | bootstrap `whenBusy`、Kernel 配置传入 |
| `web/src/...` | 发送、修饰键、请求生命周期、队列面板、退还合并 |

---

## 阶段 K：Kernel

### Task K1：队列开放、等待者与认领边界

**Files:**
- Modify: `crates/kernel/src/runtime/queue.rs`、`crates/kernel/src/runtime/lifecycle.rs:232-350`、`crates/kernel/src/runtime/run.rs`（约 306-460、1080-1115）、`crates/kernel/src/runtime/submit.rs:150-312`、`crates/kernel/src/runtime/extension/host.rs:230-300`、`crates/kernel/src/runtime/transcript.rs`（组合提交）、`crates/kernel/src/runtime/session.rs:897`（恢复核对）、`crates/kernel/src/runtime.rs:113-117`（错误文案）与 `:195-210`（`for_insertion`）
- Test: `crates/kernel/tests/protocol_alignment/queue.rs`（新增，在 `main.rs` 注册 `mod queue;`）

**Interfaces:**
- Produces:
  ```rust
  pub(super) enum QueueWaiter { Insertion(Arc<InsertionNotice>), Receipt(Arc<SubmissionReceipt>) }
  #[derive(typed_builder::TypedBuilder)]
  pub(super) struct QueueRequest {
      kind: QueueKind,
      input: protocol::RunInput,
      #[builder(default)] waiter: Option<QueueWaiter>,
      #[builder(default)] submission_id: Option<String>,
  }
  impl Kernel {
      /// Fails with SessionNotRunning, writing nothing, while the queue is closed.
      pub(super) async fn enqueue(&self, session_id: &SessionId, request: QueueRequest) -> Result<QueuedMessage, KernelError>;
  }
  impl SessionExecution {
      pub(super) fn open_queue(&self) -> Result<(), KernelError>;
      pub(super) fn close_queue(&self) -> Result<(), KernelError>;
      /// Moves the next eligible item to the claimed slot, or closes the queue when the Run will settle.
      pub(super) fn claim_next_or_close(self: &Arc<Self>, allow_follow_up: bool) -> Result<Option<ClaimedItem>, KernelError>;
  }
  /// Owns the claimed item; dropping it before a commit starts puts the item back at the
  /// head of its lane without writing a record.
  pub(super) struct ClaimedItem { /* item, Arc<SessionExecution>, committed flag */ }
  impl ClaimedItem {
      pub(super) fn item(&self) -> &PendingQueueItem;
  }
  impl Session {
      /// Replaces `consume_pending`: ends the claim before writing, commits message plus
      /// Consumed, then answers the waiter as inserted, or as uncertain when the commit fails.
      pub(super) fn consume_claimed(&self, claimed: ClaimedItem, message: &AgentMessage, record: NewRecord) -> Result<(), KernelError>;
  }
  impl SessionTranscript {
      /// Appends and syncs queue records under one lock; any failure marks writes failed and
      /// returns SessionPersistenceUncertain. Used by enqueue, remove, clear, promote, withdraw.
      pub(super) fn commit_records(&self, records: Vec<NewRecord>) -> Result<(), KernelError>;
      /// Appends the reserved message entry and its Consumed record under one lock; any
      /// failure marks writes failed and returns SessionPersistenceUncertain.
      pub(super) fn commit_consumed(&self, entry_id: EntryId, message: &AgentMessage, record: NewRecord) -> Result<(), KernelError>;
  }
  impl PendingQueue {
      /// Replays queue records, then drops items whose reserved entry already exists in the Store.
      pub(super) fn restore(store: &dyn SessionStore) -> Result<Self, KernelError>;
  }
  // QueueCancellationReason::Withdrawn
  // KernelError::SessionPersistenceUncertain 文案改为 "session persistence is uncertain after a failed write; close and resume the session before continuing"
  ```
  `PendingQueueItem.receipt` 改为 `waiter: Option<QueueWaiter>`。已认领项不在 `snapshot()` / `ids()` 中，`remove` 找不到它。`ClaimedItem` 必须一直持有到 `consume_claimed` 或 Run 退出，提交之前的任何提前返回都会经由 Drop 把它放回；`consume_claimed` 在第一次写入之前置位 committed，之后无论成败都不再放回。`commit_records` / `commit_consumed` 仿照 `commit_setting` 的写入失败处理，记 `error`（操作、queue id、原始错误）。`Session::enqueue_pending` / `remove_pending` / `clear_pending` 改用 `commit_records`，替换现有的「`append_record` 后 `sync_store`」，内存队列只在其成功后改变。`KernelError::for_insertion` 新增 `Self::SessionPersistenceUncertain => Self::SessionPersistenceUncertain`。`restore` 替换 `session.rs:897` 的 `from_records` 调用，核对命中时记 `warn`（queue id、entry id），不补写记录。

- [x] **Step 1：写失败测试**

  ```rust
  /// A message offered after the Run's final check writes nothing and starts its own Run.
  #[tokio::test]
  async fn enqueue_after_run_settles_starts_own_run() {
      // Run "hello" to settlement; kernel.queue_message(&s, FollowUp, "late") → Err(SessionNotRunning);
      // pending_messages empty; no QueueEnqueued record persisted.
  }
  /// A stop while a claimed item's message hooks are blocked keeps it out of history.
  #[tokio::test]
  async fn stop_during_claimed_hooks_keeps_item_out_of_history() {
      // Held Run; queue_message "q" (no waiter); a message_start hook blocks only for text "q";
      // once it is entered, cancel_session, then release → user_texts lacks "q"; "q" is pending again.
  }
  /// Every exit that does not insert a claimed item puts it back in the queue.
  #[tokio::test]
  async fn claimed_item_returns_to_queue_on_every_unconsumed_exit() {
      // Three runs, each with "q" queued (no waiter) and claimed at the end of the first Turn:
      // (a) Kernel with max_turns = 1; (b) the next Turn's preflight fails; (c) the sink fails on
      // the next MessageStart. In each case the Run ends with an error, user_texts lacks "q",
      // and pending_messages holds "q" at the head of its lane.
  }
  /// An enqueue whose record was appended but not synced is uncertain, and recovers once on reopen.
  #[tokio::test]
  async fn enqueue_sync_failure_leaves_outcome_uncertain() {
      // StoreFactory wrapper whose sync fails once after being armed. Held Run; arm; queue_message
      // "q" → Err(SessionPersistenceUncertain); pending lacks "q"; queue_message("next") →
      // Err(SessionPersistenceUncertain); close_session → Ok; resume → pending holds "q" exactly once.
  }
  /// A Consumed write that fails after the message entry was written is not undone or retried.
  #[tokio::test]
  async fn consumed_record_failure_leaves_outcome_uncertain() {
      // StoreFactory wrapper (pattern of lifecycle_order.rs) whose append_record fails once for
      // RecordKind::QueueCancelled. Held Run; queue_message "q"; release → Run ends with
      // Err(SessionPersistenceUncertain); pending lacks "q"; queue_message("next") →
      // Err(SessionPersistenceUncertain); close_session → Ok; resume → pending lacks "q",
      // user_texts contains "q" exactly once.
  }
  /// Removing a claimed item is a no-op; the item is inserted exactly once.
  #[tokio::test]
  async fn remove_during_claimed_hooks_is_a_no_op() {
      // Same blocking hook; remove_pending_message(q) → Ok; release → user_texts contains "q" once.
  }
  /// Extension input sent from a non-agent operation's hook neither deadlocks nor strands.
  #[tokio::test]
  async fn extension_message_in_a_non_agent_hook_does_not_deadlock() {
      // A user_bash hook calls send_user_message("from ext") while `!echo hi` holds the gate;
      // the shell completes within WAIT, then user_texts contains "from ext" exactly once.
  }
  ```

- [x] **Step 2：确认失败**：`rtk cargo test -p kernel --test protocol_alignment queue`。
- [x] **Step 3：实现**：
  - Run 在插入首条消息前 `open_queue()`，所有结束路径 `close_queue()`；`run.rs:1084` 改用 `claim_next_or_close(!should_continue_for_tools)`。
  - 认领项的消息 hook 结束后：若 `cancellation.is_cancelled()`，不插入，让 `ClaimedItem` 随 Run 退出被 Drop 放回队列头部；否则调用 `consume_claimed`：成功时 `Insertion` 调 `inserted(&message_id)`、`Receipt` 转入 `submissions`；失败时 `Insertion` 调 `rejected(&SessionPersistenceUncertain)`、`Receipt` 以同一错误结算，Run 以该错误失败结束。Turn 上限检查、preflight、事件发送的提前返回不需要额外处理，都由 Drop 放回。
  - `queue_message` / `queue_message_with_receipt` 改为调用 `enqueue`。
  - `Kernel::submit`：先 `enqueue`（`Receipt`），`SessionNotRunning` 则 `run_with_source`；删除 `wait_for_idle` → `acquire_operation` → 救回 的分支，保留父取消分支。
  - 扩展 `send_user_message`：空闲时照旧内联运行；不空闲时先 `enqueue`，`SessionNotRunning` 则 `tokio::spawn` 后台等锁运行，调用立即返回。
- [x] **Step 4：确认通过**：`rtk cargo test -p kernel`（含 `subagent`、`lifecycle_order`、`session_capabilities` 回归）。
- [x] **Step 5：检查点**：`refactor(kernel): admit queued input only while a Run consumes it`。

### Task K2：接纳序号、input hook 与 `Kernel::prompt`

**Files:**
- Create: `crates/kernel/src/runtime/admission.rs`
- Modify: `crates/config/src/kernel.rs`、`crates/protocol/src/queue.rs`（`submission_id`）、`crates/kernel/src/runtime.rs`（`KernelFactory` / `Kernel` 字段、`mod`、导出、`RunInvocation`）、`crates/kernel/src/runtime/lifecycle.rs`（接纳状态）、`crates/kernel/src/runtime/run.rs:100-140`（`input_processed`）、`crates/kernel/src/runtime/submit.rs`、`crates/kernel/src/lib.rs`、`crates/app/src/lib.rs:180-190`
- Test: `crates/kernel/tests/protocol_alignment/queue.rs`、`crates/config/tests/`（现有配置测试文件）

**Interfaces:**
- Consumes: K1 的 `enqueue` / `QueueRequest` / `QueueWaiter::Insertion`。
- Produces:
  ```rust
  // config
  #[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
  #[serde(rename_all = "snake_case")]
  pub enum QueuePlacement { #[default] FollowUp, Steer }
  // KernelConfig { max_turns, #[serde(default)] when_busy: QueuePlacement }
  // protocol: QueuedMessage { .., #[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)] submission_id: Option<String> }
  // kernel
  impl Default for WhenBusy { /* FollowUp */ }
  impl From<config::QueuePlacement> for WhenBusy;
  // KernelFactory: #[builder(default)] when_busy: WhenBusy
  // RunInvocation: #[builder(default)] when_busy: Option<WhenBusy>, #[builder(default)] input_processed: bool
  //   input_processed = true：run_with_source 跳过 `!bash` 与直接 Slash Command 识别以及 input hook，保留 Skill / Template 展开。
  // QueueRequest（K1）新增 #[builder(default)] input_processed: bool：跳过入队路径的直接 Slash Command 检查。
  // WaiterCancellation（子项目 1）新增 Captured(CancellationToken)：acquire_operation_for_prompt 使用该令牌，不再读取当前代际。
  /// One client prompt's place in its Session's admission order; dropping it releases the turn.
  pub struct AdmissionTicket { /* opaque */ }
  #[derive(typed_builder::TypedBuilder)]
  pub struct PromptRequest {
      run: RunRequest, sink: Arc<dyn EventSink>, trace_id: TraceId,
      insertion: Arc<InsertionNotice>, ticket: AdmissionTicket,
      #[builder(default)] when_busy: Option<WhenBusy>,
      #[builder(default)] submission_id: Option<String>,
  }
  pub enum PromptOutcome { Ran(RunResult), Queued(QueueId), Handled }
  impl Kernel {
      /// Takes the next admission number synchronously, in arrival order.
      pub fn admission_ticket(&self, session_id: &SessionId) -> Result<AdmissionTicket, KernelError>;
      /// Admits in ticket order: direct commands wait for the gate; others run the input hook,
      /// then queue behind the active Run or wait for the gate and run.
      pub async fn prompt(&self, request: PromptRequest) -> Result<PromptOutcome, KernelError>;
  }
  ```
  接纳状态：会话内「下一个可接纳序号」用 `tokio::sync::watch` 推进；持有者在已入队、已 Handled 或已拿到执行锁后放行；未轮到就被丢弃的序号标记跳过，不阻塞后续序号。`admission_ticket` 同时捕获当时的等待者取消令牌，`prompt` 在等待接纳、input hook 返回后、入队前、回退等锁（`WaiterCancellation::Captured`）时都使用这同一枚令牌；已取消即返回 `PromptCancelled`。是否属于直接命令只按原始输入判断，hook 结果一律以 `input_processed = true` 入队或运行。Handled 复用 `run_with_source` 中现有的 Handled 分支逻辑（抽出为共用方法），发实时 user message 并 `inserted`。

- [x] **Step 1：写失败测试**

  ```rust
  /// Busy prompts answer with their own message id only when inserted.
  #[tokio::test]
  async fn busy_prompt_answers_at_insertion() {
      // Run "hold" (Route::Wait); prompt("one") → Queued; its receiver is pending until release,
      // then equals the id of user message "one".
  }
  /// Admission order, not task scheduling, decides insertion order.
  #[tokio::test]
  async fn admission_ticket_order_decides_insertion_order() {
      // While "hold" runs: take ticket A then ticket B; spawn B's prompt("b") first, then A's prompt("a");
      // after release user texts are [.., "a", "b"].
  }
  /// Steer is consumed before earlier follow-ups at the next Turn boundary.
  #[tokio::test]
  async fn steer_is_inserted_before_follow_up() {
      // prompt("later", FollowUp) then prompt("now", Steer) while busy → texts [.., "now", "later"].
  }
  /// Without whenBusy the configured default applies.
  #[tokio::test]
  async fn default_when_busy_comes_from_kernel_config() {
      // Kernel built with .when_busy(WhenBusy::Steer); prompt("x", None) while busy → one steering item.
  }
  /// The input hook transforms busy input before it is queued.
  #[tokio::test]
  async fn busy_input_hook_transform_is_queued() {
      // Hook returns Transform "changed" for "orig" with streaming_behavior Some(FollowUp);
      // after release user texts contain "changed", not "orig".
  }
  /// A Handled busy input answers immediately and never queues.
  #[tokio::test]
  async fn busy_input_hook_handled_answers_without_queueing() {
      // Hook returns Handled → PromptOutcome::Handled, receiver resolves, pending stays empty.
  }
  /// Falling back to a Run does not run the input hook twice.
  #[tokio::test]
  async fn fallback_run_does_not_repeat_input_hook() {
      // Idle prompt("x") → hook invocation count for "x" == 1.
  }
  /// A transform that looks like a command stays plain input on both paths.
  #[tokio::test]
  async fn transformed_command_text_stays_plain_input() {
      // Hook rewrites "a" → "!echo hello" and "b" → "/compact". Busy (queued) and idle (fallback):
      // no UserBashStarted / SlashCommandStart events; the model requests contain the literal
      // texts "!echo hello" and "/compact"; neither prompt is rejected.
  }
  /// A stop during the input hook keeps the prompt from queueing or running.
  #[tokio::test]
  async fn stop_during_input_hook_rejects_prompt() {
      // Busy Session; the input hook blocks for "x"; cancel_session; release the hook →
      // prompt returns Err(PromptCancelled); pending stays empty; user_texts lacks "x".
      // Repeat with an idle Session (fallback path): same result, no Run starts for "x".
  }
  /// Direct commands keep their place: they wait for the gate while holding admission.
  #[tokio::test]
  async fn direct_command_holds_admission_until_it_runs() {
      // While busy: prompt("!echo hi") then prompt("after"); pending stays empty while "!echo hi" waits;
      // after release the shell runs before "after" is inserted.
  }
  /// Restored items without waiters are consumed by the next Run exactly once.
  #[tokio::test]
  async fn restored_items_are_consumed_by_next_run() {}
  /// Stop rejects prompts still waiting for admission.
  #[tokio::test]
  async fn stop_rejects_prompts_waiting_for_admission() {}
  ```
  配置测试：`when_busy` 缺省 `FollowUp`，`"steer"` 解析为 `Steer`。

- [x] **Step 2：确认失败**：`rtk cargo test -p kernel --test protocol_alignment queue` 与 `rtk cargo test -p config`。
- [x] **Step 3：实现**上述接口；`app` 用 `WhenBusy::from(snapshot.kernel.when_busy)` 传入工厂。日志：接纳 `info`（session id、序号、结果），入队 `info`（queue id、种类、来源、submission id），回退运行 `debug`。
- [x] **Step 4：确认通过**：`rtk cargo test -p kernel -p config -p app -p protocol`。
- [x] **Step 5：检查点**：`feat(kernel): admit busy prompts in order through the input hook`。

### Task K3：停止与异常结束撤回

**Files:**
- Modify: `crates/kernel/src/runtime/insertion.rs`、`crates/kernel/src/runtime/queue.rs`、`crates/kernel/src/runtime/lifecycle.rs`、`crates/kernel/src/runtime/session.rs:521-527`（`cancel_session`）、`crates/kernel/src/runtime/run.rs`（失败 / 取消结算前）、`crates/kernel/src/runtime/subagent/background.rs:216-237`
- Test: `crates/kernel/tests/protocol_alignment/queue.rs`

**Interfaces:**
- Consumes: K1 的 `QueueWaiter`、`close_queue`。
- Produces:
  ```rust
  impl InsertionNotice {
      /// Whether the submitter still awaits the answer (its receiver is open).
      pub fn is_listening(&self) -> bool;
  }
  impl Kernel {
      /// Closes the queue, durably withdraws items with a live waiter, then settles those waiters.
      pub(super) fn withdraw_pending(&self, session: &Session, session_id: &SessionId) -> Result<usize, KernelError>;
  }
  ```
  被撤回：`Receipt`，以及 `is_listening()` 为真的 `Insertion`；其余留在队列；已认领项不在此处理（K1 的边界负责）。撤回记录经 K1 的 `SessionTranscript::commit_records` 一次写入并同步；失败时不移除、不返回 `-32800`，这些等待者以 `SessionPersistenceUncertain` 结算。`remove_pending_message` / `clear_pending_messages` 结算被移除项的等待者。

- [x] **Step 1：写失败测试**

  ```rust
  /// Stop withdraws queued prompts and returns each as cancelled.
  #[tokio::test]
  async fn stop_withdraws_listening_prompts() {
      // Held Run; three busy prompts; cancel_session → each receiver yields Err(PromptCancelled);
      // pending empty; after close+resume still empty.
  }
  #[tokio::test]
  async fn stop_cancels_queued_submit_receipts() { /* answer.outcome == Cancelled */ }
  /// A prompt whose submitter left survives stop; the next Run inserts it once.
  #[tokio::test]
  async fn orphaned_prompt_survives_stop() {
      // Queue "kept", drop its receiver, cancel_session → pending holds "kept";
      // prompt("next") while idle → user_texts contains "kept" exactly once.
  }
  /// A claimed prompt stopped before insertion is returned to a listening submitter.
  #[tokio::test]
  async fn stop_returns_claimed_prompt_to_listening_submitter() {
      // Queue "q" via prompt(); message_start hook blocks for "q"; cancel_session; release →
      // receiver yields Err(PromptCancelled); one QueueCancelled(Withdrawn); pending empty.
  }
  /// A claimed prompt whose submitter left is kept, not deleted, by stop.
  #[tokio::test]
  async fn claimed_prompt_of_disconnected_submitter_survives_stop() {
      // Same blocking hook; drop the receiver; cancel_session; release → pending holds "q";
      // prompt("next") while idle → user_texts contains "q" exactly once.
  }
  /// Uncertain persistence answers queued prompts as unknown, never as returned.
  #[tokio::test]
  async fn uncertain_persistence_does_not_return_queued_prompts() {
      // K1's failing-Consumed store; prompt "q" and "r" while busy; "q" is claimed and its commit
      // fails → both receivers yield exactly Err(KernelError::SessionPersistenceUncertain) (matches!,
      // not a Protocol string), neither PromptCancelled;
      // close_session → Ok; resume → pending holds "r" only; user_texts contains "q" exactly once.
  }
  /// A failed Run withdraws what it left queued.
  #[tokio::test]
  async fn failed_run_withdraws_remaining_items() {}
  #[tokio::test]
  async fn remove_and_clear_settle_waiters() {}
  /// Stopping twice, or with nothing queued, withdraws once and does not fail.
  #[tokio::test]
  async fn repeated_stop_withdraws_once() {}
  ```

- [x] **Step 2：确认失败**：`rtk cargo test -p kernel --test protocol_alignment queue`。
- [x] **Step 3：实现**：`cancel_session` 在 `execution.cancel()` 后调用 `withdraw_pending`；Run 以 `Failed` / `Cancelled` 结束时，在执行体退出（`ClaimedItem` 已被 Drop 放回）之后、结算之前调用同一函数，使放回的认领项也按存活规则处理；`Reporter` 在回答为 `Cancelled` 时以 `info` 记录「subagent report {child} -> {owner} was withdrawn before delivery」。日志：撤回 `info`（session id、条数、原因）。
- [x] **Step 4：确认通过**：`rtk cargo test -p kernel`。
- [x] **Step 5：检查点**：`feat(kernel): return withdrawn queue items to their submitters`。

### Task K4：单记录转换与 `QueueChanged`

**Files:**
- Modify: `crates/kernel/src/runtime/queue.rs`（`promote`、`from_records` 移动语义、快照发布）、`crates/kernel/src/runtime.rs`（`Session.queue_publish` / `queue_publishes`）、`crates/kernel/src/runtime/session.rs:382-455`（关闭屏障）、`crates/kernel/Cargo.toml`（`tokio-util` 加 `features = ["rt"]`，`TaskTracker` 需要）、`crates/protocol/src/event.rs`
- Test: `crates/kernel/tests/protocol_alignment/queue.rs`、`crates/protocol/tests/events.rs`

**Interfaces:**
- Produces:
  ```rust
  impl Kernel {
      /// Moves one follow-up to the end of steering with one record written via `commit_records`; `false` when no longer pending.
      pub fn promote_pending_message(&self, session_id: &SessionId, queue_id: &QueueId) -> Result<bool, KernelError>;
      /// Emits the current snapshot from a spawned task, reading and sending under `queue_publish`.
      pub(super) fn publish_queue_changed(&self, session_id: &SessionId);
  }
  // Session: #[builder(default)] queue_publish: tokio::sync::Mutex<()>
  //          #[builder(default)] queue_publishes: tokio_util::task::TaskTracker
  // AgentEventPayload::QueueChanged { pending: PendingMessages }
  ```
  `from_records` 重放 `QueueEnqueued` 时，若同一 `queue_id` 已在队列，先移除再加入目标队列。`publish_queue_changed` 用 `tokio::runtime::Handle::try_current()`，无运行时则跳过，有则经 `session.queue_publishes.spawn(...)` 登记发布任务；`close_session` 在 `acquire_close` 之后、`finish_closing` 与移除运行时之前执行 `queue_publishes.close()` 并 `wait().await`，等待前以 `debug` 记录未完成的发布条数（`len()`）；在入队、认领、撤回、移除、清空、转换成功后调用；认领守卫放回由 Run 退出路径（撤回之后）调用一次。

- [x] **Step 1：写失败测试**

  ```rust
  /// A promoted follow-up is consumed as steer and stays steering after resume.
  #[tokio::test]
  async fn promote_moves_follow_up_to_steering() {
      // Queue "a", "b" (FollowUp); promote(b) == true; exactly one new QueueEnqueued record;
      // close+resume → steering [b]; after release texts [.., "b", "a"].
  }
  /// Replaying a second enqueue for the same id is a move, so a promotion cut short never loses the item.
  #[test]
  fn promote_record_replays_as_a_move() {
      // PendingQueue::from_records([enqueue(id, FollowUp)]) → follow_up [id];
      // from_records([enqueue(id, FollowUp), enqueue(id, Steering)]) → steering [id], follow_up [].
  }
  #[tokio::test]
  async fn promote_of_steering_item_is_a_no_op() {}
  /// A claimed or inserted item reports false and is inserted only once.
  #[tokio::test]
  async fn promote_after_claim_returns_false_without_duplicate() {}
  /// Every queue mutation publishes the resulting snapshot, including submission ids.
  #[tokio::test]
  async fn queue_mutations_publish_snapshots() {
      // Enqueue, claim, claim release (max_turns = 1), withdraw, remove, clear, promote → one snapshot each.
  }
  /// Close returns only after the final queue snapshot reached the sink.
  #[tokio::test]
  async fn close_waits_for_queue_snapshot_delivery() {
      // Held Run with one busy prompt; arm a sink gate that blocks the next QueueChanged; spawn
      // close_session → not finished after 200 ms; release the gate → close returns Ok, and the last
      // QueueChanged the sink recorded before that has empty steering and followUp.
  }
  ```
  `promote_record_replays_as_a_move` 测的是 `pub(super)` 的 `PendingQueue`，放在 `queue.rs` 的 `#[cfg(test)] mod tests` 中。`protocol/tests/events.rs`：`QueueChanged` 序列化为 `{"event":"queue_changed","pending":{"steering":[],"followUp":[]}}`。

- [x] **Step 2：确认失败**：`rtk cargo test -p kernel queue` 与 `rtk cargo test -p protocol`。
- [x] **Step 3：实现**上述接口。日志：转换 `info`（queue id、结果）。
- [x] **Step 4：确认通过**：`rtk cargo test -p kernel -p protocol`。
- [x] **Step 5：检查点**：`feat(kernel): promote follow-ups atomically and publish queue snapshots`。

---

## 阶段 A：ACP 与 app

### Task A1：解析、接纳、转换方法、退役 followUp、事件透传、bootstrap

**Files:**
- Modify: `crates/acp/src/server.rs`（prompt 处理器约 1400 行）、`crates/acp/src/extension.rs`、`crates/acp/src/mapping.rs`、`crates/protocol/src/identity.rs`、`crates/app/src/web.rs:41-50`、`crates/app/src/lib.rs:120-135`
- Test: `crates/acp/tests/v2_alignment.rs`、`crates/acp/tests/mapping.rs`、`crates/app/tests/web.rs`

**Interfaces:**
- Consumes: K2 的 `admission_ticket` / `prompt` / `PromptRequest` / `PromptOutcome`，K4 的 `promote_pending_message` 与 `QueueChanged`。
- Produces:
  ```rust
  /// Product options read from a prompt's `_meta.clawcode`; invalid values are -32602.
  pub(crate) struct PromptOptions { pub(crate) when_busy: Option<WhenBusy>, pub(crate) submission_id: Option<String> }
  impl TryFrom<Option<&wire::Meta>> for PromptOptions { type Error = agent_client_protocol::Error; }
  // AcpExtensionMethod::PendingMessagePromote => "_clawcode/session/pending_message/promote"; FollowUp removed
  // UiBootstrapResponse { .., when_busy: config::QueuePlacement }  // JSON "whenBusy": "follow_up" | "steer"
  ```
  `PromptRejection` 改名为 `pub(crate) struct KernelFailure(pub(crate) KernelError)`，新增分支：`KernelError::SessionPersistenceUncertain` → `internal_error().data(json!({ "outcomeUnknown": true, "message": error.to_string() }))`，其余不变（`PromptCancelled` → `-32800`，其他 → 带字符串 data 的 `internal_error`，与 `into_internal_error` 相同）。`extension.rs` 中移除（`:341`）、清空（`:356`）与新增的转换方法的错误改经 `KernelFailure` 映射，不再用 `into_internal_error`。处理器在分发循环中解析 `PromptOptions` 并领取 `admission_ticket`，再在 `tokio::spawn` 中调用 `kernel.prompt(...)`；应答仍由插入通知驱动。`QueueChanged` 映射为 `_clawcode/event` 透传，在 `metadata()` 中归入无额外字段的分支。

- [x] **Step 1：写失败测试**

  ```rust
  /// A busy prompt answers with the id of the user message inserted for it.
  #[tokio::test]
  async fn busy_prompt_answers_with_inserted_message_id() {}
  /// Uncertain persistence reaches the client through the insertion notice as outcome unknown.
  #[tokio::test]
  async fn persistence_uncertain_prompt_reports_outcome_unknown() {
      // Kernel built with a failing StoreFactory around JsonlStoreFactory (as in kernel
      // lifecycle_order.rs). Case (a): the busy prompt is claimed and its QueueCancelled append fails;
      // case (b): sync fails right after the busy prompt's QueueEnqueued append. Each prompt response
      // is an error with code -32603 and data.outcomeUnknown == true. Case (c): sync fails on a
      // pending_message/remove → the same error shape.
  }
  /// The close answer arrives after the final, empty queue snapshot.
  #[tokio::test]
  async fn close_answer_follows_final_queue_snapshot() {
      // Busy Session with two queued prompts; session/close → the last session update before the
      // close response is queue_changed with empty steering and followUp.
  }
  /// Prompts sent back to back on one connection insert in send order.
  #[tokio::test]
  async fn rapid_prompts_insert_in_send_order() {
      // "hold" running; send "p1".."p5" without awaiting their answers; release;
      // user_message texts after "hold" are exactly ["p1","p2","p3","p4","p5"].
  }
  #[tokio::test]
  async fn invalid_prompt_options_return_32602() { /* bad whenBusy; empty or 129-char submissionId */ }
  #[tokio::test]
  async fn stop_returns_each_queued_prompt_with_32800() { /* three queued → three -32800 */ }
  /// A queued prompt outlives its requesting connection.
  #[tokio::test]
  async fn queued_prompt_survives_requester_disconnect() {
      // queue "kept" on connection 1, disconnect, cancel_session, then a new prompt;
      // a new connection's replay contains user_message "kept" exactly once.
  }
  #[tokio::test]
  async fn promote_method_reports_whether_it_moved() { /* true, then false after consumption */ }
  #[tokio::test]
  async fn follow_up_method_is_no_longer_advertised() {}
  #[tokio::test]
  async fn when_busy_while_idle_runs_normally() {}
  ```
  `mapping.rs`：`queue_changed_maps_to_event_passthrough`（含 `submissionId`）。`app/tests/web.rs`：bootstrap JSON 含 `"whenBusy": "follow_up"`。

- [x] **Step 2：确认失败**：`rtk cargo test -p acp -p app`。
- [x] **Step 3：实现**上述接口；删除 `_clawcode/followUp` 的处理分支与 initialize 方法列表条目。
- [x] **Step 4：确认通过**：`rtk cargo test --workspace`、`rtk cargo clippy --workspace --tests --examples -- -Dwarnings`。
- [x] **Step 5：检查点**：`feat(acp): admit busy prompts in order and promote follow-ups`。

---

## 阶段 W：WebUI

### Task W1：发送、修饰键与队列面板

**Files:**
- Modify: `web/src/bootstrap/model.ts`（`whenBusy`）、`web/src/acp/extensions.ts`（删 `followUp`，加 `pendingMessagePromote`）、`web/src/workspace/controller.ts:170-215`（`submit`、`removePending`、`clearPending`、新增 `promotePending`）、`web/src/workspace/updateDecoder.ts`（`queue_changed`）、`web/src/domain/model.ts`（`QueuedMessage.submissionId`）、`web/src/features/composer/Composer.tsx`、`web/src/theme/workbench.css`

**Interfaces:**
- Consumes: A1 的取值、转换方法、`queue_changed`、bootstrap `whenBusy`。
- Produces（前端内部）：
  ```ts
  type BusyPlacement = "followUp" | "steer";
  controller.submit(input: PromptInput, options?: { alternate: boolean }): void; // 请求发出即返回
  controller.promotePending(queueId: string): Promise<boolean>;
  ```
  每次 `submit` 生成 `submissionId`（`crypto.randomUUID()`）放入 `_meta.clawcode`；`alternate` 为 true（Alt/Option+Enter）时 `whenBusy` 取非默认值，否则不带。Composer 在调用后立即清空并保持可用，不再等待结果。运行中提示「Enter 排队 follow-up · Alt+Enter 发送 steer」，默认 steer 时对调。队列面板分 Steer / Follow-up 两组；follow-up 项显示「转为 steer」，`false` 时提示「已开始执行，无法转换」；移除、清空、转换的错误带 `data.outcomeUnknown` 时提示「会话持久化异常，请关闭并重新打开会话」；`queue_changed` 解码为 `queue/replaced`，删除各操作后的 `refreshPending`（附着时的首次拉取保留）。

- [x] **Step 1：实现**上述内容。
- [x] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [x] **Step 3：端到端验收**（生产 app、DeepSeek、headless Chrome，配置副本仅隔离数据）：
  1. 运行中连续按 Enter 发送两条：输入框每次立即清空并保持可用，面板 Follow-up 组 2 条，提示文字正确。
  2. 第二条「转为 steer」后移入 Steer 组，并先于第一条插入。
  3. Alt+Enter 发送的消息直接进入 Steer 组。
  4. 以 `when_busy = "steer"` 的配置副本复验：提示对调，Enter 进入 Steer 组，Alt+Enter 进入 Follow-up 组。
  5. 两个标签页附着同一会话，一页排队，另一页面板实时出现。
  6. 无控制台错误。
- [x] **Step 4：检查点**：`feat(webui): choose steer or follow-up for busy input`。

### Task W2：请求生命周期与退还合并

**Files:**
- Modify: `web/src/workspace/controller.ts`（提交记录、状态迁移、合并、`outcome/unknown`）、`web/src/features/composer/Composer.tsx`（删除 catch 中的恢复）

**Interfaces:**
- Consumes: W1 的 `submit` 与 `submissionId`；A1 的应答、`-32800`、其他错误；`queue_changed` 快照。
- Produces：每会话按提交序号保存 `{ seq, submissionId, input, state }`，状态与迁移严格按 spec 4.2 的状态表：快照出现自己的 `submissionId` → 已确认入队；应答 → 删除记录；`-32800` → 退还；`-32603` 且 `data.outcomeUnknown === true` → 结果未知（另提示需关闭并重新打开会话）；其他确定错误 → 被拒绝；断线时不下结论；重连后拉到第一份队列快照时核对所有未收到终态应答的提交：仍在队列中 → 「排队中（应答已失联）」，不提示，之后从快照中消失时静默删除；已不在队列中 → 结果未知（保留原始输入、显示提示）。退还与被拒绝项进入同一缓冲，50 ms 合并窗口后按 `seq` 排序，文字以空行分隔置于当前草稿之前，图片与资源按序合并，`draft/activated` 写回；全为退还时提示「已停止，N 条未执行的消息已放回输入框」，含被拒绝项时显示其错误原因。

- [x] **Step 1：实现**上述内容。
- [x] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [x] **Step 3：端到端验收**：
  1. 停止时有 3 条排队：三段文字按提交顺序回到输入框，原草稿在后，提示条数为 3。
  2. 排队消息带图片时停止，图片回到输入框。
  3. 触发一个非 `-32800` 的确定拒绝（例如超过服务端图片上限）：输入与错误原因回到输入框。
  4. 排队并在面板中出现后刷新页面，再停止：该项留在面板，不出现结果未知与重发按钮。
  5. 确认入队后暂停 WebSocket 接收，在另一标签页停止（服务端撤回，`-32800` 在断线中丢失），再重连：显示结果未知并保留原始输入。
  6. 发送后在请求到达前断开连接（测试中暂停 WebSocket 发送后断线）：重连后显示结果未知并保留原始输入。
  7. 单条 prompt 在插入前被取消，仍恢复到输入框（子项目 1 回归）。
  8. 两个标签页各自排队后在一页停止：内容各自回到本页输入框，两页面板均清空。
  9. 无控制台错误。
- [x] **Step 4：检查点**：`feat(webui): track each submission and restore withdrawn input in order`。

## 收尾

- [x] 全量验证：`rtk cargo fmt --all -- --check`、`rtk cargo clippy --workspace --tests --examples -- -Dwarnings`、`rtk cargo test --workspace --no-fail-fast`、`cd web && rtk npm run check && rtk npm run build && npm run test:elicitation`。
- [x] 验收记录写入 `docs/superpowers/reports/2026-10-30-steer-follow-up-verification.md`（中文，按任务列出场景与结果）。

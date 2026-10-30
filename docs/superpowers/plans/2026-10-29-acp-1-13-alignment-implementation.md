# ACP 3.6 升级与 1.13 对齐实现计划

> **执行说明：** 后续实施时使用 superpowers:subagent-driven-development 或 superpowers:executing-plans 逐任务执行，用复选框（`- [ ]`）跟踪步骤。

**目标：** 把 ACP 依赖升级到 3.6.0（schema 1.13.0），使服务端与 WebUI 全面符合 v2 draft alpha.10，并用标准机制提供 elicitation、config options 与 terminal。

**架构：** Kernel 新增插入通知、等待者取消、设置更新原语与若干事件；ACP 层负责把 kernel 事件映射为 1.13 的标准更新、按连接桥接 elicitation、按投影记录前台状态；WebUI 同步实现客户端侧要求。分 A–E 五个阶段，每阶段独立可验收。

**技术栈：** Rust 2024（agent-client-protocol 3.6、agent-client-protocol-http 3.6、tokio、serde、jsonschema、typed-builder、tracing），React/TypeScript WebUI。

**设计文档：** `docs/superpowers/specs/2026-10-29-acp-1-13-alignment-design.md`

## 全局约束

- 依赖：根 `Cargo.toml` 精确锁定 `agent-client-protocol = "=3.6.0"` 与 `agent-client-protocol-http = { version = "=3.6.0", default-features = false }`（理由见 spec A1）；子 crate 只用 `{ workspace = true }` 加本地 feature。
- 代码注释英文，每个新增函数有函数级注释；`docs/` 下新文档中文。
- 超过 3 个字段的结构体用 `typed-builder`；`Option` 字段 `#[builder(default)]`。
- 跨 crate 转换遵守孤儿规则：在 ACP 定义本地借用包装类型 `UsageOf<'a>`、`ElicitationOf<'a>`、`ConfigOptionsOf<'a>`，由它们转换为 SDK 类型；不得直接在两个外部类型之间实现 `From`，tuple 也不能规避此限制。
- 克隆 `Arc` 字段写 `Arc::clone(&x)`。
- tracing 宏全路径调用、正文格式参数；不记录消息正文、表单内容、terminal 输出、凭据。日志点按 spec「日志」一节，每个任务在其代码路径上落实对应条目。
- 测试只放在 crate 的 `tests/` 或 `#[cfg(test)] mod tests`。
- 已知上限用 `shortcut:` 注释写明上限与升级路径。
- 自定义停止原因、命令类型、配置类型等扩展值必须以 `_` 开头。
- 错误码：`-32602` 参数非法、`-32800` 插入前取消、`-32002` 会话不存在或未在本连接活跃（`data.sessionId`）、`-32022` 版本不支持（`data = { requested, supported }`）、`-32603` 内部错误。
- 不使用 worktree；未经用户明确允许不 commit。允许后先跑 pre-commit（fmt + clippy），再按 `.gitmessage` 写提交信息。
- 所有 shell 命令加 `rtk` 前缀。
- 前端不写单测；WebUI 验收用生产 `app` 后端与已配置 provider。

## 审查重点

1. 同一连接连续发送两个 prompt，第二个等待 gate：期望两个都插入，响应各带自己的 `messageId`；本期不保证插入顺序（子项目 2 的队列提供顺序）。等待期间同一连接的 `session/cancel` 仍能被处理。测试在 Task A2。
2. prompt 已插入但发起的客户端在 run 结束前断开：期望 run 继续完成并落盘，服务端不 panic。测试在 Task A2。
3. Turn 已取消后客户端才回复 elicitation：期望该回复被忽略，不报错、不影响后续请求。测试在 Task C2。
4. run 进行中修改 model：期望当前请求不变，下一次模型请求使用新 model。测试在 Task D2。
5. terminal 输出含中文、emoji 与 ANSI 转义：期望 base64 往返后字节完全一致。测试在 Task E2。
6. 普通 run 占锁时排队的 `!bash` / `!!bash` 被 cancel 或 close：期望返回 `-32800` 且 shell 完全未执行；获锁与令牌安装之间也不能漏取消。测试在 Task A1、A4。
7. 长时间 user-bash 执行中重连：期望开始消息、运行状态及阶段 E 的输出可恢复；成功、取消、执行器错误均只结算一次。测试在 Task A3、A6、E2。
8. 取消工具随后发出 `MessageEnd`、再刷新或冷恢复：期望始终是 cancelled，旧记录仍可读取。测试在 Task B3。
9. Kernel 已完成但 watcher 尚未发送：close/config 响应必须等待发送确认；watcher 退出时等待必须失败而非挂死。机制测试在 Task A4 的 `delivery.rs` 单元测试，端到端顺序在 Task A4、D2。
10. input hook 报错：保留诊断并继续插入；真正的 model preflight 错误才用于插入前拒绝测试。测试在 Task A1、A2。
11. assistant 已产生用量后发生内部错误、取消或重试：idle 保留本轮用量且不重计。测试在 Task B6。
12. user-bash 由 SIGTERM 退出或被停止：保存 cwd 与 signal，实时和持久化回放一致；旧消息字段缺失仍可读。测试在 Task E1、E2。
13. 获锁后 hook 阻塞或预压缩期间取消：期望返回 `-32800`、无用户消息，前台以 `idle(cancelled)` 结束而非 `idle(error)`。测试在 Task A1、A2。
14. `/compact` 执行中 cancel 或 close：期望 `idle(cancelled)`，close 时先于响应；非取消错误仍为 `idle(error)`。测试在 Task A5。
15. 两个标签页与扩展并发修改配置：期望每个连接按到达顺序应用全部配置消息后，最终状态等于 Kernel 的最终设置。测试在 Task D2。
16. 后台 subagent 回报或扩展消息触发的 run 在等待 gate、hook 阻塞或预压缩期间被停止：期望输入不丢失，用户消息仍落盘，结算为 `Cancelled`。测试在 Task A1。
17. `!bash` 长时间持续输出：期望 journal 中同一 terminal 只保留最新快照，重连不重放全部历史快照；bash 进度只发 `TerminalUpdate`。测试在 Task E2。
18. run 卡在不响应取消的 hook 时 close：期望同一连接上其他会话的请求不被挡住；超过 10 秒宽限期返回 `-32603`（`data.closing = true`），后台关闭完成后再清理。测试在 Task A4。
19. 提交后在插入前被停止，或等待插入期间用户继续输入：期望提交内容回到输入框（放在新输入之前），新输入不被清掉。验收在 Task A8。

---

## 阶段 A：编译迁移与必须语义

### Task A1：Kernel 插入通知与等待者取消

本任务不依赖 SDK 版本，在 SDK 2.0 上完成并单独验收（spec A1「检查点顺序」）：ACP 的 `run_traced` 调用点暂传 `None`，`session/prompt` 保持现有立即响应，到 Task A2 再改为插入后响应。

**文件：**

- 新增： `crates/kernel/src/runtime/insertion.rs`
- 修改： `crates/kernel/src/runtime.rs`（`mod insertion;`、`pub use insertion::InsertionNotice;`、`KernelError::PromptCancelled`、`run_traced` 参数）
- 修改： `crates/kernel/src/lib.rs`（导出 `InsertionNotice`）
- 修改： `crates/kernel/src/runtime/run.rs:6`（`run_with_source` 新参数；`persist_message` 之后触发；`InputResult::Handled` 分支）
- 修改： `crates/kernel/src/runtime/command/execution.rs`（执行上下文携带 `InsertionNotice`；正常路径与忙碌分支的 Invocation 落盘后各触发一次；不改 `persist_and_emit_slash_message`）
- 修改： `crates/kernel/src/runtime/lifecycle.rs`（`waiters` 令牌；`acquire_operation_for_prompt`；`#[cfg(test)] mod tests` 增加准入边界单元测试）
- 修改： `crates/kernel/src/runtime/user_bash.rs`（独立 user-bash 路径也使用可取消 gate 等待与令牌安装；`BashExecution` 落盘后临时触发 `inserted`，Task A3 移到开始时）
- 修改： `crates/kernel/src/runtime/submit.rs`（`PromptCancelled` → `AnswerOutcome::Cancelled`）
- 修改： `crates/acp/src/server.rs`、`crates/kernel/src/runtime/extension/host.rs:262`（调用点传 `None`）
- 修改： `crates/kernel/tests/mcp_host.rs`、`crates/kernel/tests/turn.rs`（已有 `run_traced` 调用补参数）；搜索所有 `run_traced` / `run_with_source` 调用同步签名。
- 新增： `crates/kernel/tests/protocol_alignment/main.rs`、`crates/kernel/tests/protocol_alignment/insertion.rs`

**接口：**

- 提供接口：
  ```rust
  pub struct InsertionNotice(/* Mutex<Option<oneshot::Sender<Result<MessageId, KernelError>>>> */);
  impl InsertionNotice {
      pub fn new() -> (Arc<Self>, tokio::sync::oneshot::Receiver<Result<MessageId, KernelError>>);
      pub(in crate::runtime) fn inserted(&self, message_id: &MessageId);
      pub(in crate::runtime) fn rejected(&self, error: &KernelError);
  }
  // KernelError: #[error("prompt was cancelled before insertion")] PromptCancelled
  pub async fn run_traced(&self, request: RunRequest, sink: Arc<dyn EventSink>, trace_id: TraceId,
                          insertion: Option<Arc<InsertionNotice>>) -> Result<RunResult, KernelError>;
  // SessionExecution: waiters: Mutex<CancellationToken>（#[builder(default)]）；cancel() 同时取消并替换 waiters
  // SessionExecution and its Session forwarding method:
  /// Acquires the prompt gate and installs cancellation without an admission gap.
  async fn acquire_operation_for_prompt(&self, cancellation_parent: Option<&CancellationToken>, waiters: WaiterCancellation)
      -> Result<(tokio::sync::MutexGuard<'_, ()>, CancellationToken), KernelError>;
  /// Whether a session-wide cancel may reject this run before insertion.
  pub(in crate::runtime) enum WaiterCancellation { Reject, Ignore }
  /// Only runs with an external client waiting for acceptance may be rejected by cancel_session.
  impl From<Option<&InsertionNotice>> for WaiterCancellation;
  ```
  内部来源（`Kernel::submit`、扩展 `send_user_message`）没有 `InsertionNotice`：等待 gate 时不响应等待者令牌，不做插入前复查；插入前因取消结束时先落盘首条用户消息再以 `Cancelled` 结算（spec A4「取消范围」）。
  `rejected` 需要把 `KernelError` 复制给接收端：为 `KernelError` 增加内部方法 `fn for_insertion(&self) -> KernelError`，保留 `PromptCancelled`、`SessionNotFound`、`SessionClosing`、`ExtensionBlocked` 原样，其余转为 `KernelError::Protocol(error.to_string())`。
- 测试目标 `protocol_alignment` 复用 subagent fixture：`#[allow(dead_code)] #[path = "../subagent/fixture.rs"] mod fixture;`。

- [ ] **Step 1：写失败测试**（`insertion.rs`）

```rust
#[tokio::test]
async fn insertion_reports_persisted_user_message_id() {
    let (notice, receiver) = InsertionNotice::new();
    // run_traced(session, "hello", sink, trace, Some(notice))
    let id = receiver.await.expect("sent").expect("inserted");
    let user = transcript.iter().find(|m| matches!(m.content, MessageContent::User { .. })).unwrap();
    assert_eq!(user.identity.message_id, id);
}

#[tokio::test]
async fn slash_command_insertion_uses_invocation_message() { /* "/name x" → id == SlashCommand Invocation message id */ }

/// A busy direct command is persisted, so it counts as inserted and reports failure via its output message.
#[tokio::test]
async fn busy_slash_command_is_inserted_with_failed_output() {
    // Hold the gate with run A; send "/name x" as run B with its notice.
    let id = receiver_b.await.expect("sent").expect("inserted");
    // Invocation id == id; next message is a Failed SlashCommand output carrying SlashCommandError::Busy.
    // Sink saw no SlashCommandStart / SlashCommandEnd for run B.
}

#[tokio::test]
async fn extension_handled_input_inserts_live_only_message() {
    // test extension input hook returns Handled
    let id = receiver.await.expect("sent").expect("inserted");
    assert!(sink.saw_user_message(&id));
    assert!(transcript.iter().all(|m| m.identity.message_id != id)); // not persisted
}

#[tokio::test]
async fn cancel_rejects_prompt_waiting_for_gate() {
    // run A hangs; prompt B awaits gate with its notice; kernel.cancel_session(&s)
    assert!(matches!(receiver_b.await.expect("sent"), Err(KernelError::PromptCancelled)));
    assert!(matches!(run_b.await, Err(KernelError::PromptCancelled)));
}

#[tokio::test]
async fn submit_maps_prompt_cancelled_to_cancelled_outcome() {
    // submit waits for gate (idle path raced by run A); cancel_session
    assert_eq!(answer.outcome, AnswerOutcome::Cancelled);
}

/// Neither prefixed shell form may start after cancellation while waiting for the gate.
#[tokio::test]
async fn cancel_rejects_waiting_user_bash_without_spawning() {
    // Hold the gate with a model barrier; enqueue both ! and !! cases; cancel.
    // Assert PromptCancelled and that the command's temporary marker file does not exist.
}

/// Closing rejects waiting shell prompts through the same admission path.
#[tokio::test]
async fn close_rejects_waiting_user_bash_without_spawning() { /* no marker file; PromptCancelled */ }

/// Until Task A3 moves the trigger to execution start, a shell prompt reports insertion after its durable write.
#[tokio::test]
async fn user_bash_reports_insertion_after_persisting() { /* "!echo hi" → id == BashExecution message id */ }

/// A provider preflight error propagates before any user message is inserted.
#[tokio::test]
async fn preflight_failure_rejects_insertion() { /* failed receiver; no User message */ }

/// Input hook errors remain diagnostics and do not reject a prompt.
#[tokio::test]
async fn input_hook_error_reports_diagnostic_and_still_inserts() { /* ExtensionHandlerFailed then successful insertion */ }

/// A cancel that lands while a post-admission hook is blocked still rejects the prompt before insertion.
#[tokio::test]
async fn cancel_during_blocked_hook_rejects_before_insertion() {
    // Test extension's before_agent_start hook waits on a barrier; start run with notice; cancel_session; release barrier.
    assert!(matches!(receiver.await.expect("sent"), Err(KernelError::PromptCancelled)));
    assert!(matches!(run.await, Err(KernelError::PromptCancelled)));
    // No User message in the transcript; the Run settled once with AgentOutcome::Cancelled.
}

/// Cancellation surfacing from pre-prompt compaction is a pre-insertion cancel, not an internal error.
#[tokio::test]
async fn cancelled_pre_prompt_compaction_rejects_with_prompt_cancelled() {
    // Force pre-prompt compaction with a model that hangs until cancelled; cancel_session.
    assert!(matches!(receiver.await.expect("sent"), Err(KernelError::PromptCancelled)));
}

/// A subagent report waiting behind another run survives the user's stop.
#[tokio::test]
async fn cancel_does_not_reject_waiting_internal_submission() {
    // Run A holds the gate; kernel.submit(owner, report) waits; cancel_session; run A ends.
    // The report is persisted as a User message and its Run executes.
}

/// Internal input interrupted before insertion stays in the transcript.
#[tokio::test]
async fn internal_submission_cancelled_before_insertion_is_persisted() {
    // submit triggers pre-prompt compaction that hangs until cancelled; cancel_session.
    // The User message with the report text exists; AgentSettled is Cancelled; answer.outcome == Cancelled.
}

/// Extension-authored input follows the same no-loss rule.
#[tokio::test]
async fn extension_message_waiting_for_gate_survives_cancel() {}
```

准入边界需要直接操作 `SessionExecution`，写在 `crates/kernel/src/runtime/lifecycle.rs` 已有的 `#[cfg(test)] mod tests` 里，不加测试专用钩子：

```rust
/// Cancelling while a prompt waits for the gate rejects it without installing a token.
#[tokio::test]
async fn cancel_before_admission_rejects_without_token() {
    // Hold the gate; spawn acquire_operation_for_prompt(None, WaiterCancellation::Reject); cancel(); drop the gate.
    // Expect Err(PromptCancelled) and no installed cancellation.
}

/// A cancel issued right after admission reaches the token installed during admission.
#[tokio::test]
async fn cancel_after_admission_cancels_installed_token() {
    // let (_guard, token) = acquire_operation_for_prompt(None, WaiterCancellation::Reject).await?; cancel();
    assert!(token.is_cancelled());
}

/// Internal waiters keep their place when a session-wide cancel only targets client prompts.
#[tokio::test]
async fn ignored_waiter_cancellation_still_admits() {
    // Hold the gate; spawn acquire_operation_for_prompt(None, WaiterCancellation::Ignore); cancel(); drop the gate.
    // Expect Ok with an uncancelled token.
}
```

- [ ] **Step 2：确认失败**：`rtk cargo test -p kernel --test protocol_alignment` 与 `rtk cargo test -p kernel --lib lifecycle` 编译失败。
- [ ] **Step 3：实现**：普通 `run_with_source` 与 `execute_user_bash_operation` 共用 `acquire_operation_for_prompt`，同时等待 gate、waiters、close 与父取消，取消优先；拿到 gate 后复查代际，并在与 `cancel()` 一致的锁序下原子安装运行令牌。按 spec A4 将插入前 cancel/close 转为 `PromptCancelled`，维护操作不改用此方法。`WaiterCancellation` 由 `Option<&InsertionNotice>` 转换得到，`Kernel::submit` 与扩展 `send_user_message` 的调用点因此为 `Ignore`；内部来源不做插入前复查，插入前因取消结束时先以预分配 id 落盘首条用户消息并发 `MessageEnd`，再以 `Cancelled` 结算。为包含普通输入、Slash Command 与 user-bash 分流的完整返回路径报告插入前错误；带通知的 run 在首条用户消息落盘之前复查本次运行令牌，已取消则返回 `PromptCancelled`（Slash Command 不复查，见 spec A2；`UserBashStarted` 前的复查属于 Task A3）；插入前的 `ModelError::Cancelled`（预压缩）同样转换为 `PromptCancelled`；`run_with_source` 的错误结算分支对 `PromptCancelled` 使用 `AgentOutcome::Cancelled`（现有分支对所有错误都用 `Failed`，见 `run.rs` 错误结算）。`SlashCommandStart` 之后、Invocation 落盘之前的错误以 `SlashCommandEnd { status: Failed }` 结束。首条用户消息、Slash Command Invocation（正常路径与忙碌分支）落盘后 `inserted`，忙碌分支不发前台状态事件；`Handled` 分支发只推送的 User 消息后 `inserted`；user-bash 在 `BashExecution` 落盘后临时 `inserted`（加 `shortcut:` 注释，Task A3 移除）。保留 `emit_input` 的错误容错行为，失败测试用 model preflight。日志沿用 spec「日志」。
- [ ] **Step 4：验证**：`rtk cargo test --workspace`（SDK 2.0，含 subagent 回归）全部通过。
- [ ] **Step 5：检查点**：经允许后提交 `feat(kernel): report prompt insertion and cancel waiting prompts`。

### Task A2：SDK 3.6 迁移与 prompt 接受语义

**文件：**

- 修改： `Cargo.toml:103,137`
- 修改： `crates/acp/Cargo.toml:38`（features 增加 `"stdio"`）
- 修改： `crates/acp/src/server.rs`（所有请求闭包的连接参数）
- 修改： `crates/acp/src/batching.rs:120-160`
- 修改： `crates/acp/src/transport.rs`、`crates/app/src/web.rs`、`crates/app/src/lib.rs`、`crates/app/src/main.rs`、`crates/app/tests/web.rs`（`ServerOptions` / `CorsOptions`）
- 修改： `crates/acp/src/content.rs:78`、`crates/acp/src/mapping.rs`（`AvailableCommand`、non-exhaustive 构造）
- 修改： `crates/acp/src/server.rs` 的 `session/prompt` 处理器（建 `InsertionNotice`、派生 run、等待接收端后以 `messageId` 响应）
- 测试： `crates/acp/tests/mapping.rs`
- 新增： `crates/acp/tests/v2_alignment.rs`

**接口：**

- 依赖接口： Task A1 `InsertionNotice`、`run_traced(.., Some(notice))`、`KernelError::PromptCancelled`。
- 提供接口： 全部请求闭包签名 `async move |request, responder, connection: V2ConnectionTo<Client>|`；`BatchComponent::into_channel_and_future(self) -> (Channel, Option<ConnectionDriver>)`。

- [ ] **Step 1：写失败测试**（`crates/acp/tests/mapping.rs` 与 `crates/acp/tests/v2_alignment.rs`）

```rust
#[test]
fn available_commands_use_command_type_discriminator() {
    // map an AvailableCommandsChanged event with one command
    let json = serde_json::to_value(&update).expect("json");
    assert_eq!(json["availableCommands"][0]["type"], "command");
}
```

测试夹具：在 `v2_alignment.rs` 中按 `crates/acp/tests/extension.rs:36-95` 写 `ScriptedModelFactory`（按最后一条消息路由，支持挂起直到取消），按 `extension.rs:360-392` 用 `Client.v2().connect_with(..)` 并在同一连接上先 `session/new` 再发后续请求。

```rust
#[tokio::test]
async fn prompt_response_carries_inserted_message_id() {
    assert_eq!(response.message_id, user_message_update.message_id);
}
#[tokio::test]
async fn prompt_rejected_before_insertion_emits_no_user_message() { /* model preflight error -> JSON-RPC error, no user_message */ }
/// A failing input handler reports a diagnostic while preserving successful prompt acceptance.
#[tokio::test]
async fn input_hook_error_does_not_reject_prompt() { /* messageId returned; UserMessage exists */ }
/// The prompt handler must not hold the dispatch loop: cancel arrives on the same connection.
#[tokio::test]
async fn prompt_waiting_for_gate_is_cancelled_with_32800() { assert_eq!(error.code, (-32800).into()); }
/// A cancel during a blocked post-admission hook is still a pre-insertion cancel.
#[tokio::test]
async fn prompt_cancelled_during_hook_returns_32800() { /* -32800; no user_message; idle(cancelled) follows running */ }
/// Concurrent waiting prompts are each accepted; insertion order is not asserted in this phase.
#[tokio::test]
async fn two_waiting_prompts_each_receive_their_own_message_id() { /* Review Focus 1: distinct ids, both UserMessages exist */ }
#[tokio::test]
async fn prompt_survives_requester_disconnect() { /* Review Focus 2: drop client after response; transcript has answer */ }
```

- [ ] **Step 2：升级依赖并编译**：两处版本改为精确的 `=3.6.0`，加 `stdio` feature，`rtk cargo update -p agent-client-protocol -p agent-client-protocol-http`，`rtk cargo check -p acp --tests`。预期约 55 个错误，分布见 spec A1。
- [ ] **Step 3：迁移**：闭包参数改 `V2ConnectionTo<Client>`；`batching.rs` 用 `ConnectionDriver` 包装原 future 并返回 `Some(driver)`；`ServerOptions::default().with_path(..).with_cors(..).with_health_endpoint(..)`，`CorsOptions` 的 `match` 增加 `_` 分支；`ResourceLink.size` 转 `u64`（负值丢弃）；可用命令构造 `wire::AvailableCommand::Command(wire::Command::new(name, description).input(..))`。
- [ ] **Step 3b：prompt 接受语义**：处理器建 `InsertionNotice`，用 `tokio::spawn` 派生 run 任务，再把 responder 移入 `connection.spawn` 派生的响应任务，由它等待接收端；回调随即返回，不得在回调内 await 接收端（SDK 分发循环会被占住，见 spec A2 第 3 步）。按 spec A2 第 3 步映射结果（`Ok(id)` → `PromptResponse::new(id)`；`PromptCancelled` → `-32800`；其他错误映射；通道关闭 → `-32603`）。活跃会话检查留给 Task A4。不得为编译通过使用占位 ID。
- [ ] **Step 4：验证**：`rtk cargo test --workspace` 全部通过（含 Step 1 测试）；`rtk cargo clippy --workspace --tests --examples -- -Dwarnings` 无告警。
- [ ] **Step 5：检查点**：经用户允许后提交 `build(deps): upgrade agent-client-protocol to 3.6 and accept prompts on insertion`。

### Task A3：`!bash` 开始即插入、前台状态与 journal

**文件：**

- 修改： `crates/protocol/src/event.rs`（`UserBashStarted`、`UserBashFinished`）
- 修改： `crates/kernel/src/runtime/user_bash.rs:59-160`
- 修改： `crates/acp/src/mapping.rs`（running、idle、`UserMessage` 与失败停止原因）
- 修改： `crates/acp/src/projection.rs`（`OperationKind::UserBash` 与 journal 开始/结束，按 run_id 保留执行期间事件）
- 测试： `crates/kernel/tests/protocol_alignment/insertion.rs`、`crates/acp/tests/mapping.rs`、`crates/acp/src/projection.rs` 的 `#[cfg(test)] mod tests`

**接口：**

- 依赖接口： Task A1 `InsertionNotice` 与插入点复查；Task A2 的 SDK 3.6 类型。
- 提供接口： `AgentEventPayload::UserBashStarted { run_id: RunId, message_id: MessageId, command: String, cwd: PathBuf, exclude_from_context: bool }`；`AgentEventPayload::UserBashFinished { run_id: RunId, message_id: MessageId, outcome: AgentOutcome, result_emitted: bool }`；`OperationKind::UserBash`。ACP `UserMessage` 的 `_meta.clawcode.origin = "user_bash"`；阶段 E 复用这些身份与 cwd。

- [ ] **Step 1：写失败测试**

```rust
// kernel
#[tokio::test]
async fn user_bash_inserts_at_start_and_persists_same_id() {
    // run_traced("!sleep 1; echo done") with notice; receiver resolves before the command ends
    let id = tokio::time::timeout(Duration::from_millis(500), receiver).await.expect("early").unwrap().unwrap();
    // after run: BashExecution message has identity.message_id == id
}
// acp mapping
#[test]
fn user_bash_started_maps_to_tagged_user_message() {
    assert_eq!(json["sessionUpdate"], "user_message");
    assert_eq!(json["content"][0]["text"], "!echo hi");
    assert_eq!(json["_meta"]["clawcode"]["origin"], "user_bash");
}
#[test]
fn bash_execution_replay_emits_user_message_with_same_id() { /* replay mapping */ }

/// A running shell retains its accepted message before the final transcript write.
#[test]
fn user_bash_journal_retains_started_message_until_finished() { /* baseline + same run_id; start retained; finish closes journal */ }

/// Every started user-bash operation has one foreground terminal state.
#[tokio::test]
async fn user_bash_success_cancel_and_error_settle_once() {
    // Assert running -> message result when available -> one idle with the matching stop reason.
    // An executor error emits UserBashFinished(Failed, result_emitted=false), not a second prompt error.
}
```

- [ ] **Step 2：确认失败**。
- [ ] **Step 3：实现**：按 spec A2.1，在可取消 admission 后、复查运行令牌未取消时发带 run_id、绝对 cwd 的 `UserBashStarted`；投影依次发 running、user message 并建立 user-bash journal，成功后 `inserted`，同时删除 Task A1 在落盘后的临时触发及其 `shortcut:` 注释，并把 A1 的 `user_bash_reports_insertion_after_persisting` 改为本任务的开始时插入断言。成功/取消先以同 id 持久化、同步、发 `MessageEnd`，再发 `UserBashFinished(result_emitted=true)`；未发结果消息时为 false，执行器明确取消结算为 Cancelled，其他执行器或持久化错误结算为 Failed。终态关闭 journal，映射/发送错误记录原始错误并走标准 error 兜底；`!!` 保留原前缀与上下文排除语义。运行中 resume 的快照验收接 Task A6。
- [ ] **Step 4：验证**：`rtk cargo test -p kernel -p acp`。
- [ ] **Step 5：检查点**：`feat(kernel): insert user bash messages when execution starts`。

### Task A4：ACP 取消、关闭、活跃会话与发送确认

**文件：**

- 修改： `crates/acp/src/server.rs`（prompt、cancel、close 处理器；活跃会话检查）
- 修改： `crates/acp/src/projection.rs`（所有事件组的 `last_sequence`，独立于恢复游标）
- 新增： `crates/acp/src/delivery.rs`（`SessionDelivery`、`SendPermit`；`#[cfg(test)] mod tests` 覆盖等待、许可与 watcher 退出）
- 测试： `crates/acp/src/batching.rs` 的 `#[cfg(test)] mod tests`（通知/响应帧顺序）
- 修改： `crates/acp/tests/v2_alignment.rs`（Task A2 创建）

**接口：**

- 依赖接口： Task A1 `KernelError::PromptCancelled`；Task A2 的 prompt 处理器与 `v2_alignment.rs` 夹具。
- 提供接口： `fn session_not_active(session_id: &SessionId) -> agent_client_protocol::Error`（`-32002`，`data = { "sessionId": id }`），作为 `impl From<&SessionId> for AcpInactiveSession` 之类的类型方法实现；活跃判定为当前连接 `watchers.sessions` 含该 id。
- 提供接口： `SessionDelivery`（当前连接 watcher 的 `tokio::sync::watch::Receiver<Option<Sequence>>`），其 `async fn wait_through(&self, sequence: Sequence) -> Result<(), agent_client_protocol::Error>` 只确认该连接通知已按序进入 SDK 出站队列；`async fn send_permit(&self) -> SendPermit<'_>` 返回与 watcher 发送共用的锁，持有期间该连接不发新通知，必须在 `wait_through` 之后获取；`ProjectionRegistry::last_sequence(&SessionId) -> Result<Option<Sequence>, ProjectionError>`。watcher 完整发送每组通知后发布进度，初始化与订阅边界一致；无目标 sequence 时直接跳过等待。Task D2 复用该接口。
- 测试夹具：复用 Task A2 在 `v2_alignment.rs` 中建立的 `ScriptedModelFactory` 与连接辅助。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn prompt_on_inactive_session_returns_32002() {
    assert_eq!(error.code, (-32002).into());
    assert_eq!(error.data.unwrap()["sessionId"], session_id);
}
#[tokio::test]
async fn cancel_for_inactive_session_still_cancels() { /* second connection never resumed; cancel stops run */ }
#[tokio::test]
async fn close_projects_idle_cancelled_before_response() { /* idle(cancelled) notification precedes close response */ }
/// Shell prompts waiting behind another run are cancelled before process creation.
#[tokio::test]
async fn waiting_user_bash_cancel_and_close_return_32800() { /* ! and !!; no marker file */ }
/// An already idle session closes without waiting for a nonexistent terminal event.
#[tokio::test]
async fn idle_close_does_not_wait_for_new_terminal_event() {}
/// A close stuck behind a run that ignores cancellation does not freeze the connection.
#[tokio::test]
async fn pending_close_does_not_block_other_sessions_on_connection() {
    // Session S1: test extension hook ignores cancellation and waits on a barrier; send session/close(S1).
    // While close is pending, session/new + session/prompt on S2 over the same connection succeed.
    // Release the barrier: S1 idle(cancelled) then close response.
}
/// A close that outlives the grace period answers with a closing error and finishes in the background.
#[tokio::test(start_paused = true)]
async fn close_past_grace_period_reports_closing_then_completes() {
    // Hook ignores cancellation; paused time auto-advances past CLOSE_GRACE while the hook waits.
    // Response is -32603 with data {"sessionId": id, "closing": true}; release the hook; watcher and projection are removed.
}
```

「watcher 尚未发送」只能在 crate 内构造：集成测试拿不到 `pub(crate)` 的发送许可，也不得加生产测试钩子。以下写在 `crates/acp/src/delivery.rs` 的 `#[cfg(test)] mod tests`，直接驱动发布端：

```rust
/// A response barrier waits until the watcher publishes the target sequence.
#[tokio::test]
async fn wait_through_blocks_until_sequence_is_published() { /* pending at 6; publish 7 → Ok */ }
/// A dead watcher cannot leave a response barrier pending forever.
#[tokio::test]
async fn wait_through_fails_when_publisher_drops() { /* drop publisher → Err, no false success */ }
/// Holding the send permit keeps the watcher from publishing until it is released.
#[tokio::test]
async fn send_permit_excludes_watcher_publish() {}
```

- [ ] **Step 2：确认失败**：`rtk cargo test -p acp --test v2_alignment` 与 `rtk cargo test -p acp --lib delivery`。
- [ ] **Step 3：实现**：在 Task A2 的 prompt 处理器最前面加入活跃会话检查；cancel 通知不做活跃检查。`SessionDelivery` 的发送许可与 watcher 共用同一把锁，watcher 每发一组通知时持有。按 spec A4.1 增加当前连接的发送进度确认：所有通知成功入队后才推进 sequence，断线、watcher 退出或不可恢复 lag 使等待失败，不能跨过未发送事件。close 回调做完活跃检查后，把 responder 移入 `connection.spawn` 的任务并立即返回；该任务先取得句柄，调用 `kernel.close_session` 后捕获 `last_sequence`，等 `SessionDelivery::wait_through` 成功才响应并清理 watcher/projection。`kernel.close_session` 超过 `const CLOSE_GRACE: Duration = Duration::from_secs(10)` 未完成时，先以 `-32603`、`data = { sessionId, closing: true }` 响应并 warn，继续等待同一个 future 完成后再清理；按 spec A4 加 `shortcut:` 注释说明不强制终止 run。batching 保留通知在响应前的帧顺序。日志见 spec「日志」。
- [ ] **Step 4：验证**：`rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): scope requests to active sessions and confirm delivery`。

### Task A5：错误停止原因

**文件：**

- 修改： `crates/protocol/src/event.rs`（`SlashCommandEnd.message`）
- 修改： `crates/protocol/src/slash_command.rs:123`（`SlashCommandStatus::Cancelled`）
- 修改： `crates/kernel/src/runtime/command/execution.rs`（失败时填 `message`；取消产出 `Cancelled`，不再并入 `ExecutionFailed`）
- 测试： `crates/kernel/tests/protocol_alignment/insertion.rs`、`crates/acp/tests/v2_alignment.rs`
- 修改： `crates/acp/src/mapping.rs:257-340`；`crates/protocol/src/identity.rs`（删除 `ACP_ERROR_STOP_REASON`）
- 测试： `crates/acp/tests/mapping.rs`（更新 `failed_agent_settlement_does_not_map_to_cancelled`）

**接口：**

- 提供接口： `SlashCommandEnd { run_id, status, #[serde(default, skip_serializing_if = "Option::is_none")] message: Option<String> }`；`SlashCommandStatus::{Succeeded, Failed, Cancelled}`（新增值，旧记录照常读取）。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn failed_run_maps_to_error_stop_reason() {
    assert_eq!(json["stopReason"], "error");
    assert_eq!(json["error"]["code"], -32603);
    assert_eq!(json["error"]["message"], "provider failed");
}
#[test]
fn failed_slash_command_uses_message_or_default() { /* None → "Slash Command failed" */ }
#[test]
fn cancelled_slash_command_maps_to_cancelled_stop_reason() { assert_eq!(json["stopReason"], "cancelled"); }
// kernel
/// Cancelling or closing during /compact settles the command as cancelled, not failed.
#[tokio::test]
async fn compact_cancel_and_close_end_cancelled() {
    // Model hangs in the compaction summary; cancel_session (and separately close_session).
    // Output message status == Cancelled; SlashCommandEnd { status: Cancelled }.
}
/// A non-cancellation command error still reports Failed.
#[tokio::test]
async fn compact_provider_error_stays_failed() {}
// acp
#[tokio::test]
async fn compact_cancel_projects_idle_cancelled() { /* close variant: idle(cancelled) precedes close response */ }
```

- [ ] **Step 2：确认失败**。
- [ ] **Step 3：实现**：`StopReason::Error(ErrorStopReason::new().error(Error::new(-32603, message)))`。`execution.rs` 在命令返回错误或 `Failed` 输出时，若本次运行令牌已取消或错误为 `ModelError::Cancelled`，改为 `SlashCommandStatus::Cancelled` 的输出与 `SlashCommandEnd`；mapping 将 `Cancelled` 映射为 `StopReason::Cancelled`。Invocation 落盘前不复查取消（spec A2），落盘后的取消由命令执行观察到并按本步结算。
- [ ] **Step 4：验证**：`rtk cargo test -p acp -p kernel`。
- [ ] **Step 5：检查点**：`feat(acp): report failures with the standard error stop reason`。

### Task A6：resume 当前状态同步

**文件：**

- 修改： `crates/acp/src/projection.rs`（`SessionProjection` 记录最后前台状态；订阅时原子读出）
- 修改： `crates/acp/src/server.rs:745-990`（resume：过滤回放中的 `StateUpdate`，回放后发送快照）
- 测试： `crates/acp/tests/v2_alignment.rs`

**接口：**

- 提供接口： `pub(crate) enum ForegroundState { Running, RequiresAction, Idle }`；`ProjectionWatch` 增加字段 `state: ForegroundState`；`impl From<ForegroundState> for wire::StateUpdate`（Idle 不带 stop reason）。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn resume_replay_excludes_historical_state_updates() {
    // completed run, then new connection resumes with replayFrom start
    let states: Vec<_> = notifications_before_response.iter().filter(is_state_update).collect();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["state"], "idle"); // snapshot only, after all replayed messages
}
#[tokio::test]
async fn resume_of_running_session_reports_running_snapshot() { /* model hangs; resume → last notification before response is running */ }
#[tokio::test]
async fn cursor_resume_excludes_historical_state_updates() { /* replayFrom {_clawcode/event cursor} */ }
/// A running user-bash journal reconstructs acceptance before its result is durable.
#[tokio::test]
async fn resume_running_user_bash_restores_message_and_running_state() { /* start and cursor resumes; same messageId; running snapshot */ }
/// Reconnection does not own or cancel the shell operation.
#[tokio::test]
async fn user_bash_survives_disconnect_and_resumes_idle_after_completion() { /* final persisted message; idle snapshot */ }
```

- [ ] **Step 2：确认失败**。
- [ ] **Step 3：实现**：`SessionProjection::append_group` 遇到 `StateUpdate` 更新 `state`；`subscribe_*` 在持锁时一并返回 `state`；回放（durable replay 与 backlog）发送前过滤 `SessionUpdate::StateUpdate`；回放后先发快照再响应。
- [ ] **Step 4：验证**：`rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): synchronize current state on session resume`。

### Task A7：initialize 版本协商

**文件：**

- 修改： `crates/acp/src/server.rs:516-600`
- 测试： `crates/acp/tests/v2_alignment.rs`

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn initialize_selects_v2_for_newer_request() { assert_eq!(response["protocolVersion"], 2); /* request 3 */ }
#[tokio::test]
async fn initialize_rejects_older_version_with_32022() {
    assert_eq!(error.code, (-32022).into());
    assert_eq!(error.data.unwrap(), serde_json::json!({"requested": 1, "supported": [2]}));
}
```

- [ ] **Step 2：确认失败**（若 SDK 3.6 已满足则该测试直接通过，保留为回归）。
- [ ] **Step 3：实现**：仅在 SDK 未覆盖的分支补齐。
- [ ] **Step 4：验证**：`rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`test(acp): pin v2 initialize negotiation`。

### Task A8：WebUI 阶段 A

**文件：**

- 修改： `web/src/workspace/controller.ts`（prompt 结果 `messageId`；initialize）
- 修改： `web/src/workspace/updateDecoder.ts:79-90,186-188`（命令 `type`；`requires_action`）
- 修改： `web/src/features/conversation/MessageView.tsx`（`origin === "user_bash"` 不渲染）
- 修改： `web/src/acp/protocol.ts`（类型）
- 修改： `web/src/domain/model.ts:168`、`web/src/features/conversation/CommandMessageCard.tsx:28`（命令状态增加 `cancelled`，显示「已取消」）
- 修改： `web/src/features/composer/Composer.tsx:106-125`（提交时立即清空并保留副本；`-32800` 与其他错误时放回）

- [ ] **Step 1：实现**：`state === "running" || state === "requires_action"` 视为运行；命令只取 `type === "command"`；命令输出状态 `cancelled` 显示为「已取消」；prompt 结果的 `messageId` 用于去重（已存在同 id 消息时不再创建）。Composer 按 spec A7「输入框」：提交时立即清空输入框与草稿并保留本次提交的副本；成功丢弃副本；`-32800` 时把副本（文本、资源、图片）放回，已有新输入时副本在前、空行分隔，提示「已取消，内容已放回输入框」；其他错误同样放回并显示错误。
- [ ] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build` 通过。
- [ ] **Step 3：端到端验收**：生产 app + provider：发送 prompt、`!echo hi`、`/session`；长时间 `!bash` 运行中刷新仍为 running；普通 run 占锁时排队 `!bash` / `!!bash` 后停止，确认 shell 未执行；触发预压缩的长上下文中提交 prompt，等待插入期间继续输入再停止，确认提交内容回到输入框且新输入保留；停止与关闭后的状态正确。
- [ ] **Step 4：检查点**：`feat(webui): adopt ACP 1.13 prompt acceptance and states`。

## 阶段 B：状态与实体对齐

### Task B1：requires_action

**文件：**

- 修改： `crates/protocol/src/event.rs`（`ActionRequired`、`ActionResolved`）
- 修改： `crates/kernel/src/runtime/mcp.rs:393-470`（挂起计数变化时发出）
- 修改： `crates/acp/src/mapping.rs`、`crates/acp/src/projection.rs`（记录 `RequiresAction`）
- 测试： `crates/kernel/tests/mcp_host.rs`、`crates/acp/tests/mapping.rs`、`crates/acp/tests/v2_alignment.rs`

**接口：**

- 提供接口： `AgentEventPayload::ActionRequired { request_id: String }`、`ActionResolved { request_id: String }`。

- [ ] **Step 1：写失败测试**

```rust
// kernel mcp_host.rs, adapt pending_mcp_elicitations_have_session_snapshots
#[tokio::test]
async fn elicitation_during_run_emits_action_required_then_resolved() { /* event order */ }
// acp mapping.rs
#[test]
fn action_required_maps_to_requires_action_state() { assert_eq!(json["state"], "requires_action"); }
// acp v2_alignment.rs
#[tokio::test]
async fn resume_during_pending_elicitation_reports_requires_action() { /* snapshot */ }
```

- [ ] **Step 2–4**：确认失败 → 实现（计数在 `elicitations` 锁内判断 0→1 与 →0，仅当 `active_run` 存在时发出）→ `rtk cargo test -p kernel -p acp`。
- [ ] **Step 5：检查点**：`feat: report requires_action while elicitations block a run`。

### Task B2：setup 响应初始状态

**文件：**

- 修改： `crates/acp/src/server.rs`（new/resume 响应 `available_commands`；resume 回放后 `session_info_update`）
- 测试： `crates/acp/tests/v2_alignment.rs`

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn session_new_returns_available_commands() { assert!(response["availableCommands"].as_array().unwrap().iter().any(|c| c["name"] == "session")); }
#[tokio::test]
async fn resume_sends_session_info_before_state_snapshot() { /* order: ..., session_info_update, state_update, response */ }
```

- [ ] **Step 2–4**：确认失败 → 实现（空列表省略）→ `rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): return initial commands and session info on setup`。

### Task B3：工具 name、kind、locations、cancelled

**文件：**

- 修改： `crates/protocol/src/tool.rs:254`（`ToolResult.cancelled`）
- 修改： `crates/protocol/src/message.rs`（持久化 `MessageContent::ToolResult.cancelled`，缺字段默认 false）
- 修改： `crates/protocol/src/event.rs:129-135`（`ToolExecutionStart.cwd`）
- 修改： `crates/kernel/src/runtime/tools/batch.rs`（设置 `cancelled`、复制到结果消息；填入 `cwd`）
- 修改： `crates/kernel/src/runtime/run.rs`（扩展 message-end 替换结果后保留取消标记）
- 新增： `crates/acp/src/tool_call.rs`（`ToolKindOf`、`ToolLocations`）
- 修改： `crates/acp/src/mapping.rs`（ToolExecutionEnd、实时 MessageEnd、持久化回放使用相同状态优先级）
- 测试： `crates/kernel/tests/turn.rs`、`crates/acp/tests/mapping.rs`、`crates/acp/tests/v2_alignment.rs`

**接口：**

- 提供接口：
  ```rust
  // protocol::ToolResult
  #[serde(default, skip_serializing_if = "std::ops::Not::not")] #[builder(default)] pub cancelled: bool,
  // protocol::MessageContent::ToolResult
  #[serde(default, skip_serializing_if = "std::ops::Not::not")] cancelled: bool,
  // acp
  pub(crate) struct ToolKindOf<'a>(pub &'a ToolCall);
  impl From<ToolKindOf<'_>> for wire::ToolKind;
  pub(crate) struct ToolLocations<'a> { pub call: &'a ToolCall, pub cwd: &'a Path }
  impl From<ToolLocations<'_>> for Vec<wire::ToolCallLocation>;
  ```
  `AcpEventMapper::map` 是无状态的，拿不到会话 cwd：`AgentEventPayload::ToolExecutionStart` 增加 `cwd: PathBuf` 字段，由 `tools/batch.rs` 填入 `batch.session.cwd`。`locations` 只在实时的工具开始更新中设置；回放的工具调用不设置（字段可选）。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn tool_start_sets_name_kind_and_absolute_location() {
    assert_eq!(json["name"], "read");
    assert_eq!(json["kind"], "read");
    assert_eq!(json["locations"][0], serde_json::json!({"path": "/work/src/main.rs", "line": 10}));
}
#[test]
fn cancelled_tool_result_maps_to_cancelled_status() { assert_eq!(json["status"], "cancelled"); }
#[tokio::test]
async fn cancelled_turn_marks_tool_results_cancelled() { /* kernel turn.rs */ }
/// A persisted message must not overwrite the live cancelled state.
#[test]
fn cancelled_tool_message_end_preserves_cancelled_status() { /* map ToolExecutionEnd then MessageEnd; both cancelled */ }
/// The cancellation marker survives storage and a fresh connection replay.
#[tokio::test]
async fn cancelled_tool_remains_cancelled_after_durable_resume() { /* bypass retained journal; replay stored message */ }
/// An extension replacing visible tool content cannot erase cancellation.
#[tokio::test]
async fn tool_message_end_extension_preserves_cancelled_flag() {}
/// Existing tool records remain readable with the previous terminal status.
#[test]
fn old_tool_result_without_cancelled_defaults_to_false() {}
```

- [ ] **Step 2–4**：确认失败 → 按 spec B3 将取消标记贯穿 ToolResult、MessageContent、扩展替换、存储和所有映射，统一 cancelled 优先于 is_error；搜索并同步所有 `MessageContent::ToolResult` 构造/转换及 fixtures，保持 provider 输入语义 → `rtk cargo test --workspace`。
- [ ] **Step 5：检查点**：`feat: report tool names, kinds, locations, and cancellation`。

### Task B4：compaction 实体

**文件：**

- 修改： `crates/acp/src/mapping.rs:342,460`
- 测试： `crates/acp/tests/mapping.rs`（替换 `completed_compaction_maps_to_product_extension_update`）

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn compaction_maps_to_standard_entity_lifecycle() {
    assert_eq!(start["sessionUpdate"], "compaction_update");
    assert_eq!(start["status"], "in_progress");
    assert_eq!(end["status"], "completed");
    assert_eq!(start["compactionId"], end["compactionId"]);
}
#[test]
fn failed_compaction_carries_error_text() { assert_eq!(end["status"], "failed"); }
```

- [ ] **Step 2–4**：确认失败 → 实现（`compactionId` = `format!("compaction-{run_id}")`）→ `rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): project compaction as the standard entity`。

### Task B5：Session notices

**文件：**

- 修改： `crates/acp/src/mapping.rs`（`ExtensionHandlerFailed`、`SkillDiagnostic` → `Notice`）
- 修改： `crates/acp/src/projection.rs`（`EventGroup.live_only`，不进入回放）
- 修改： `crates/acp/src/server.rs`（new/resume 响应后发送 prompt 诊断 notice；MCP 状态订阅中连接失败发 notice）
- 测试： `crates/acp/tests/mapping.rs`、`crates/acp/tests/v2_alignment.rs`

**接口：**

- 提供接口： `EventGroup { .., #[builder(default)] live_only: bool }`；notice title 固定为 `Extension handler failed`、`Skill expansion failed`、`Prompt resource issue`、`MCP server unavailable`。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn skill_diagnostic_maps_to_warning_notice() { assert_eq!(json["severity"], "warning"); assert_eq!(json["title"], "Skill expansion failed"); }
#[tokio::test]
async fn notices_are_not_replayed_on_resume() { /* live notice seen; resume replay has none */ }
#[tokio::test]
async fn prompt_diagnostics_follow_session_new_response() { /* notice arrives after response */ }
```

- [ ] **Step 2–4**：确认失败 → 实现 → `rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): surface diagnostics as session notices`。

### Task B6：idle 用量

**文件：**

- 修改： `crates/protocol/src/event.rs`（`AgentSettled.usage`）
- 修改： `crates/kernel/src/runtime/run.rs`（execution future 外累加 usage，所有结算分支传入）
- 修改： `crates/kernel/src/runtime/settlement.rs`（必填 usage，传给 AgentSettled）
- 修改： `crates/acp/src/mapping.rs:321-340`
- 测试： `crates/kernel/tests/turn.rs`、`crates/acp/tests/mapping.rs`

**接口：**

- 提供接口： `AgentSettled { run_id, outcome, #[serde(default)] usage: ModelUsage }`；`RunSettlement` 增加必填 `usage: ModelUsage`（无 builder 默认）；ACP 本地 `pub(crate) struct UsageOf<'a>(pub &'a ModelUsage)` 与 `impl From<UsageOf<'_>> for wire::Usage`（`reasoning_tokens` → `thought_tokens`，`cache_*` → `cached_*`）。
- 累加值在 `run_with_source` 的内部 execution future 之外；每个 assistant attempt 结算后、扩展 message-end 校验或持久化等可能失败的操作前恰好累计一次。成功、内部错误和取消都使用同一值；`RunSettlement.messages` 仍服务原用途，不承担错误路径的用量恢复。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn agent_settled_usage_sums_run_assistants() { /* two assistant replies 15 + 15 → total 30 */ }
#[test]
fn idle_state_carries_usage() { assert_eq!(json["usage"]["totalTokens"], 30); assert!(json["usage"].get("thoughtTokens").is_none()); }
/// Settled usage survives an internal error after a model has already consumed tokens.
#[tokio::test]
async fn failed_run_preserves_usage_from_settled_assistant() {
    // Model settles with 15 tokens; a message-end extension returns an invalid identity.
    // Kernel returns Protocol error; AgentSettled(Failed).usage.total_tokens == 15.
}
/// Known usage before cancellation is retained without another accumulation pass.
#[tokio::test]
async fn cancelled_run_preserves_known_usage() {}
/// Retries contribute each settled attempt once, including known failed-attempt usage.
#[tokio::test]
async fn retry_usage_counts_each_attempt_once() { /* attempts use 5 + 15 -> total 20 */ }
/// No model usage yields zero tokens and no synthesized reasoning count.
#[tokio::test]
async fn preflight_failure_has_zero_usage_without_reasoning_tokens() {}
/// Unknown reasoning counts stay unknown unless at least one attempt reports a value.
#[test]
fn reasoning_usage_preserves_none_and_sums_known_values() { /* [None,None] -> None; [None,Some(3),Some(4)] -> Some(7) */ }
```

- [ ] **Step 2–4**：确认失败 → 按 spec B6 累加并显式传入 usage，更新所有 `RunSettlement` 构造与事件 fixtures；ACP 用本地 `UsageOf` 转换，不直接给两个外部类型实现 trait → `rtk cargo test --workspace`（同时验证包装类型在真实 crate 边界下编译）。
- [ ] **Step 5：检查点**：`feat: report run usage on idle state updates`。

### Task B7：WebUI 阶段 B

**文件：**

- 修改： `web/src/workspace/updateDecoder.ts`（`compaction_update`、`notice`；删除 `_clawcode/event` 中 compaction 解析）
- 新增： `web/src/features/notices/NoticeToasts.tsx`
- 修改： `web/src/shell/AppShell.tsx`、`web/src/features/conversation/ToolCallCard.tsx`（`cancelled` 状态样式与 kind 图标）、`web/src/theme/workbench.css`

- [ ] **Step 1：实现**：notice 以 toast 显示 6 秒、可关闭、最多 3 条；`cancelled` 状态显示「已取消」。
- [ ] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [ ] **Step 3：端到端验收**：工具取消后、收到结果消息后及刷新后均保持「已取消」；`/compact` 显示；触发一个扩展失败或 skill 诊断出现 toast。
- [ ] **Step 4：检查点**：`feat(webui): render standard compaction, notices, and tool states`。

## 阶段 C：标准 elicitation

### Task C1：Kernel 侧准备

**文件：**

- 修改： `crates/protocol/src/mcp/host.rs:20-32`（`McpRequestContext.tool_call_id`）
- 修改： `crates/mcp/src/tool.rs:190-200`（填入 `call.tool_call_id`）
- 修改： `crates/kernel/src/runtime/mcp.rs`（`resolve_mcp_elicitation` 已结算返回 `KernelError::ElicitationResolved`）
- 修改： `crates/kernel/src/runtime.rs`（新错误变体）
- 测试： `crates/kernel/tests/mcp_host.rs`

**接口：**

- 提供接口： `McpRequestContext { .., #[serde(default)] #[builder(default)] pub tool_call_id: Option<ToolCallId> }`；`KernelError::ElicitationResolved(String)`。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn elicitation_context_carries_tool_call_id() { assert_eq!(request.context.tool_call_id.as_ref().map(|id| id.as_str()), Some("mcp-call")); }
#[tokio::test]
async fn resolving_settled_elicitation_reports_resolved() { assert!(matches!(second, Err(KernelError::ElicitationResolved(_)))); }
```

- [ ] **Step 2–4**：确认失败 → 实现 → `rtk cargo test -p kernel -p mcp`。
- [ ] **Step 5：检查点**：`feat(kernel): correlate elicitations with tool calls`。

### Task C2：ElicitationBridge

**文件：**

- 新增： `crates/acp/src/elicitation.rs`
- 修改： `crates/acp/src/projection.rs`（`EventGroup.elicitations`）
- 修改： `crates/acp/src/server.rs`（initialize 记录能力；watcher 调用 bridge；resume 后重建）
- 修改： `crates/acp/Cargo.toml`（`jsonschema = { workspace = true }`）
- 修改： `crates/protocol/src/identity.rs`、`crates/acp/src/extension.rs`（删除 `McpElicitationList`、`McpElicitationRespond`）
- 新增： `crates/acp/tests/elicitation.rs`

**接口：**

- 依赖接口： Task B1 事件、Task C1 上下文与错误。
- 提供接口：
  ```rust
  pub(crate) enum ElicitationChange { Requested(protocol::McpElicitationRequest), Resolved { request_id: String } }
  // EventGroup: #[builder(default)] elicitations: Vec<ElicitationChange>
  pub(crate) struct ElicitationModes { pub form: bool, pub url: bool }
  impl From<Option<&wire::ElicitationCapabilities>> for ElicitationModes;
  pub(crate) struct ElicitationBridge { /* modes, outstanding: Mutex<HashMap<(SessionId, String), RequestCancellationHandle>> */ }
  impl ElicitationBridge {
      pub(crate) fn request(&self, connection: &V2ConnectionTo<Client>, kernel: &Arc<Kernel>, session_id: &SessionId, request: &McpElicitationRequest);
      pub(crate) fn resolved(&self, session_id: &SessionId, request_id: &str);
      pub(crate) fn reestablish(&self, connection: &V2ConnectionTo<Client>, kernel: &Arc<Kernel>, session_id: &SessionId) -> Result<(), KernelError>;
  }
  /// Borrows the session and MCP request for an ACP-owned conversion.
  pub(crate) struct ElicitationOf<'a> { pub session_id: &'a SessionId, pub request: &'a McpElicitationRequest }
  impl From<ElicitationOf<'_>> for wire::CreateElicitationRequest; // session scope + optional toolCallId
  ```
- 测试夹具：参照 `crates/kernel/tests/mcp_host.rs:201-375` 的 `SamplingConnector`/`SamplingClient` 写 `ElicitingConnector`，其工具调用时发起一次 form elicitation；客户端用 `Client.v2().on_receive_request(..)` 处理 `elicitation/create`。

- [ ] **Step 1：写失败测试**（`crates/acp/tests/elicitation.rs`）

```rust
#[tokio::test]
async fn capable_client_receives_and_answers_elicitation() { /* accept {"strategy":"balanced"} → MCP tool output contains it */ }
#[tokio::test]
async fn second_connection_request_is_cancelled_after_first_answer() { /* receives $/cancel_request */ }
#[tokio::test]
async fn pending_elicitation_is_reestablished_after_resume() { /* reconnect → new elicitation/create with same message */ }
#[tokio::test]
async fn replay_does_not_resend_resolved_elicitation() {}
#[tokio::test]
async fn unknown_action_is_treated_as_cancel() { /* MCP receives cancel */ }
#[tokio::test]
async fn schema_invalid_accept_is_sent_as_cancel() {}
/// A schema the bridge cannot compile is cancelled without asking the client.
#[tokio::test]
async fn uncompilable_requested_schema_is_cancelled_without_request() { /* no elicitation/create; MCP receives cancel */ }
#[tokio::test]
async fn client_without_capability_receives_no_request() {}
#[tokio::test]
async fn late_response_after_turn_cancel_is_ignored() { /* Review Focus 3 */ }
```

- [ ] **Step 2–4**：确认失败 → 实现（spec C2 全部规则；`ElicitationResolved` 静默忽略；由本地 `ElicitationOf` 构造标准请求；`request` 保持同步签名，回复由 `connection.spawn` 的任务处理，watcher 不原地等待；发送前编译 `requestedSchema`，失败则直接以 cancel 回复 kernel 并 warn）→ `rtk cargo test -p acp`，验证转换在真实 ACP crate 中编译。
- [ ] **Step 5：检查点**：`feat(acp): bridge MCP elicitations through standard elicitation/create`。

### Task C3：WebUI 阶段 C

**文件：**

- 修改： `web/src/acp/connection.ts:183-205`（agent 请求注册表、`$/cancel_request`）
- 新增： `web/src/features/mcp/ElicitationDialog.tsx`、`web/src/features/mcp/elicitationForm.ts`（受限 schema 解析与校验）
- 修改： `web/src/workspace/controller.ts`（initialize 声明能力；注册处理器；移除 list/respond）
- 删除： `web/src/features/mcp/McpElicitationDialog.tsx`

**接口：**

- 提供接口： `AcpConnection.handle(method: string, handler: (params: unknown, signal: AbortSignal) => Promise<unknown>): void`；处理器抛出 `AcpRequestError` 时按其 code 回错误。

- [ ] **Step 1：实现**：表单字段类型与无法渲染时回 `-32602` 的规则见 spec C3；URL 模式显示 host，确认后 `window.open`；请求排队一次一个。
- [ ] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [ ] **Step 3：端到端验收**：测试数据目录配置一个发起 elicitation 的 MCP server；表单、URL、拒绝、取消；两个标签页先答者生效、另一页弹窗关闭；刷新后重建。
- [ ] **Step 4：检查点**：`feat(webui): answer standard elicitation requests`。

## 阶段 D：session config options

### Task D1：Kernel 设置原语

**文件：**

- 修改： `crates/kernel/src/runtime/settings.rs`（`SessionSettingsUpdate`）
- 修改： `crates/kernel/src/runtime/extension/host.rs:581-700`（改为调用它）
- 修改： `crates/kernel/src/runtime.rs`（公开 API、`SessionSettings`）
- 修改： `crates/protocol/src/event.rs`（`SettingsChanged`）
- 测试： `crates/kernel/tests/protocol_alignment/settings.rs`

**接口：**

- 提供接口：
  ```rust
  #[derive(Debug, Clone)] pub struct SessionSettings { pub model: ModelProfile, pub thinking_level: ThinkingLevel, pub models: Vec<ModelProfile> }
  impl Kernel {
      pub fn session_settings(&self, session_id: &SessionId) -> Result<SessionSettings, KernelError>;
      pub async fn set_session_model(&self, session_id: &SessionId, provider: &str, model_id: &str) -> Result<ModelProfile, KernelError>;
      pub async fn set_session_thinking_level(&self, session_id: &SessionId, level: ThinkingLevel) -> Result<ThinkingLevel, KernelError>;
  }
  // AgentEventPayload::SettingsChanged { model: ModelProfile, thinking_level: ThinkingLevel }
  ```

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn set_session_model_persists_and_emits_settings_changed() { /* resume → same model; sink saw SettingsChanged */ }
#[tokio::test]
async fn extension_model_change_emits_settings_changed() {}
#[tokio::test]
async fn unknown_model_is_rejected() {}
```

- [ ] **Step 2–4**：确认失败 → 实现 → `rtk cargo test -p kernel`（含 `extensions_host`、`session_extension_state` 回归）。
- [ ] **Step 5：检查点**：`refactor(kernel): share session settings updates`。

### Task D2：ACP config options

**文件：**

- 新增： `crates/acp/src/config.rs`
- 修改： `crates/acp/src/server.rs`（new/resume 响应 `config_options`；`SetSessionConfigOptionRequest` 处理器）
- 修改： `crates/acp/src/mapping.rs`（`SettingsChanged` → `ConfigOptionUpdate`）
- 测试： `crates/acp/tests/v2_alignment.rs`

**接口：**

- 依赖接口： Task A4 `ProjectionRegistry::last_sequence` 与当前连接的 `SessionDelivery::{wait_through, send_permit}`；Task D1 设置快照与修改 API。
- 提供接口： ACP 本地 `pub(crate) struct ConfigOptionsOf<'a>(pub &'a SessionSettings)` 与 `impl From<ConfigOptionsOf<'_>> for Vec<wire::SessionConfigOption>`；`pub(crate) enum ConfigChange { Model { provider: String, model_id: String }, ThinkingLevel(ThinkingLevel) }`；`impl TryFrom<(&SessionSettings, &wire::SetSessionConfigOptionRequest)> for ConfigChange`（目标为本地类型，错误映射 `-32602`；model 值与 `"{provider_id}/{model_id}"` 整体比较）。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn session_new_returns_model_and_thinking_options() { assert_eq!(opts[0]["configId"], "model"); assert_eq!(opts[1]["category"], "thought_level"); }
#[tokio::test]
async fn invalid_config_value_returns_32602() {}
#[tokio::test]
async fn model_id_with_slash_matches() { /* "openrouter/deepseek/deepseek-chat" */ }
#[tokio::test]
async fn config_option_update_precedes_response() {}
/// An unchanged value cannot create a wait for a SettingsChanged event that will never exist.
#[tokio::test]
async fn unchanged_config_does_not_wait_for_new_event() {}
#[tokio::test]
async fn config_change_during_run_applies_to_next_request() { /* Review Focus 4 */ }
/// Concurrent changes from two connections and an extension never leave a client on a stale value.
#[tokio::test]
async fn concurrent_config_changes_converge_on_every_connection() {
    // Connections 1 and 2 and a test extension repeatedly set different thinking levels concurrently.
    // After quiescence, folding each connection's config frames in arrival order equals kernel.session_settings().
    // Do not assert a fixed frame order: a response may land between update(A) and update(B) and already carry B.
}
```

- [ ] **Step 2–4**：确认失败 → 按 spec D2 用 `ConfigOptionsOf` 转换；Kernel 修改完成后捕获投影 `last_sequence` 并 `SessionDelivery::wait_through`，再取 `send_permit`，在许可内重新读取 Kernel 当前设置快照并发送响应，最后释放许可；无新事件时跳过等待但仍在许可内读取快照；watcher 失败正常返回错误 → `rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): expose model and thinking as session config options`。

### Task D3：WebUI 阶段 D

**文件：**

- 新增： `web/src/features/session/SessionConfigSelects.tsx`
- 修改： `web/src/shell/AppShell.tsx:207-210`、`web/src/workspace/controller.ts`、`web/src/workspace/updateDecoder.ts`（`config_option_update`）

- [ ] **Step 1：实现**：两个紧凑下拉框；请求中禁用；失败提示；无配置项回退固定模型名。
- [ ] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [ ] **Step 3：端到端验收**：切换模型与 thinking 后发送 prompt，助手元信息显示新模型；刷新保持。
- [ ] **Step 4：检查点**：`feat(webui): switch model and thinking from the header`。

## 阶段 E：terminal 实体

### Task E1：bash 退出信息与 `!bash` 实时输出

**文件：**

- 修改： `crates/protocol/src/tool.rs:190-197`（`ToolResultDetails::Bash` 增加 `exit_code`、`signal`）
- 修改： `crates/protocol/src/extension/result.rs`（`UserBashResult.signal`，共享执行器与扩展替代结果）
- 修改： `crates/protocol/src/message.rs`（`BashExecutionMessage.cwd`，新消息保存绝对路径，旧消息缺失可读）
- 修改： `crates/tools/src/bash.rs`、`crates/tools/src/builtin/bash.rs`
- 修改： `crates/protocol/src/event.rs`（`UserBashOutput`）、`crates/kernel/src/runtime/user_bash.rs`
- 修改： `crates/acp/src/projection.rs`（`UserBashOutput` 按 run_id 加入 Task A3 的 journal）
- 测试： `crates/tools/tests/bash.rs`、`crates/kernel/tests/protocol_alignment/insertion.rs`

**接口：**

- 依赖接口： Task A3 `UserBashStarted.cwd`、run_id/message_id 与 `UserBashFinished`；Task B3 持久化 cancelled 标记。
- 提供接口： `ToolResultDetails::Bash { truncation, full_output_path, #[serde(default, skip_serializing_if = "Option::is_none")] exit_code: Option<i32>, #[serde(default, skip_serializing_if = "Option::is_none")] signal: Option<String> }`；`AgentEventPayload::UserBashOutput { run_id: RunId, message_id: MessageId, output: String }`（完整尾部替换快照）；`UserBashResult.signal: Option<String>` 与 `BashExecutionMessage.cwd: Option<PathBuf>` 均加 `#[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)]`。
- 退出状态路径：在 `ExitStatus` 被丢弃前读取实际 unix signal 并命名（读取代码放在 `#[cfg(unix)]` 下，其他平台为 `None`），或由主动终止原因记录 SIGKILL → `BashExecutionOutput.result.signal` → 工具 details / 持久化 `BashExecutionMessage.result.signal`。不能从只有 Exited/Aborted/TimedOut 的 `BashTermination` 猜出任意实际信号；旧结果保持 None。

- [ ] **Step 1：写失败测试**

```rust
#[tokio::test]
async fn bash_details_carry_exit_code() { /* "exit 3" → exit_code Some(3), signal None */ }
#[tokio::test]
async fn timed_out_bash_reports_sigkill() { assert_eq!(signal.as_deref(), Some("SIGKILL")); }
#[tokio::test]
async fn user_bash_streams_output_events() { /* at least one UserBashOutput before completion */ }
/// A signal exit retains its actual cause in both shared and persisted shell results.
#[tokio::test]
async fn user_bash_sigterm_survives_persistence() { /* kill -TERM $$ -> signal Some("SIGTERM"), exit_code None */ }
/// Cancellation reports the signal used by the executor rather than dropping exit details.
#[tokio::test]
async fn stopped_user_bash_preserves_sigkill() {}
/// Error tool results retain their output and exit metadata.
#[tokio::test]
async fn failed_and_timed_out_bash_keep_terminal_details() { /* exit 3 and timeout; is_error true; details retained */ }
/// The live start and durable result agree on the original absolute working directory.
#[tokio::test]
async fn user_bash_persists_started_cwd() {}
/// Old shell records remain readable without invented exit signals or working directories.
#[test]
fn old_bash_message_defaults_missing_cwd_and_signal_to_none() {}
```

- [ ] **Step 2–4**：确认失败 → 按 spec E1 统一事件字段为 `output: String`，传播 run_id、cwd、signal；非零退出/超时/中止用保留 details 的错误 ToolResult，保留原错误文本与 is_error，不能退化为只含文本的 ToolError；同步所有 UserBashResult、BashExecutionMessage、Bash details 构造与 fixtures → `rtk cargo test -p tools -p kernel -p acp`。
- [ ] **Step 5：检查点**：`feat: report bash exit status and stream user bash output`。

### Task E2：ACP terminal 映射

**文件：**

- 新增： `crates/acp/src/terminal.rs`
- 修改： `crates/acp/src/mapping.rs`（bash 工具开始/更新/结束、`UserBash*`、回放；bash 进度只发 `TerminalUpdate`；删除 `BashExecution` 的 `_clawcode/event` 投影）
- 修改： `crates/acp/src/projection.rs`（`EventGroup.snapshot_of`；journal 同键替换，spec E3.1）
- 测试： `crates/acp/tests/mapping.rs`（替换 `user_bash_replay_preserves_blocked_disposition` 的断言）
- 测试： `crates/acp/tests/v2_alignment.rs`（运行中重连恢复 terminal 输出、持久化信号回放）

**接口：**

- 提供接口： `EventGroup { .., #[builder(default)] snapshot_of: Option<String> }`（值为 terminalId，仅实时输出快照组设置）；`pub(crate) struct TerminalSnapshot<'a> { pub terminal_id: &'a str, pub output: &'a str }`；`impl From<TerminalSnapshot<'_>> for wire::TerminalUpdate`（base64 编码）；`pub(crate) struct TerminalExit<'a> { pub exit_code: Option<i32>, pub signal: Option<&'a str> }`。

- [ ] **Step 1：写失败测试**

```rust
#[test]
fn bash_tool_maps_to_terminal_lifecycle() {
    // start: tool_call_update.content[0] == {"type":"terminal","terminalId":id} then terminal_update{command,cwd}
    // update: terminal_update.output.data == base64(tail)
    // end: terminal_update.exitStatus.exitCode == 0 before final tool status
}
#[test]
fn user_bash_maps_to_tagged_tool_call_and_terminal() { assert_eq!(json["_meta"]["clawcode"]["origin"], "user"); }
#[test]
fn bash_replay_emits_final_terminal_snapshot() {}
#[test]
fn terminal_output_round_trips_unicode_and_ansi() { /* Review Focus 5: "中文🙂\x1b[31mred\x1b[0m" */ }
/// Resuming a live shell reconstructs its terminal from the same journal as its user message.
#[tokio::test]
async fn running_user_bash_resume_restores_latest_terminal_output() { /* same ids; full tail snapshot; running state */ }
/// Durable replay preserves cwd, SIGTERM, and SIGKILL instead of inferring them from current state.
#[tokio::test]
async fn user_bash_terminal_replay_preserves_cwd_and_signal() {}
/// An executor failure closes the displayed terminal and tool before idle(error).
#[test]
fn user_bash_failure_without_result_emits_terminal_and_tool_end() { /* result_emitted=false; exitStatus {}; failed tool then idle(error) */ }
/// Cancellation before an executor result still closes already accepted terminal/tool entities.
#[test]
fn user_bash_cancellation_without_result_emits_cancelled_tool_end() { /* result_emitted=false; exitStatus {}; cancelled tool then idle(cancelled) */ }
/// A normal result already emitted the terminal/tool updates before the foreground finish event.
#[test]
fn user_bash_finished_with_result_does_not_duplicate_tool_end() { /* result_emitted=true -> only idle state_update */ }
/// Progress carries the tail once instead of duplicating it in tool content and raw output.
#[test]
fn bash_progress_maps_only_to_terminal_update() {}
/// A long-running shell keeps one retained snapshot per terminal (projection.rs mod tests).
#[test]
fn journal_keeps_only_latest_terminal_snapshot() { /* 1000 snapshots -> start + latest snapshot retained, in sequence order */ }
/// A cursor that points at a replaced snapshot resumes from the latest snapshot.
#[test]
fn cursor_into_replaced_snapshot_replays_latest() {}
```

- [ ] **Step 2–4**：确认失败 → 实现 spec E2、E3，消费 Task E1 的统一 output/cwd/signal 契约；`MessageEnd` 生成正常 terminal/tool 终态，`UserBashFinished` 用 result_emitted 区分仅发 idle 与无结果时按 outcome 补 failed/cancelled 终态；保留扩展阻止执行的展示语义；`shortcut:` 注释说明不发 chunk；按 spec E3.1 给实时输出快照组设置 `snapshot_of`，journal 追加时删除同键旧组再追加，开始与终态组不设键；`shortcut:` 注释说明出站队列不合并快照 → `rtk cargo test -p acp`。
- [ ] **Step 5：检查点**：`feat(acp): project bash output as terminal entities`。

### Task E3：WebUI 阶段 E

**文件：**

- 新增： `web/src/features/conversation/TerminalView.tsx`、`web/src/workspace/terminals.ts`（状态与解码）
- 修改： `web/src/workspace/updateDecoder.ts`（`terminal_update`、`terminal_output_chunk`；删除 `bash_execution` 解析）
- 修改： `web/src/features/conversation/ToolCallCard.tsx`、`web/src/features/conversation/Conversation.tsx`
- 删除： `web/src/features/conversation/BashExecutionCard.tsx`

- [ ] **Step 1：实现**：按 spec E4；ANSI 剥离使用一个保留跨块状态的小解析器；`origin === "user"` 卡片显示「用户执行」。
- [ ] **Step 2：构建**：`cd web && rtk npm run check && rtk npm run build`。
- [ ] **Step 3：端到端验收**：bash 工具与 `!bash` 的实时输出、退出码、停止后的 signal；长命令运行中刷新恢复输出；SIGTERM/SIGKILL 退出后刷新仍显示保存的信号，cwd 不随当前目录变化。
- [ ] **Step 4：检查点**：`feat(webui): render bash output as terminals`。

## 收尾

- [ ] 全量验证：`rtk cargo fmt --all -- --check`、`rtk cargo clippy --workspace --tests --examples -- -Dwarnings`、`rtk cargo test --workspace`、`cd web && rtk npm run check && rtk npm run build`。
- [ ] 验收记录写入 `docs/superpowers/reports/2026-10-29-acp-1-13-alignment-verification.md`（中文，按阶段列出场景与结果）。

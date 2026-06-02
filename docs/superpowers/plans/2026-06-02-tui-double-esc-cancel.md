# TUI Double Esc Cancel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 支持 TUI 当前 active session 连续两次 `Esc` 通过 kernel `Op::Cancel` 取消当前 turn，并正确唤醒等待 subagent 的 parent。

**Architecture:** TUI 只负责双 `Esc` 确认和 active session 选择；ACP cancel notification 继续作为 UI 到 agent 的协议入口；kernel `cancel()` 改为发送 `Op::Cancel`，session runtime 把 cancel 作为 turn terminal outcome 处理而不是关闭 thread。

**Tech Stack:** Rust 2024, tokio, crossterm, ACP, kernel session runtime, typed-builder.

---

## 文件结构

- 修改 `crates/tui/src/app.rs`：增加双 `Esc` 确认状态、active session 可取消判断、TUI 按键测试。
- 修改 `crates/tui/src/ui/state.rs`：暴露 active agent status 是否可取消的判断，避免 TUI 只依赖 root prompt 状态。
- 修改 `crates/kernel/src/lib.rs`：让 `Kernel::cancel()` 发送 `Op::Cancel`。
- 修改 `crates/kernel/src/session.rs`：把 `Op::Cancel` 处理为 turn cancel outcome，发送 cancelled event，通知 subagent interrupted，并保留 runtime。
- 修改 `crates/kernel/src/thread_manager.rs`：补充 `send_op`/cancel 行为测试，保留 watch cancel 兼容能力。

## Task 1: Kernel cancel 进入 Op::Cancel

**Files:**
- Modify: `crates/kernel/src/lib.rs`
- Test: `crates/kernel/src/thread_manager.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/kernel/src/thread_manager.rs` 中新增测试，证明 `send_op` 可以把 `Op::Cancel` 发到 session runtime channel，作为后续 `Kernel::cancel()` 的目标路径。

```rust
#[tokio::test]
async fn send_op_routes_cancel_to_session_runtime() {
    let manager = ThreadManager::new();
    let session_id = SessionId::from("session");
    let (tx_op, mut rx_op) = mpsc::unbounded_channel();
    let thread = test_thread(session_id.clone(), tx_op);
    manager.insert_thread(thread).await;

    manager
        .send_op(
            &session_id,
            Op::Cancel {
                session_id: session_id.clone(),
            },
        )
        .await
        .expect("send cancel op");

    let op = rx_op.recv().await.expect("cancel op");
    assert!(matches!(op, Op::Cancel { session_id: id } if id == session_id));
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p kernel send_op_routes_cancel_to_session_runtime`

Expected: 测试通过后再继续，因为这个测试证明底层 `send_op` 能传递 cancel；`Kernel::cancel()` 的失败点由后续行为测试覆盖。

- [ ] **Step 3: 修改 `Kernel::cancel()`**

把 `crates/kernel/src/lib.rs` 中的 `cancel()` 改为：

```rust
async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
    self.thread_manager
        .send_op(
            session_id,
            Op::Cancel {
                session_id: session_id.clone(),
            },
        )
        .await
}
```

- [ ] **Step 4: 运行 targeted kernel 测试**

Run: `rtk cargo test -p kernel send_op_routes_cancel_to_session_runtime`

Expected: PASS。

## Task 2: Session runtime 将 cancel 视为 interrupted turn

**Files:**
- Modify: `crates/kernel/src/session.rs`
- Test: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: 写 parent mailbox 唤醒测试**

在 `crates/kernel/src/agent/control.rs` 已有 `notify_child_terminal_turn` 测试基础上补充 `AgentStatus::Interrupted` 覆盖，确认 interrupted 会标记 mailbox pending。

```rust
#[tokio::test]
async fn interrupted_child_terminal_turn_marks_parent_mailbox_pending() {
    let (control, parent_id, child_id, parent_queue) =
        test_control_with_parent_and_child().await;

    control
        .notify_child_terminal_turn(&child_id, AgentStatus::Interrupted)
        .await
        .expect("notify interrupted child");

    let pending_rx = control
        .subscribe_session_mailbox_activity(&parent_id)
        .await
        .expect("parent mailbox subscription");
    assert!(
        pending_rx.has_changed().expect("mailbox watcher open"),
        "wait_agent must observe interrupted child terminal notification"
    );

    let messages = parent_queue.lock().await.drain_mailbox_input_items();
    assert!(messages
        .iter()
        .any(|message| message.content.contains("interrupted")));
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p kernel interrupted_child_terminal_turn_marks_parent_mailbox_pending`

Expected: 如果已有行为已满足则 PASS；否则 FAIL 并定位缺口。

- [ ] **Step 3: 修改 `TurnStepOutcome`**

在 `crates/kernel/src/session.rs` 中把 `TurnStepOutcome` 扩展为：

```rust
enum TurnStepOutcome {
    /// Turn finished executing (success or error).
    Finished(AgentStatus),
    /// Current turn was cancelled by the user.
    Cancelled,
    /// Session should shut down.
    Shutdown,
}
```

并将 `Op::Cancel` 分支改为返回 `TurnStepOutcome::Cancelled`，`Op::CloseSession` 和 channel close 继续返回 `Shutdown`。

- [ ] **Step 4: 在 prompt 和 inter-agent turn 外层处理 Cancelled**

在 prompt turn 和 inter-agent turn 的 match 中新增 `Cancelled` 分支：

```rust
TurnStepOutcome::Cancelled => {
    notify_cancelled_turn(&mut rt, &ctx, &tx).await;
}
```

其中 `notify_cancelled_turn` 是 session 模块私有 async 函数，负责发送 `StopReason::Cancelled`、持久化 aborted、通知 `AgentStatus::Interrupted`。函数级注释必须使用英文。

- [ ] **Step 5: 运行 targeted session/agent 测试**

Run: `rtk cargo test -p kernel interrupted_child_terminal_turn_marks_parent_mailbox_pending`

Expected: PASS。

## Task 3: TUI 双 Esc 取消 active session

**Files:**
- Modify: `crates/tui/src/app.rs`
- Modify: `crates/tui/src/ui/state.rs`

- [ ] **Step 1: 新增 active 状态判断**

在 `AppState` 中新增函数：

```rust
/// Returns whether the current session can receive a user cancellation request.
pub fn is_cancelable(&self) -> bool {
    self.running_prompt
        || matches!(
            self.agent_status,
            Some(AgentStatus::PendingInit | AgentStatus::Running)
        )
}
```

- [ ] **Step 2: 新增 EscCancelState**

在 `crates/tui/src/app.rs` 中新增小结构：

```rust
/// Tracks the two-step Escape confirmation for cancelling a running session.
#[derive(Debug, Default)]
struct EscCancelState {
    /// Last Escape press while the active session was cancelable.
    last_escape: Option<time::Instant>,
}
```

并提供带英文函数级注释的方法 `register_escape` 和 `clear`。

- [ ] **Step 3: 修改 `run_loop` 和 `handle_key_event` 签名**

`run_loop` 持有 `let mut esc_cancel = EscCancelState::default();`，调用 `handle_key_event` 时传 `&mut esc_cancel`。

- [ ] **Step 4: 修改 Esc 分支**

当 `ui.router.active_state().is_cancelable()` 为 true 时，第一次 `Esc` 不退出；第二次 `Esc` 调用 `client.cancel(ui.router.active_session_id().clone())`。非 cancelable 时保留原有 `Esc` 退出行为。

- [ ] **Step 5: 运行 TUI 测试**

Run: `rtk cargo test -p tui`

Expected: PASS。

## Task 4: 验证

**Files:**
- Verify only.

- [ ] **Step 1: 格式化**

Run: `rtk cargo fmt`

Expected: exit 0。

- [ ] **Step 2: targeted tests**

Run: `rtk cargo test -p kernel interrupted_child_terminal_turn_marks_parent_mailbox_pending`

Expected: PASS。

Run: `rtk cargo test -p tui`

Expected: PASS。

- [ ] **Step 3: 检查 diff**

Run: `rtk git diff -- crates/tui/src/app.rs crates/tui/src/ui/state.rs crates/kernel/src/lib.rs crates/kernel/src/session.rs crates/kernel/src/thread_manager.rs crates/kernel/src/agent/control.rs docs/superpowers/specs/2026-06-02-tui-double-esc-cancel-design.md docs/superpowers/plans/2026-06-02-tui-double-esc-cancel.md`

Expected: diff 只包含本需求相关改动，所有新增/修改代码的非平凡逻辑和新增函数都有英文注释。

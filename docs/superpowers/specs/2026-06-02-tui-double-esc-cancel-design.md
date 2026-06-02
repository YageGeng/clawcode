# TUI 双 Esc 触发 Kernel Cancel 设计

## 背景

当前 TUI 在 prompt 运行时支持通过 `AcpClient::cancel` 发出 ACP cancel notification，但 kernel 的 `cancel()` 实现直接触发 `ThreadManager::cancel_thread()` 的 `cancel_tx`，没有向 session runtime 发送 `Op::Cancel`。同时，`Esc` 在非运行状态会退出 TUI，运行状态下没有双击确认取消语义。

subagent 场景还有额外约束：父 agent 可能正在执行 `wait_agent`，等待子 agent 的 terminal notification。如果取消子 agent 只停止 child event stream 或直接结束 child runtime，而不通过 `notify_child_terminal_turn(child, AgentStatus::Interrupted)` 通知父 session mailbox，父 agent 可能无法被唤醒。

## 目标

1. TUI 在当前 active session 可取消时，连续按两次 `Esc` 触发取消。
2. 取消必须进入 kernel 的 `Op::Cancel { session_id }` 操作路径。
3. root/main agent 和 subagent 都按当前 active session 作为取消目标。
4. 取消 subagent 时，父 session 的 `wait_agent` 必须被 mailbox update 唤醒。
5. 取消当前 turn 后保留 session runtime，允许后续继续发送 prompt 或 inter-agent message。

## 非目标

1. 不实现父子级联取消。
2. 不把 root active 时的双 `Esc` 隐式转发到正在运行的 child。
3. 不修改 `/agent`、`/model`、approval overlay 的按键语义。
4. 不执行 `git commit`。

## 设计

### TUI 输入语义

TUI 新增一个短生命周期的取消确认状态，记录最近一次 `Esc` 的时间。

当 active session 可取消时：

1. 第一次 `Esc` 只记录确认状态，不退出 TUI。
2. 第二次 `Esc` 如果落在确认窗口内，调用 `AcpClient::cancel(active_session_id)`。
3. 如果超过确认窗口，新的 `Esc` 重新作为第一次确认。
4. 其他普通按键会清除确认状态，避免旧的 `Esc` 意外影响后续输入。

active session 可取消的判断覆盖两类状态：

1. root/main prompt 正在运行：`AppState::is_running_prompt() == true`。
2. subagent 外部状态为 `AgentStatus::PendingInit | AgentStatus::Running`。

### Kernel Cancel 语义

`Kernel::cancel(session_id)` 改为向对应 thread 发送：

```rust
Op::Cancel {
    session_id: session_id.clone(),
}
```

`ThreadManager::cancel_thread()` 保留取消 watch 的能力，但 session runtime 的正式取消入口使用 `send_op(..., Op::Cancel { ... })`。

### Session Runtime 语义

`run_turn_select_loop` 收到 `Op::Cancel` 时不返回 `Shutdown`。它应返回新的取消结果，外层负责：

1. 发送 `Event::turn_complete(session_id, StopReason::Cancelled)`。
2. 持久化 turn aborted 记录。
3. 对 subagent 调用 `notify_terminal_turn(..., AgentStatus::Interrupted)`。
4. 保持 session runtime loop 继续运行。

`Op::CloseSession` 仍然是关闭 runtime 的操作，不能与 `Op::Cancel` 混淆。

### Subagent Wait 唤醒

subagent 被取消后必须走现有 `notify_child_terminal_turn(child, AgentStatus::Interrupted)`。这个函数会：

1. 更新 child registry status。
2. 唤醒 child status watcher。
3. 向 parent session mailbox enqueue terminal notification。

因此 root/parent 正在执行 `wait_agent` 时，会通过 `subscribe_session_mailbox_activity(parent_session_id)` 收到变化并返回 `"Wait completed."`。

## 测试策略

1. kernel：验证 `Kernel::cancel()` 发送 `Op::Cancel`，而不是只触发 `cancel_tx`。
2. kernel/session：验证运行中的 child 收到 `Op::Cancel` 后通知 parent mailbox，并将 child 状态变为 `Interrupted`。
3. kernel/session：验证 cancel 后 thread 不被关闭，后续仍能接收新的 `Op::Prompt` 或 inter-agent message。
4. TUI：验证 active root 运行时第一次 `Esc` 不取消，第二次 `Esc` 取消。
5. TUI：验证 active subagent `AgentStatus::Running` 时双 `Esc` 触发 cancel，而不是退出 TUI。

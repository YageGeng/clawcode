# `wait_agent` 通知交付缺口根因分析

## 现象

多个 agent 在极短时间内（几乎同时）完成时，`wait_agent` 只观察到部分完成通知，其余通知可能稍后被注入父 session 历史，但不再被 `wait_agent` 当作新的 mailbox update 观察到。后续 wait 直接返回 `"No live agents to wait for"`——agent 确实完成了，但父 agent 的 `wait_agent` 唤醒/交付语义丢失。

**复现率**：3 次压力测试中触发 2 次（4-agent 并发场景）。

## 数据流总览

```
子 agent 完成
  │
  ▼
notify_terminal_turn (session.rs:551)
  │
  ▼
notify_child_terminal_turn (control.rs:420)
  ├─ ① 更新 registry status → Completed  ──────────────────────┐
  ├─ ② 通知 status_watchers                                    │
  ├─ ③ 构建 InterAgentMessage { trigger_turn: false }           │
  └─ ④ thread_manager.send_op → 父 session 的 rx_op channel    │
                                                                 │
父 session run_loop                                              │
  ├─ run_turn_select_loop (tokio::select!)  ← 并发消费 rx_op    │
  │    ├─ branch 1: &mut turn (turn 完成)                       │
  │    └─ branch 2: rx_op.recv() → enqueue 到 input_queue       │
  │                                                              │
  ├─ drain_pending_inter_agent_messages ← 消费 input_queue       │
  │                                                              │
  └─ 外层 loop: rx_op.recv() → 延迟送达的 op 在此被 enqueue     │
                                             │
                    ┌────────────────────────┘
                    ▼
              wait_agent 检查:                                   │
              list_agents → is_final() → 不视为 live ◄─── 步骤①已执行
              has_changed() → false (input_queue 为空)
              → "No live agents to wait for"
```

## 三个互相配合的根因机制

### 机制 1：turn 完成分支返回后剩余 `rx_op` 未入队

**位置**：`crates/kernel/src/session.rs:396-468`

```rust
tokio::select! {
    result = &mut turn => {           // ← 第一分支（turn 完成）
        ...
    }
    op = rt.rx_op.recv() => match op { // ← 第二分支（入站 op）
        ...
    }
}
```

Tokio 的 `select!` 默认并不会偏向第一分支；当前 Tokio 默认会随机选择起始 polling 分支，只有显式写 `biased;` 才会按源码顺序从上到下 poll。这里的问题不是固定优先级偏斜，而是当 `wait_agent` 返回、model turn 结束的同时，还有子 agent 的完成通知在 `rx_op` channel 中排队时，如果本轮 `select!` 选中了 turn 完成分支，`run_turn_select_loop` 会立即返回，剩余 op 留在 channel 中未被消费进 `input_queue`。

**复现条件**：≥3 个 agent 几乎同时完成 + 父 agent 的 turn 也在同时结束。op 抵达时机与 turn 完成时机碰撞；是否触发取决于该轮 `select!` 的实际 polling/ready 选择，而不是固定偏向第一分支。

### 机制 2：`drain_pending_inter_agent_messages` 时序缺口

**位置**：`crates/kernel/src/session.rs:338 vs 261`

```
run_loop:
  run_turn_select_loop(...)  ← 某些 op 留在了 rx_op channel 里
  notify_terminal_turn(...)  ← 无影响
  while ... take_next_triggering_message()  ← 无影响（trigger_turn: false）
  drain_pending_inter_agent_messages()  ← 只 drains 已被 select-loop enqueue 的消息
  ── 时序缺口 ──
  loop: rt.rx_op.recv().await  ← 这里才收到留在 channel 里的 op
       → enqueue 到 input_queue  ← 但 drain 已经过了！
```

留在 channel 里的 op 最终会被外层 loop 收到并 enqueue，但 `drain_pending_inter_agent_messages` 已经执行过了。下一次 turn 开始时（line 304），初始化 drain 会把它们注入历史：

```rust
Some(Op::Prompt { .. }) => {
    drain_pending_inter_agent_messages(...)  // ← 这里注入历史，但 wait_agent 不再观察到新 update
    run_turn_select_loop(...)  // wait_agent 在其中执行
}
```

此时 `wait_agent` 订阅到的是空队列，因此 `has_changed()` 为 false。

### 机制 3：Status 更新先于通知送达

**位置**：`crates/kernel/src/agent/control.rs:425-485`

```rust
// 步骤 1: 先更新 status — agent 变为 Completed (is_final = true)
self.registry.update_agent_status(child_session_id, status.clone());
//    ... 中间还有很多逻辑 ...
// 步骤 5: 最后才发送通知给父 session
self.thread_manager.send_op(&parent_session_id, Op::InterAgentMessage { message }).await?;
```

步骤 1 和步骤 5 之间有显著的时间差。在此期间，父 agent 调用 `list_agents` 会看到子 agent 已经是 `is_final()` → 不算 "live"。

配合机制 1 和 2：当通知最终送达父 session 时（可能在外层 loop 中进入 `input_queue`，随后在下一 turn 的 init drain 中被注入历史），`wait_agent` 早已检查过 `list_agents` 并判定 "无 live agent"，返回了 `"No live agents to wait for"`。

## 端到端的丢失场景

以 4 个 agent (c1-c4) 同时完成为例，完整时间线：

| 时间 | 事件 |
|------|------|
| T1 | c1-c4 全部调用 `notify_child_terminal_turn` |
| T2 | 4 个 agent 的 registry status 全部更新为 Completed |
| T3 | 4 个 `Op::InterAgentMessage` 通过 `send_op` 发往 root 的 `rx_op` channel |
| T4 | root 的 `run_turn_select_loop` 处理了其中 1 个 op（比如 c4），enqueue 进 input_queue |
| T5 | 此时 root 的 turn 完成（wait_agent 返回）——本轮 `select!` 选中 turn 完成分支，其余 3 个 op 留在 channel |
| T6 | `drain_pending_inter_agent_messages` 只 drain 了 c4 |
| T7 | 用户看到 c4 的 `inter_agent_communication` |
| T8 | 外层 loop `rt.rx_op.recv().await` 收到 c1-c3 的 op，enqueue 进 input_queue |
| T9 | 下次 turn 开始，init drain（line 304）把 c1-c3 注入历史，但不再触发 `wait_agent` |
| T10 | `wait_agent` 检查：`has_changed()` = false, `list_agents` 无 live agent → **"No live agents to wait for"** |

## 受影响的关键路径

三个文件，五个关键位置：

| 文件 | 行号 | 问题 |
|------|------|------|
| `session.rs` | 396-468 | turn 完成分支立即返回，剩余 `rx_op` 可能未入队 |
| `session.rs` | 338 | `drain` 在 op 送达之前执行 |
| `session.rs` | 304 | 下次 turn 的 init drain 注入延迟送达的通知，但 `wait_agent` 不再观察到新 update |
| `control.rs` | 425-426 | status 更新在通知发送之前 |
| `control.rs` | 482-485 | 通知发送在 status 更新之后 |

## 可能的修复方向

1. **在 `run_turn_select_loop` 返回前 drain 一次 `rx_op`**：在 `return` 之前用 `try_recv()` 把 channel 中剩余的 op 消费掉并入队，确保所有到达的 op 都在 drain 之前被处理。

2. **在外层 `run_loop` 的 drain 之前先 drain channel**：line 338 之前用 `try_recv()` 检查并处理残留 op，填补 drain 和后续 recv 之间的时序缺口。

3. **谨慎评估 `tokio::select!` 的 `biased` 模式**：默认 `select!` 不是第一分支优先；如果改成 `biased;` 并把 `rx_op.recv()` 放在前面，只能缓解 ready 碰撞，仍需要评估连续入站 op 对 turn 完成分支的公平性。更稳妥的是在 turn 完成后、任何 mailbox drain 前显式 flush 已到达的 `rx_op`。

4. **调整 `notify_child_terminal_turn` 的顺序**：先 `send_op` 再 `update_agent_status`，消除 status-update 和 notification 之间的窗口，确保通知到达时 agent 仍被视为 live。

# ACP 流式文本 Envelope 合并实施计划

> **供执行代理使用：** 必须使用 `superpowers:executing-plans` 在当前会话中逐项执行本计划；项目禁止使用子代理和 worktree。步骤使用复选框（`- [ ]`）跟踪。

**目标：** 将当前已经排队、属于同一文本块的连续 ACP v2 Assistant message/thought 流式 chunk 拼接为一条 notification，减少重复 envelope，同时保持 ToolCall、journal 和断线恢复语义不变。

**架构：** 在现有 frame-aware batching adapter 内增加一个专注于 typed `session/update` 的文本 coalescer。coalescer 只改写待发送副本，不修改 Projection journal；合并通知使用第一条事件 sequence 作为区间起点，并通过 `lastSequence` 标记原子提交终点，前端据此推进 cursor。

**技术栈：** Rust 2024、`agent-client-protocol` v2 typed schema、Serde JSON、futures channel、TypeScript 6、React/Zustand、生产 WebSocket WebUI。

**Spec：** `docs/superpowers/specs/2026-08-21-acp-websocket-recovery-design.md`

## 全局约束

- spec 和 plan 使用中文；所有新增函数与非平凡代码注释使用英文。
- Rust 修改遵循 TDD；测试只能放在 crate 级 `tests/` 或标准 `#[cfg(test)] mod tests`。
- 前端代码免于 TDD，但必须通过 TypeScript、ESLint 和生产构建。
- 超过三个字段的 Rust struct 使用 `typed-builder`，`Option` 字段使用 `#[builder(default)]`。
- 不新增第三方依赖，不修改持久化 Store，不压缩 Operation journal。
- 只合并当前已排队的连续 chunk，不增加 timer 或等待窗口。
- ToolCall、Tool result、Tool update、MessageEnd 和生命周期事件保持原数量与顺序。
- 所有 shell 命令使用 `rtk` 前缀。
- 未经用户再次明确授权，不创建 commit。

---

### Task 1：扩展恢复分组元数据解码

**Files：**
- Modify: `web/src/acp/protocol.ts`
- Modify: `docs/superpowers/specs/2026-08-21-acp-websocket-recovery-design.md`

**Interfaces：**
- Produces: coalescer 在 transport 副本中写入 `lastSequence`，常规 `GroupMeta` 与 journal 保持不变。
- Produces: TypeScript `ProjectionGroupMeta.lastSequence?: EventSequence`。
- Invariant: 缺少 `lastSequence` 等价于当前事件 `sequence`。

- [x] **Step 1：扩展 TypeScript 解码接口**

在 `ProjectionGroupMeta` 增加：

```ts
lastSequence?: EventSequence;
```

在 `AcpProtocol.projectionGroupMeta` 中仅接受正安全整数，并保持字段可选：

```ts
|| (group.lastSequence !== undefined && !this.validSequence(group.lastSequence))
```

返回值中仅在合法字段存在时附加：

```ts
...(this.validSequence(group.lastSequence)
  ? { lastSequence: group.lastSequence as EventSequence }
  : {})
```

- [x] **Step 2：运行前端静态检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 2：按 typed ACP chunk 合并待发送 notification

**Files：**
- Create: `crates/acp/src/coalescing.rs`
- Modify: `crates/acp/src/lib.rs`
- Modify: `crates/acp/src/batching.rs`

**Interfaces：**
- Produces: transport 副本外层恢复元数据的 JSON 字段 `lastSequence`。
- Produces: `pub(crate) fn coalesce_updates(messages: Vec<RawJsonRpcMessage>) -> Vec<RawJsonRpcMessage>`；不合法或不兼容的输入是透传边界，不是 adapter 错误。
- Produces: 一个内部 `StreamChunk` 类型，负责 eligibility、相等 merge key、文本拼接和重新编码。
- Invariant: 输入消息顺序不变；只有相邻 eligible chunk 可以折叠。

- [ ] **Step 1：写 Agent message chunk 合并失败测试**

在 `crates/acp/src/coalescing.rs` 的标准测试模块中构造两个完整 typed `UpdateSessionNotification`：相同 SessionId、RunId、TurnId、MessageId，sequence 分别为 41 和 42，正文分别为 `"Hel"` 与 `"lo"`。调用 `coalesce_updates` 后断言：

```rust
assert_eq!(messages.len(), 1);
assert_eq!(decoded_text(&messages[0]), "Hello");
assert_eq!(event_sequence(&messages[0]), 41);
assert_eq!(recovery_last_sequence(&messages[0]), Some(42));
```

测试 fixture 必须通过 `wire::UpdateSessionNotification`、`wire::SessionUpdate::AgentMessageChunk` 和 `GroupMeta` 构造真实 ACP 参数，不手写不完整协议对象。

- [ ] **Step 2：运行测试并确认失败**

Run: `rtk cargo test -p acp coalesces_agent_message_chunks`

Expected: FAIL，`coalescing` 模块或 `coalesce_updates` 尚不存在。

- [ ] **Step 3：实现 typed eligibility 与合并**

实现 `StreamChunk::try_from_message`，只接受：

```rust
wire::SessionUpdate::AgentMessageChunk(chunk)
wire::SessionUpdate::AgentThoughtChunk(chunk)
```

并校验：

```text
session_id 相同
chunk variant 相同
message_id 相同
outer product metadata 的 runId、turnId 合法且相同
projectionIndex == 0
projectionCount == 1
operationPhase 缺失
content 是 Text
后一 sequence > 前一 sequence
```

`StreamChunk::try_merge` 只拼接 Text 正文，并把来源 sequence 写入候选 notification 的 `sessionRecovery.lastSequence`。第一次成功合并时也要明确写入最终终点；没有发生合并时返回原始消息，避免为单 chunk 改变 wire 输出。annotations 必须相同，chunk 级事件元数据保留区间首事件，避免丢弃有差异的文本语义标注。

所有新增函数增加英文函数级注释；解析失败或不符合条件不是协议错误，而是合并边界，原消息必须原样透传。

- [ ] **Step 4：运行首个测试并确认通过**

Run: `rtk cargo test -p acp coalesces_agent_message_chunks`

Expected: PASS。

- [ ] **Step 5：补充边界失败测试**

依次增加并先运行确认至少一个新断言失败：

```text
coalesces_agent_thought_chunks
does_not_merge_different_message_ids
does_not_merge_message_and_thought_chunks
does_not_merge_non_text_content
does_not_merge_multi_update_projection_groups
does_not_merge_operation_boundaries
tool_call_update_breaks_chunk_coalescing
preserves_order_around_non_mergeable_updates
```

ToolCall 测试输入顺序必须为 chunk、ToolCall update、chunk，输出仍为三条且 ToolCall 位于中间。

- [ ] **Step 6：实现边界并跑 coalescer 测试**

Run: `rtk cargo test -p acp coalescing::tests`

Expected: 全部 PASS。

- [ ] **Step 7：接入现有 batch drain**

`forward_outbound` 继续最多从 source 消耗 `max_batch_size` 条原始 `session/update`。drain 完成后调用：

```rust
let mut messages = coalesce_updates(messages);
```

随后按现有规则输出 Single 或 `TransportBatch`。因此：

- 单个未合并 update 仍是 Single；
- 多个普通 update 仍是标准 JSON-RPC batch；
- 多个同文本块 chunk 可以缩成一个 Single；
- 原始消费数量和最终 batch 成员数都不超过 `max_batch_size`。

- [ ] **Step 8：补充 batching 集成测试**

在 `crates/acp/src/batching.rs` 现有测试模块使用真实 chunk fixture，覆盖：

```text
forward_outbound_coalesces_queued_text_chunks
forward_outbound_limits_coalescing_to_batch_size
forward_outbound_keeps_tool_call_boundary
```

`max_batch_size = 2`、三个连续同块 chunk 的期望是两个 frame：第一帧正文为前两个 delta 拼接结果，第二帧为第三个原始 chunk。

- [ ] **Step 9：运行 ACP 测试**

Run: `rtk cargo test -p acp`

Expected: 全部 PASS。

---

### Task 3：让前端 cursor 提交合并区间终点

**Files：**
- Modify: `web/src/workspace/recovery.ts`
- Modify: `web/src/acp/protocol.ts`

**Interfaces：**
- Consumes: `ProjectionGroupMeta.lastSequence?: EventSequence`。
- Produces: `PendingGroup.lastSequence: EventSequence`。
- Invariant: 普通通知提交 `sequence + 1`；合并通知提交 `lastSequence + 1`。

- [ ] **Step 1：扩展 PendingGroup**

将 `PendingGroup` 增加：

```ts
lastSequence: EventSequence;
```

在 `accept` 中计算并校验：

```ts
const lastSequence = group.lastSequence ?? sequence;
if (!AcpProtocol.validSequence(lastSequence) || lastSequence < sequence) {
  this.requireFullReplay(notification.sessionId);
  throw new Error("ACP projection sequence range is invalid");
}
```

- [ ] **Step 2：保持原子组一致性**

创建 pending group 时保存 `lastSequence`；同一 group 后续 notification 必须具有相同的起止 sequence、count 和 phase，否则沿用当前 full replay 错误路径。虽然合并文本组固定只有一个 notification，该校验保证未来元数据不会产生半提交。

- [ ] **Step 3：按区间终点推进 cursor**

`RecoveryBatch.cursor` 增加 `lastSequence`，`commit` 改为：

```ts
nextSequence: (batch.cursor.lastSequence + 1) as EventSequence
```

Operation end 的既有 cursor 清理语义保持不变。

- [ ] **Step 4：运行前端检查和构建**

Run: `rtk npm --prefix web run check`

Expected: PASS。

Run: `rtk npm --prefix web run build`

Expected: PASS。

---

### Task 4：完整验证与真实 WebUI E2E

**Files：**
- Verify only: `crates/acp/src/coalescing.rs`
- Verify only: `crates/acp/src/batching.rs`
- Verify only: `web/src/workspace/recovery.ts`
- Verify only: `docs/superpowers/specs/2026-08-21-acp-websocket-recovery-design.md`

**Interfaces：**
- Consumes: Tasks 1–3 的最终代码。
- Produces: 编译、测试、协议帧和真实断线恢复证据。

- [ ] **Step 1：格式化并检查 diff**

Run: `rtk cargo fmt --all`

Run: `rtk git diff --check`

Expected: 两者成功，且 formatter 没有产生无关修改。

- [ ] **Step 2：运行 Rust lint 与相关测试**

Run: `rtk cargo clippy -p acp --all-targets -- -D warnings`

Expected: PASS。

Run: `rtk cargo test -p acp`

Expected: PASS。

- [ ] **Step 3：运行前端生产验证**

Run: `rtk npm --prefix web run check`

Expected: PASS。

Run: `rtk npm --prefix web run build`

Expected: PASS，并更新可供生产 `app` backend 使用的 `web/dist`。

- [ ] **Step 4：运行生产后端真实 E2E**

使用生产 app 后端和当前配置 Provider 启动：

```bash
rtk cargo build -p app
rtk target/debug/clawcode serve --bind 127.0.0.1:3000 --web-root web/dist
```

在真实 WebUI 创建 Session，让当前 Provider 生成足够长的 Assistant 输出，并检查 WebSocket：

```text
至少一条 agent_message_chunk 或 agent_thought_chunk notification 的正文包含多个来源 delta
该 notification 的 sequence 保持来源首 sequence
sessionRecovery.lastSequence 等于来源末 sequence
ToolCall、MessageEnd 和 Idle 保持独立且顺序正确
```

- [ ] **Step 5：验证断线恢复**

在 Run 输出期间关闭当前 WebSocket，等待后端继续产生事件，再让 Controller 自动重连。确认：

```text
最终 Assistant 文本无重复、无缺失
Running 不因断线错误清零
重连后最终收到 Idle
前端上报 cursor 等于最近完整合并区间 lastSequence + 1
服务端可从未压缩 journal 的原始 EventGroup 接续该 cursor
```

- [ ] **Step 6：最终状态核对**

Run: `rtk git status --short`

Run: `rtk git diff --stat`

Run: `rtk git diff --check`

Expected: 仅包含本功能的代码、测试、spec 和 plan；不创建 commit。

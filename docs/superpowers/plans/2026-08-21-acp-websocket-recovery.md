# ACP WebSocket 断线恢复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 WebUI 在 WebSocket 断开后立即、单链路地重连，并通过标准 ACP `initialize` 与 `session/resume` 恢复活动或刚结束的 Agent Operation；连接断开不得停止 Kernel Run，也不得依赖 `_clawcode/session/watch`。

**Architecture:** 在 Kernel 增加不获取 operation gate 的只读 Session attachment 校验；在 ACP factory 级别增加共享 Session Projection Registry，由连接无关的 EventSink 将每个 Kernel 事件映射一次、写入当前/最近 Operation journal 并广播给任意连接 watcher。`initialize` 根据前端游标返回 `watch`/`resume` 计划，两种计划都由标准 `session/resume` 安装 watcher。前端使用连接 generation、单一重连任务、心跳和按事件组原子提交的恢复游标完成重连。

**Tech Stack:** Rust 2024、Tokio broadcast/CancellationToken、agent-client-protocol v2、Axum HTTP/WebSocket transport、TypeScript 6、React 19、Zustand 5、Vite 8。

**Spec:** `docs/superpowers/specs/2026-08-21-acp-websocket-recovery-design.md`

## Global Constraints

- 不创建 worktree，不创建 commit；只有用户后续明确授权后才允许提交。
- 所有 shell 命令都以 `rtk` 开头；命令链中的每个命令也分别以 `rtk` 开头。
- Rust 修改严格先写失败测试，再写最小实现。前端按项目规则不要求新增单元测试，但必须完成类型检查、Lint、生产构建和真实后端验收。
- 开始 Rust API/handler 任务前读取并遵循 `rust-utoipa-axum-api`；开始各实现任务前读取并遵循 `superpowers:test-driven-development`；出现失败时读取并遵循 `superpowers:systematic-debugging`；宣布完成前读取并遵循 `superpowers:verification-before-completion`。
- 所有新增函数必须有英文函数级注释；非平凡逻辑添加英文原因注释。Rust 超过 3 个字段的 struct 使用 `typed-builder` 并通过 builder 构造，`Option` 字段使用 `#[builder(default)]`；克隆 `Arc` 字段使用 `Arc::clone(&field)`。
- 日志只放在 watcher 生命周期、Operation journal 状态切换、恢复分支、lag 补偿和错误返回处；使用完整限定名 `tracing::info!()` / `tracing::warn!()` / `tracing::error!()`，正文格式化参数，不使用 tracing event fields，不记录事件正文或凭据。
- WebUI E2E 只使用生产 `app` 后端和当前配置 Provider，不引入 fixture backend。

---

### Task 1: 为 Kernel 增加非阻塞 Session attachment 校验

**Files:**
- Modify: `crates/kernel/src/runtime/session.rs`
- Modify: `crates/kernel/tests/session_capabilities.rs`

- [ ] **Step 1: 写活动 Run 期间 attachment 校验立即返回的失败测试**

在 `session_capabilities.rs` 复用现有阻塞 Model/Notify 测试夹具：创建 Session，启动一个被 `Notify` 卡住的 Run，确认 Run 已进入后，在 100ms timeout 内调用新 API。

```rust
let snapshot = tokio::time::timeout(
    std::time::Duration::from_millis(100),
    async { kernel.validate_session_attachment(&session_id, &cwd) },
)
.await
.expect("attachment validation must not wait for the active operation")
.expect("active Session attachment");
assert!(snapshot.running);
```

- [ ] **Step 2: 写 cwd 与 lifecycle 拒绝测试**

增加断言：错误 cwd 返回 `KernelError::SessionCwdMismatch`；启动尚未完成或 Close 已开始的 live Session 返回现有 lifecycle 错误。测试只调用公开 API，不在 production source 添加 test-only 接口。

- [ ] **Step 3: 运行目标测试并确认失败原因是 API 尚不存在**

Run: `rtk cargo test -p kernel --test session_capabilities validate_session_attachment -- --nocapture`

Expected: 编译失败，指出 `Kernel::validate_session_attachment` 不存在。

- [ ] **Step 4: 实现只读同步校验**

在 `Kernel` 上增加公开同步方法；它只读取 live Session map、调用 `ensure_active()`、比较 cwd，并读取 `active_run()`，不调用 Resume Hook、不获取 operation gate、不修改 Session。

```rust
/// Validates a live Session attachment without waiting for its operation gate.
pub fn validate_session_attachment(
    &self,
    session_id: &SessionId,
    cwd: &std::path::Path,
) -> Result<protocol::SessionRuntimeSnapshot, KernelError> {
    let session = self.session(session_id)?;
    session.ensure_active()?;
    if session.cwd.as_path() != cwd {
        return Err(KernelError::SessionCwdMismatch {
            session_id: session_id.clone(),
            expected: session.cwd.clone(),
            received: cwd.to_path_buf(),
        });
    }
    Ok(protocol::SessionRuntimeSnapshot {
        session_id: session_id.clone(),
        running: session.execution.active_run()?.is_some(),
    })
}
```

在 cwd/lifecycle 错误返回前增加上下文充分、无敏感内容的日志。

- [ ] **Step 5: 运行 Kernel 目标测试**

Run: `rtk cargo test -p kernel --test session_capabilities validate_session_attachment -- --nocapture`

Expected: 新增 attachment 测试全部通过，活动 Run 测试不超时。

---

### Task 2: 定义 ACP 恢复扩展元数据与游标解析

**Files:**
- Create: `crates/acp/src/recovery.rs`
- Modify: `crates/acp/src/lib.rs`
- Modify: `crates/acp/Cargo.toml`

- [ ] **Step 1: 在 `recovery.rs` 的标准 `#[cfg(test)] mod tests` 中写失败测试**

覆盖以下输入输出：

- Initialize `_meta.clawcode.sessionRecovery` 能解析 version 与 Session cursors；
- 未声明扩展返回空请求并保持旧客户端兼容；
- `ReplayFrom::Other` 只接受 type `_clawcode/event` 且必须包含合法 `runId`、`sequence`；
- 未知 custom cursor 返回 InvalidParams；
- cursor unavailable error data 包含稳定 code、sessionId、runId、sequence；
- projection 元数据能序列化 `operationPhase: start/end`。

```rust
assert_eq!(
    AcpReplayRequest::try_from(Some(wire::ReplayFrom::Other(cursor)))?,
    AcpReplayRequest::Event(EventCursor {
        run_id: RunId::try_from("run-1")?,
        sequence: Sequence::try_from(24_u64)?,
    }),
);
```

- [ ] **Step 2: 运行目标单测并确认失败**

Run: `rtk cargo test -p acp recovery::tests --lib -- --nocapture`

Expected: 编译失败，因为 `recovery` 模块和恢复类型尚不存在。

- [ ] **Step 3: 增加依赖并实现类型驱动协议适配**

在 `crates/acp/Cargo.toml` 的 `# builder` 组加入 `typed-builder = { workspace = true }`，在 async 组加入 `tokio-util = { workspace = true }`。在 `lib.rs` 注册私有 `recovery` 模块。

实现以下核心类型，serde 字段使用 camelCase；超过 3 字段的类型 derive `typed_builder::TypedBuilder`：

```rust
pub(crate) const RECOVERY_CURSOR_TYPE: &str = "_clawcode/event";
pub(crate) const CURSOR_UNAVAILABLE_CODE: &str =
    "_clawcode/session_recovery_cursor_unavailable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpReplayRequest {
    None,
    Start,
    Event(EventCursor),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventCursor {
    pub session_id: Option<SessionId>,
    pub run_id: RunId,
    pub sequence: Sequence,
}
```

`EventCursor` 的 `session_id` 仅用于 Initialize cursor；从 `session/resume.replayFrom` 解析时保持 `None`，SessionId 以请求参数为准。为 `wire::ReplayFrom` 实现 `TryFrom`，不增加单参数 free helper。实现 typed initialize request/plan、`RecoveryMode::{Watch, Resume}`、`GroupMeta`、resume diagnostic meta，并提供把它们合并进 `wire::Meta` 的关联方法。

- [ ] **Step 4: 运行恢复协议单测**

Run: `rtk cargo test -p acp recovery::tests --lib -- --nocapture`

Expected: 所有解析、兼容、错误 data 和 phase 序列化测试通过。

---

### Task 3: 实现 factory 级 ACP Session Projection Registry

**Files:**
- Create: `crates/acp/src/projection.rs`
- Modify: `crates/acp/src/lib.rs`
- Modify: `crates/acp/src/server.rs`

- [ ] **Step 1: 先写 Projection Registry 单元测试**

在 `projection.rs` 的 `#[cfg(test)] mod tests` 使用可控 AgentEvent 序列覆盖：

1. 两个 receiver 收到完全相同、按 sequence 有序的组；
2. receiver 全部释放后 `ProjectionSink::emit` 仍返回成功；
3. `RunStart` 捕获 baseline 并开启 journal，`AgentSettled` 以 end phase 写入且保留 completed journal；
4. 下一 Operation 的 baseline 包含上一 Operation durable transcript，然后替换旧 journal；
5. 内嵌 `CompactionStart/End` 不替换外层 Run journal，独立 Compaction 建立自己的边界；
6. 从 sequence 截取 backlog 后先发 backlog 再发 live，重叠 sequence 只出现一次；
7. receiver lag 后能按最后已发 sequence 从 journal 补偿；RunId 或覆盖范围不匹配返回 cursor unavailable；
8. 没有活动 journal 的 User Bash `MessageEnd` 只实时广播、不进入可恢复日志。

- [ ] **Step 2: 运行 Projection 单测并确认失败**

Run: `rtk cargo test -p acp projection::tests --lib -- --nocapture`

Expected: 编译失败，因为 registry、journal 和 sink 尚不存在。

- [ ] **Step 3: 实现 Projection 数据模型**

实现以下职责清晰的类型；不要拆出大量单参数 free helper：

```rust
pub(crate) struct ProjectionRegistry {
    kernel: Arc<Kernel>,
    sessions: Mutex<BTreeMap<SessionId, Arc<SessionProjection>>>,
}

pub(crate) struct ProjectionSink {
    session_id: SessionId,
    registry: Arc<ProjectionRegistry>,
}

#[derive(Clone, typed_builder::TypedBuilder)]
pub(crate) struct EventGroup {
    run_id: Option<RunId>,
    sequence: Sequence,
    metadata: wire::Meta,
    updates: Vec<wire::SessionUpdate>,
    #[builder(default)]
    operation_phase: Option<OperationPhase>,
}
```

`SessionProjection` 持有一个 `tokio::sync::broadcast::Sender<Arc<EventGroup>>` 和受 `std::sync::Mutex` 保护的 journal state。Operation journal 保存 `run_id`、Operation 前 baseline、分组日志、running/completed 状态和“增量是否仍可用”。Registry 每个 Session 只保留当前或最近完成的一个 journal。

- [ ] **Step 4: 实现连接无关 EventSink**

`ProjectionSink::emit` 按以下固定顺序工作：

1. 从事件 payload 判断 run_id 与 start/end phase；无显式 run_id 的流式事件继承当前 running journal；
2. 在 start 事件入日志前同步调用 `Kernel::session_replay` 捕获 baseline；
3. 调用 `AcpEventMapper::metadata` 和 `AcpEventMapper::map` 各一次；
4. 在同一 Session projection 临界区内开始/追加/完成 journal；
5. 释放锁后广播同一个 `Arc<EventGroup>`；无 receiver 是正常情况；
6. 映射或 journal 错误先记录上下文日志，再返回 `SinkError::Consumer`。

为 `EventGroup` 实现关联方法，将 base metadata 与 `sessionRecovery { runId, projectionIndex, projectionCount, operationPhase }` 合并，并生成有序的 `UpdateSessionNotification`。User Bash 和 fatal fallback 允许发送不带 recoverable group 的实时通知；fatal fallback 必须把当前 journal 标记为不可增量恢复，避免前端用不完整 cursor 继续。

- [ ] **Step 5: 实现原子 subscribe/snapshot API**

Registry API 至少包括：

```rust
pub(crate) fn sink(
    self: &Arc<Self>,
    session_id: SessionId,
) -> Arc<dyn EventSink>;

pub(crate) fn subscribe_from(
    &self,
    session_id: &SessionId,
    request: ProjectionReplay,
) -> Result<ProjectionFeed, ProjectionError>;

pub(crate) fn recovery_plan(
    &self,
    cursor: &EventCursor,
) -> RecoveryPlan;

pub(crate) fn remove(&self, session_id: &SessionId);
```

`subscribe_from` 必须在同一锁区先创建 broadcast receiver，再截取 baseline/backlog/tail，确保 snapshot 与 live 无空窗。`ProjectionFeed` 保存 backlog、receiver、run_id、tail sequence、running 与非分组 terminal fallback；因字段超过 3 个，使用 typed-builder。

- [ ] **Step 6: 运行 Projection 单测**

Run: `rtk cargo test -p acp projection::tests --lib -- --nocapture`

Expected: 多 watcher、journal retention、lag、边界、无 watcher 和 User Bash 测试全部通过。

---

### Task 4: 让所有 ACP 连接共享 Registry，并移除 connection-bound sink

**Files:**
- Modify: `crates/acp/src/server.rs`
- Modify: `crates/acp/src/extension.rs`
- Modify: `crates/acp/src/transport.rs`
- Modify: `crates/acp/tests/disconnect.rs`
- Modify: `crates/acp/tests/extension.rs`

- [ ] **Step 1: 写两个已 attachment component 共享实时流的失败测试**

在 `extension.rs` 用同一个 factory 创建两个并发 component：第一连接 New Session，第二连接在 Run 开始前用标准 Resume attachment 同一 Session，第一连接再发 Prompt。断言两边都收到相同 Assistant 更新和 Idle；断开第一连接后再执行 Compact/Slash Command/User Bash，第二连接仍收到对应更新。测试同时覆盖 Prompt、Compact、Slash Command、User Bash 的 sink 构造路径。

保留并增强 `disconnect.rs` 现有断言：第一连接释放后 `session_runtime.running` 仍为 true，说明 transport 生命周期不拥有 Kernel Run。断线 backlog 恢复的失败断言留到 Task 6，因为那一任务才实现 cursor Resume。

- [ ] **Step 2: 运行断线测试并确认失败**

Run: `rtk cargo test -p acp --test extension two_components_share_live_projection -- --nocapture`

Expected: 第二 component 等待实时通知超时，证明当前 sink 仍绑定第一连接。

- [ ] **Step 3: 将 Registry 提升到 `AcpServerFactory` 生命周期**

`AcpServerFactory` 增加第三个字段 `projections: Arc<ProjectionRegistry>`，`new` 中用同一个 Kernel 构造一次 registry。`component()` 只克隆共享 registry。

修改 `http_router`，先在 per-connection closure 外创建一次 factory：

```rust
let factory = Arc::new(AcpServerFactory::new(kernel, id_generator));
Ok(AcpHttpServer::new(move || {
    factory.component(AcpTransportKind::Http)
})
.with_options(server_options)
.into_router())
```

如果 `component()` 返回值的生命周期约束要求 owned factory clone，则为 factory 增加一个只克隆 Arc 字段的 `Clone` 实现，不能退回到 closure 内新建 registry。

- [ ] **Step 4: 替换 Prompt 与 Extension 的 connection-bound sink**

删除 `AcpEventSink` 及 `AtomicBool`。Prompt、Compact、User Bash 和 Extension/Slash 相关 sink 都改为 `projections.sink(session_id)`。Prompt detached task 只持有 Kernel、Registry、SessionId 和 trace，不持有原 connection；Run fatal error 通过 Registry 的非分组 terminal fallback 广播，并让该 journal 后续强制全量恢复。

`AcpExtensionDispatcher` 增加 `projections` 与 Session watcher map 后超过 3 个字段，derive `TypedBuilder` 并全部改用 builder 构造：

```rust
let response = AcpExtensionDispatcher::builder()
    .kernel(kernel)
    .connection(connection)
    .mcp_watchers(mcp_watchers)
    .projections(projections)
    .session_watchers(session_watchers)
    .build()
    .execute(request)
    .await?;
```

- [ ] **Step 5: Close/Delete 时清理 Projection**

仅在 Kernel close/delete 成功后调用 `projections.remove(&session_id)`；错误返回前记录 SessionId 和错误。普通 WebSocket 断开只结束该连接 watcher，不删除 projection、不取消 Kernel Operation。

同时先实现 live-only watcher：New、Fork 和无 replay cursor 的 Resume 都从 Registry 订阅“从当前 tail 之后开始”的实时广播。每个 component 使用自己的 Session watcher map，因此两个连接可以同时 attachment；同一连接重复 attachment 会替换旧 watcher。Task 6 在此基础上加入 backlog/full replay。

- [ ] **Step 6: 运行 ACP 断线与 Extension 测试**

Run: `rtk cargo test -p acp --test extension two_components_share_live_projection -- --nocapture`

Expected: 两个 component 都收到相同实时更新；关闭一个不影响另一个。

Run: `rtk cargo test -p acp --test disconnect -- --nocapture`

Expected: 现有断线 Run 生存测试继续通过。

Run: `rtk cargo test -p acp --test extension -- --nocapture`

Expected: 现有 Extension 测试和新增 sink 入口测试全部通过。

---

### Task 5: 在标准 Initialize 握手中返回 watch/resume 计划

**Files:**
- Modify: `crates/acp/src/server.rs`
- Create: `crates/acp/tests/recovery.rs`

- [ ] **Step 1: 写 Initialize recovery plan 失败测试**

在 `recovery.rs` 集成测试中通过真实 ACP component 发送三种 Initialize：

1. 无 recovery meta，响应能力与旧测试一致；
2. 当前或最近已完成 journal 的 RunId 相同且覆盖 `nextSequence` 时，包含“落后于 tail 需要补发”和“等于 tail + 1 只需绑定 live”两种情况，计划 mode 都为 `watch`；
3. RunId 被替换、sequence 早于 journal head、sequence 晚于 tail + 1 或 journal 不可增量时，计划 mode 为 `resume`。

响应位置固定为 `_meta.clawcode.sessionRecovery = { version: 1, sessions: [{ sessionId: "session-1", mode: "watch", runId: "run-1", nextSequence: 24 }] }`。每个 plan 至少包含 sessionId、mode；可恢复时包含 runId 和 nextSequence。

- [ ] **Step 2: 运行 Initialize 测试并确认失败**

Run: `rtk cargo test -p acp --test recovery initialize_ -- --nocapture`

Expected: 响应中不存在 sessionRecovery plans。

- [ ] **Step 3: 实现 Initialize request 解析和响应合并**

Initialize handler 从 request meta 解析游标，逐个调用 `projections.recovery_plan`，统计 watch/resume 数量并写一条 lifecycle 日志。扩展元数据与现有 `{ methods }` 放在同一个产品 namespace 对象中，不能覆盖 methods。

旧客户端没有 recovery meta 时，不生成 Session plans，不改变 Initialize、New、Resume 的兼容行为。

- [ ] **Step 4: 运行 Initialize 测试**

Run: `rtk cargo test -p acp --test recovery initialize_ -- --nocapture`

Expected: watch/resume/compatibility 三类测试全部通过。

---

### Task 6: 用标准 `session/resume` 完成增量/全量恢复并安装 watcher

**Files:**
- Modify: `crates/acp/src/server.rs`
- Modify: `crates/acp/src/extension.rs`
- Modify: `crates/acp/tests/recovery.rs`
- Modify: `crates/acp/tests/disconnect.rs`

- [ ] **Step 1: 写 Resume 与 watcher 的失败集成测试**

测试矩阵：

- `_clawcode/event` cursor 从 inclusive sequence 补发 journal 后继续 live；
- backlog/live 重叠处按 sequence 去重，一个 Kernel event 的 projectionIndex 从 0 到 count-1；
- active Run 的 `replayFrom:start` 在 100ms 内完成，不等待 operation gate；
- completed journal 仍可补发非持久化的流式/状态事件；
- cursor unavailable 在发送任何 update 前返回稳定 error data；前端可随后用 start 全量恢复；
- 两个 component 同时 Resume 同一 Session 都收到事件，断开一个不影响另一个；
- 同一连接重复 Resume 会替换旧 watcher，不产生重复通知；
- New、Resume、Fork 都安装 Session watcher；
- Run 断线期间结束后，重连最终只得到一份 assistant 内容和一个 Idle。

- [ ] **Step 2: 运行 Resume 测试并确认失败**

Run: `rtk cargo test -p acp --test recovery resume_ -- --nocapture`

Expected: custom ReplayFrom 仍被拒绝，active full resume 或重连 watcher 用例失败。

- [ ] **Step 3: 实现连接级 watcher 管理**

每个 `component()` 创建：

```rust
Arc<Mutex<BTreeMap<SessionId, tokio_util::sync::CancellationToken>>>
```

`AcpServer::watch_session` 收到 `ProjectionFeed` 后，先取消并替换同 Session 的旧 token，再通过 `connection.spawn` 发送 backlog 和 live。live receiver lag 时从 `last_sent_sequence + 1` 调 Registry 补偿；补偿失败则记录错误并结束 watcher，等待前端下一次 Resume。任务结束时仅在 map 中仍是同一个 token 才删除条目。

发送规则：完整 `EventGroup` 按 index 顺序发送；snapshot/live 重叠 sequence 跳过；非分组 terminal fallback 只用于强制全量路径。发送失败只结束 watcher并记录日志，不向 Projection Sink/Kernel 返回错误。

- [ ] **Step 4: 实现 Resume 三分支**

Resume handler 使用 `AcpReplayRequest::try_from(request.replay_from)`：

1. `Event(cursor)`：调用 `validate_session_attachment`，再原子 `subscribe_from(Cursor)`；任何 cursor 错误在发送 update 前返回稳定 cursor unavailable data。
2. `Start` 且 registry 有 current/recent journal：调用只读 attachment 校验，订阅 `ProjectionReplay::Start`，按 baseline、journal、terminal fallback、live 的顺序发送。
3. 无 Projection：沿用 `Kernel::resume_session` 和 `session_replay` 的 durable replay，然后创建 live-only subscription。

两种恢复成功后都发送 Available Commands、安装 MCP watcher、安装 Session watcher，并在 `ResumeSessionResponse._meta.clawcode.sessionRecovery` 返回 `{ mode, runId, journalTailSequence, running }`。响应元数据只用于诊断，不作为前端推进 cursor 的依据。

- [ ] **Step 5: 在 New 与 Fork 安装 live watcher**

New 成功后确保 registry Session entry 存在并安装 live-only watcher；Fork 在发送 Available Commands、安装 MCP watcher后安装同样 watcher。这样首次 Prompt、Compact、Slash 和 User Bash 都经共享 Registry 广播到当前连接。

- [ ] **Step 6: 运行 ACP 恢复测试**

Run: `rtk cargo test -p acp --test recovery -- --nocapture`

Expected: Initialize、增量、全量、lag、completed journal、多连接和 watcher replacement 全部通过。

Run: `rtk cargo test -p acp --test disconnect -- --nocapture`

Expected: 断线 Run 不中止，重连补齐且终态无重复。

---

### Task 7: 前端解析恢复协议并按 Projection Group 原子提交

**Files:**
- Modify: `web/src/acp/protocol.ts`
- Create: `web/src/workspace/recovery.ts`
- Modify: `web/src/workspace/updateRouter.ts`
- Modify: `web/src/workspace/state.ts`

- [ ] **Step 1: 在 `protocol.ts` 增加严格类型与解码器**

增加：

```typescript
export type SessionRecoveryCursor = Readonly<{
  sessionId: SessionId;
  runId: RunId;
  nextSequence: EventSequence;
}>;

export type ProjectionGroupMeta = Readonly<{
  runId: RunId;
  projectionIndex: number;
  projectionCount: number;
  operationPhase?: "start" | "end";
}>;
```

增加 Initialize plan、Resume diagnostic 和 cursor unavailable data 类型。所有解码器严格验证 version、字符串 ID、精确 sequence、index/count 范围和 phase；未声明 recovery meta 返回 `undefined`，旧服务端通知继续走立即应用路径。

- [ ] **Step 2: 实现 Session Recovery Buffer**

`recovery.ts` 增加 `SessionRecoveryBuffer`，每 Session 保存 committed cursor、当前未完成 group 和 force-full 标志。公开方法：

```typescript
accept(notification: SessionUpdateNotification): RecoveryBatch | undefined;
commit(batch: RecoveryBatch): void;
cursors(): readonly SessionRecoveryCursor[];
requireFullReplay(sessionId: SessionId): void;
reset(sessionId: SessionId): void;
```

规则严格按 spec：只在完整 start 组提交后建立 cursor；只接收同 runId、连续 sequence、index 唯一且 count 一致的组；完整组按 index 排序；异常将 Session 标记为 full-only；完整 end 组先提交 UI，再清除 cursor。未完成组断线时丢弃，不修改正式状态。

- [ ] **Step 3: 为 Workspace reducer 增加单次 batch action**

在 `WorkspaceAction` 增加：

```typescript
| { readonly type: "workspace/batch"; readonly actions: readonly WorkspaceAction[] }
```

reducer 对 batch 顺序 fold 子 action 并一次返回最终 WorkspaceState。禁止 batch 内嵌 batch。这样一个 Kernel event 映射出的多条 ACP 更新不会在 Zustand 中暴露半组状态。

- [ ] **Step 4: 将 Router 改为“缓冲、解码、一次提交、再推进 cursor”**

`SessionUpdateRouter.apply` 先调用 buffer.accept。legacy/direct 通知作为单条 batch 立即处理；recovery group 未完成时直接返回；完整组用临时 state 顺序 decode，收集所有 WorkspaceAction 后 dispatch 一个 `workspace/batch`，成功后调用 `buffer.commit`。运行时刷新请求在 batch 提交后触发并按 Session 去重。

Router 增加 `recoveryCursors()`、`requireFullReplay(sessionId)`、`resetRecovery(sessionId)`，供 Controller 握手使用。

- [ ] **Step 5: 运行前端静态检查**

Run: `rtk npm --prefix web run check`

Expected: TypeScript 与 ESLint 退出码 0，无 unsafe cast/未使用符号错误。

---

### Task 8: 重写前端连接 generation、单链路重连与心跳

**Files:**
- Modify: `web/src/acp/connection.ts`
- Modify: `web/src/workspace/controller.ts`
- Modify: `web/src/workspace/state.ts`
- Modify: `web/src/workspace/sessionState.ts`

- [ ] **Step 1: 为 `AcpConnection` 增加活动回调和独立超时**

`AcpConnectionCallbacks` 增加 `activity()`。每个成功解析的 JSON-RPC 消息调用一次。`connect(timeoutMs = 10_000)` 在超时后关闭当前 socket 并拒绝；`request` 接受 `{ timeoutMs?: number }`，PendingRequest 保存 timeout handle，resolve/reject/close 都清理 handle。

```typescript
request<T>(
  method: string,
  params: unknown,
  options: Readonly<{ timeoutMs?: number }> = {}
): Promise<T>;
```

显式 `close()` 继续抑制 closed callback；心跳 timeout 由 Controller 显式 close 后进入统一重连调度。

- [ ] **Step 2: 将 Controller 改为 generation 驱动的单连接状态机**

增加 connection generation、`connectPromise`、唯一 reconnect timer、heartbeat timer、heartbeat in-flight、last message time 和 DOM listener 状态。所有 callback 捕获 `(generation, localConnection)`，只有同时匹配当前代次与当前连接才可改状态；握手与恢复全过程只使用 localConnection，不调用 `requireConnection()`。

`connect()` 顺序固定为：

1. 建立 WebSocket；
2. Initialize 携带 `updateRouter.recoveryCursors()`；
3. 校验 capabilities 与 recovery plans；
4. 标准 `session/list`；
5. 对已有 workspace：watch plan 保留当前投影并用 `_clawcode/event` Resume；resume plan 或无 cursor 先清空该 Session transcript/recovery buffer，再用 `replayFrom:start`；
6. cursor unavailable 时 `requireFullReplay`、清空该 Session transcript，再以 start 重试一次；
7. 刷新 runtime/tree/pending/skills/MCP snapshots；
8. 全部成功且 generation 仍有效后进入 Ready、重置退避并启动心跳。

Prompt request 若在断线窗口结果未知，只保留 `outcomeUnknown`，不得自动重发。

- [ ] **Step 3: 移除断线 invalidation 与 runtime polling workaround**

断线不再 dispatch `sessions/invalidated`，不再把 running 清成 false；删除 `SessionWorkspaceAction` 和 `WorkspaceAction` 中对应 invalidated variant 与 reducer 分支。删除 `runtimePolls` 和 `scheduleSessionRuntimePoll`，因为 Session watcher 能在重连后继续提供实时终态。

- [ ] **Step 4: 实现唯一重连 timer 和网络唤醒**

指数退避使用现有 delay 表并增加小幅 jitter；`scheduleReconnect` 若 timer 或 connectPromise 已存在则不重复创建。首次启动连接失败与已连接后的 close 都进入同一重连调度。显式 reconnect、`online` 和页面从 hidden 变 visible 时取消退避并立即调用统一 `ensureConnected()`。`close()` 清理 reconnect/heartbeat timer、关闭连接、移除 listeners，并递增 generation 让所有旧 callback 失效。

- [ ] **Step 5: 实现 idle heartbeat**

连接 Ready 后周期检查：至少 20 秒没有收到 ACP 消息且没有 probe 时，用当前 localConnection 发送标准 `session/list`，设置独立 5 秒 timeout。成功只更新 activity，不刷新 UI；失败或超时显式关闭该 connection并调用统一断线处理。同一 generation 最多一个 heartbeat request。

- [ ] **Step 6: 运行前端检查和生产构建**

Run: `rtk npm --prefix web run check`

Expected: 类型检查和 ESLint 通过。

Run: `rtk npm --prefix web run build`

Expected: Vite 生产构建成功，生成 dist assets，无 TypeScript 构建错误。

---

### Task 9: 全量回归、日志审计和真实 WebUI 验收

**Files:**
- Verify: `crates/kernel/src/runtime/session.rs`
- Verify: `crates/acp/src/recovery.rs`
- Verify: `crates/acp/src/projection.rs`
- Verify: `crates/acp/src/server.rs`
- Verify: `crates/acp/src/extension.rs`
- Verify: `crates/acp/src/transport.rs`
- Verify: `web/src/acp/connection.ts`
- Verify: `web/src/acp/protocol.ts`
- Verify: `web/src/workspace/controller.ts`
- Verify: `web/src/workspace/recovery.ts`
- Verify: `web/src/workspace/updateRouter.ts`

- [ ] **Step 1: 格式化并检查改动范围**

Run: `rtk cargo fmt --all`

Expected: rustfmt 完成且退出码 0。

Run: `rtk cargo fmt --all -- --check`

Expected: 无格式差异。

Run: `rtk git status --short`

Expected: 只出现本计划列出的实现/测试/文档文件，以及用户原有且未被修改的无关文件；不得出现 worktree 或 commit。

- [ ] **Step 2: 运行 Rust 静态检查**

Run: `rtk cargo clippy -p kernel -p acp --all-targets -- -D warnings`

Expected: 退出码 0，无 clippy warning；特别确认没有 `await_holding_lock`、隐式 Arc field clone 或 tracing structured fields。

- [ ] **Step 3: 运行 Kernel 与 ACP 全量测试**

Run: `rtk cargo test -p kernel`

Expected: Kernel 全量测试通过。

Run: `rtk cargo test -p acp`

Expected: ACP unit/integration tests全部通过，包括 disconnect、recovery、extension、mapping、tracing。

- [ ] **Step 4: 再次运行前端检查与生产构建**

Run: `rtk npm --prefix web run check`

Expected: 退出码 0。

Run: `rtk npm --prefix web run build`

Expected: 退出码 0。

- [ ] **Step 5: 审计注释和日志**

Run: `rtk git diff --check`

Expected: 无 whitespace error。

Run: `rtk grep -n "tracing::\|fn " crates/acp/src/recovery.rs crates/acp/src/projection.rs crates/acp/src/server.rs crates/acp/src/extension.rs crates/kernel/src/runtime/session.rs`

Expected: 新函数均有英文函数级注释；日志只位于生命周期、状态切换、恢复/lag 和错误分支，正文包含 SessionId/RunId/sequence 等必要上下文但不包含消息正文或凭据。

- [ ] **Step 6: 使用生产 app 与当前 Provider 做真实 WebUI E2E**

按仓库现有 app 启动方式运行生产后端和 WebUI，不替换 Provider。依次验证：

1. 发起持续时间足够断线的真实 Agent Run；
2. 浏览器中途断开 WebSocket，UI 保持 Running，Kernel Run 继续；
3. 恢复网络后不刷新页面即可补齐文本/工具/状态并继续实时显示，最终只出现一份 Assistant 输出和 Idle；
4. 两个标签页打开同一 Session，二者都收到终态；关闭一个标签不影响另一个和后端 Run；
5. 暂停服务器后恢复，单页始终只有一个 reconnect timer/connection generation；
6. 在 Prompt response 未知窗口断线，前端不重复发送 Prompt；
7. Run 在断线期间已结束时，completed journal 仍能恢复丰富事件；
8. 保持连接空闲超过 20 秒，确认 heartbeat 成功不刷新 Session 列表；让 heartbeat 超时，确认 5 秒内进入统一重连。

- [ ] **Step 7: 最终差异审查，不提交**

Run: `rtk git diff --stat`

Expected: 差异只覆盖设计范围。

Run: `rtk git diff -- docs/superpowers/specs/2026-08-21-acp-websocket-recovery-design.md docs/superpowers/plans/2026-08-21-acp-websocket-recovery.md crates/kernel crates/acp web/src`

Expected: 没有 `_clawcode/session/watch`，没有 connection-bound EventSink，没有断线 invalidation/poll workaround，没有凭据日志，也没有 commit。

---

## 补充修复与 ACP v2 Multi-Message 性能计划（方案 A）

本节覆盖 review 发现的三个恢复正确性问题，并在 ACP component 与物理 transport 之间增加帧级适配器。适配器只聚合当前已经排队的连续标准 JSON-RPC `session/update` notification，不等待定时器；每个 batch 最多 64 条。实现过程遵守项目限制：不使用 subagent、worktree，也不创建 commit。

### Task 10: 修复嵌套 compaction 的 RunId 投影错误

**Files:**
- Modify: `crates/acp/src/projection.rs`

- [x] **Step 1: 先增加失败回归测试**

构造外层 Agent Run 和拥有独立 RunId 的内层 compaction，依次投影 `RunStart`、`CompactionStart`、`CompactionEnd`、`AgentSettled`。断言 journal 中所有 group 都使用外层 Agent RunId，且订阅者能连续收到完整序列和终态。

Run: `rtk cargo test -p acp projection::tests::nested_compaction_uses_active_agent_run_id -- --exact`

Expected: 当前实现因 compaction RunId 与 journal RunId 不一致而失败。

- [x] **Step 2: 使用活动 Agent Run 上下文投影 compaction**

修改 compaction 事件分支：payload 自带 RunId 只标识 compaction 操作，恢复 group 必须继承 Session 当前活动的 Agent RunId。为非平凡分支增加英文注释，避免把 operation id 当作 recovery run id。

- [x] **Step 3: 重跑定向测试**

Run: `rtk cargo test -p acp projection::tests::nested_compaction_uses_active_agent_run_id -- --exact`

Expected: 测试通过，sequence 连续且终态保留。

### Task 11: 修复 watcher lag 和 durable resume 订阅竞态

**Files:**
- Modify: `crates/acp/src/server.rs`
- Modify: `crates/acp/src/projection.rs`

- [x] **Step 1: 先增加 watcher cursor 回归测试**

覆盖从 `run_id=None` 启动的 live-only watcher：发送 RunStart 后必须记录实际 RunId 和下一 sequence；模拟 broadcast lag 后，恢复订阅必须从该 cursor 继续，而不是静默退出。

Run: `rtk cargo test -p acp server::tests::live_only_watch_tracks_cursor_for_lag_recovery -- --exact`

Expected: 当前 watcher 不更新 cursor，测试失败。

- [x] **Step 2: 实现 watcher cursor 更新与 lag 恢复**

为 `ProjectionWatch` 增加小型状态方法并写英文函数注释。每次成功发送 group 后更新 RunId/next sequence；收到 `Lagged` 时调用 projection subscribe，从最后确认 cursor 补齐 journal，再继续 live receiver。若 cursor 已淘汰，发送明确 recovery 错误并结束该 watcher，不能让健康 WebSocket 静默停更。

- [x] **Step 3: 先增加 durable resume 排序回归测试**

覆盖无 journal 的 durable Resume：在 resume 开始到 live watcher 建立之间触发 RunStart，断言客户端仍能收到 RunStart。测试应使用生产 projection/registry 路径，不增加 production test hook。

Run: `rtk cargo test -p acp projection::tests::durable_resume_selects_new_journal_without_mixing_replay -- --exact`

Expected: 当前先 resume/replay 后 subscribe 的顺序可丢失事件，测试失败。

- [x] **Step 4: 调整 durable resume 顺序并重跑测试**

先创建 live receiver，再调用 Kernel resume；随后先在内存捕获 durable replay，再检查 journal。journal 已出现时只使用 baseline + journal；否则只发送 durable replay 并接续预先建立的 receiver，禁止混合两种恢复源。

Run: `rtk cargo test -p acp`

Expected: 两个回归测试均通过。

### Task 12: 增加 ACP v2 JSON-RPC batch 传输适配器

**Files:**
- Create: `crates/acp/src/batching.rs`
- Modify: `crates/acp/src/lib.rs`
- Modify: `crates/acp/src/server.rs`

- [x] **Step 1: 先增加帧级失败测试**

用 ACP v2 `Channel`/`TransportFrame` 构造 transport-facing 与 component-facing 两端，覆盖：连续 `session/update` 合并；65 条拆成 64+1；非 session 消息保持顺序并触发 flush；单条保持合法单消息；入站帧原样转发。

Run: `rtk cargo test -p acp batching::tests -- --nocapture`

Expected: 新测试在适配器实现前失败。

- [x] **Step 2: 实现无等待的 bounded batching adapter**

增加 `BatchComponent<C>`，覆写 ACP component 的 frame-aware channel 接口。出站只 drain 当前已排队的连续 `session/update` notification，以 64 条为上限构造标准 `TransportFrame::Batch`；遇到非目标帧或队列暂空立即 flush。入站不改写。所有新增函数写英文函数级注释，Arc 字段使用显式 `Arc::clone`。

- [x] **Step 3: 接入所有 ACP transport 并验证**

在 component factory 的唯一出口包装适配器，使 stdio、HTTP、WebSocket 与测试 transport 行为一致。

Run: `rtk cargo test -p acp batching::tests`

Expected: batch 边界、顺序和双向转发测试全部通过。

### Task 13: 前端按物理帧原子应用 Multi-Message

**Files:**
- Modify: `web/src/acp/connection.ts`
- Modify: `web/src/workspace/controller.ts`
- Modify: `web/src/workspace/updateRouter.ts`

- [x] **Step 1: 扩展 JSON-RPC frame 解码**

`AcpConnection` 接受单个 object 或非空 JSON-RPC array。response/request 仍逐项处理；同一 frame 内的 notification 收集后只调用一次 Controller callback。空数组和非法成员写 diagnostic，不得破坏后续连接。

- [x] **Step 2: 增加 `SessionUpdateRouter.applyMany`**

同一物理 frame 的多个 `session/update` 依次推进 recovery buffer/cursor，但只 dispatch 一次 `workspace/batch`。任何成员校验失败时，将本 frame 涉及的 Session 标记为需要 full replay，避免 cursor 与 UI 部分提交不一致。

- [x] **Step 3: 运行前端检查与生产构建**

Run: `rtk npm --prefix web run check`

Expected: TypeScript 与 ESLint 通过。

Run: `rtk npm --prefix web run build`

Expected: 生产构建成功。

### Task 14: 全量验证与真实断线重连 E2E

**Files:**
- Verify: `crates/acp/src/batching.rs`
- Verify: `crates/acp/src/projection.rs`
- Verify: `crates/acp/src/server.rs`
- Verify: `web/src/acp/connection.ts`
- Verify: `web/src/workspace/controller.ts`
- Verify: `web/src/workspace/updateRouter.ts`

- [x] **Step 1: 运行格式、静态检查与全量测试**

Run: `rtk cargo fmt --all -- --check`

Run: `rtk cargo clippy -p kernel -p acp --all-targets -- -D warnings`

Run: `rtk cargo test -p kernel -- --test-threads=1`

Run: `rtk cargo test -p acp`

Run: `rtk npm --prefix web run check`

Run: `rtk npm --prefix web run build`

Expected: 全部退出码为 0；Kernel 使用单线程规避已确认的全局 tracing capture 并发干扰。

- [x] **Step 2: 启动生产 app 与当前 Provider**

按仓库正式启动流程运行 `app`，WebUI 直接连接生产 backend，不使用 fixture。创建 cwd 指向本项目的新 Session，并要求该 Session review 当前项目代码、spec 和未提交改动。

- [x] **Step 3: 浏览器中模拟真实连接中断与恢复**

在 Agent Run 仍运行时关闭活动 ACP WebSocket，确认 UI 保持 Running，并观察浏览器自动建立新 connection；随后额外重启生产 app 验证退避重连。确认无需刷新即可收到 backlog，随后继续实时消息并最终进入 Idle。

- [x] **Step 4: 核对恢复正确性和性能证据**

检查浏览器页面状态与后端日志：Assistant 文本和 Idle 不重复；sequence 连续；没有 cursor unavailable 或 silent watcher exit；backlog 中存在包含多个 `session/update` 的 JSON-RPC batch，且每批不超过 64 条。

- [x] **Step 5: 最终审查且不提交**

Run: `rtk git diff --check`

Run: `rtk git status --short`

Run: `rtk git diff --stat`

Expected: 只保留用户原有改动和本次方案 A 的实现/测试/文档；不创建 commit。

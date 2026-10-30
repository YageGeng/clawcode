# ACP 3.6 升级与 1.13 对齐设计

## 目标

把 `agent-client-protocol` 与 `agent-client-protocol-http` 从 2.0.0 升级到 3.6.0（schema 从 1.5.0 升到 1.13.0，v2 draft 从 alpha.1 升到 alpha.10），使 clawcode 服务端与 WebUI 全面符合 1.13 的 v2 draft；用标准机制替换已有的自定义扩展（elicitation、compaction 事件、诊断提示），并新增 session config options（model 与 thinking 切换）与 terminal 实体（bash 输出）。

参考：

- SDK 变更：`agentclientprotocol/rust-sdk` 的 `src/agent-client-protocol/CHANGELOG.md`、`src/agent-client-protocol-http/CHANGELOG.md`（2.0.0 → 3.6.0）。
- 规范：`agentclientprotocol/agent-client-protocol` 标签 `v1.13.0` 下的 `docs/protocol/v2/draft/*.mdx` 与 `schema/v2/CHANGELOG.md`。

约束：

- WebUI 是唯一 ACP 客户端，与后端同步升级，不保留旧协议兼容。
- 代码风格沿用 docparse：借用上下文的结构体承载流程，逻辑组织为结构体方法；转换使用 `From` / `TryFrom`；超过 3 个字段的结构体使用 `typed-builder`。
- 跨 crate 转换遵守孤儿规则：ACP 用本地借用包装类型 `UsageOf<'a>`、`ElicitationOf<'a>`、`ConfigOptionsOf<'a>` 实现转换，不直接为 `protocol` / `kernel` 类型与 SDK 类型之间实现外部 trait。
- 已知上限用 `shortcut:` 注释标注上限与升级路径。

非目标：

- steer / follow-up 切换（后续子项目 2）；`unstable_subagents` 映射（后续子项目 3）。
- permission 请求、agent plan、authentication、`additionalDirectories`、client system prompt：clawcode 无对应功能，不声明相应能力。
- MCP OAuth 改为 URL 模式 elicitation；`elicitation/complete`（mcp crate 尚未处理 MCP URL 完成通知）。

## 分阶段

| 阶段 | 内容 | 依赖 |
|---|---|---|
| A | SDK 编译迁移与必须语义：插入即接受（含 `!bash` 开始即插入）、错误停止原因、取消/关闭覆盖未插入 prompt、resume 状态同步、initialize 协商、命令 `type`、错误码 | — |
| B | 状态与实体对齐：`requires_action`、setup 响应初始状态、工具 `name`/`kind`/`locations`/`cancelled`、compaction 实体、notices、idle 用量 | A |
| C | 标准 elicitation | A、B1 |
| D | session config options | A |
| E | terminal 实体 | A、B3 |

每个阶段结束时 fmt、clippy、全量测试与前端构建通过，并按阶段做 WebUI 端到端验收。

## 阶段 A：编译迁移与必须语义

### A1 编译迁移

- 根 `Cargo.toml`：`agent-client-protocol = "=3.6.0"`，`agent-client-protocol-http = { version = "=3.6.0", default-features = false }`。必须精确锁定：3.0.0（10-06）到 3.6.0（10-28）三周内发布了 7 个版本，v2 经 `unstable_protocol_v2` 开启，次版本会随草案改变 v2 的线上语义（3.4.0 升级 schema 1.12.0 并调整 v2 生命周期，3.6.0 改变 v2 initialize 协商并拒绝 null 请求 ID），`"3.6"` 允许 `cargo update` 拉到 3.7。后续升级作为独立任务主动进行。`crates/acp/Cargo.toml` 在 `unstable_protocol_v2` 之外显式开启 `stdio`（`transport.rs` 使用 `Stdio`）。
- `server.rs` 全部请求处理闭包的连接参数改为 `V2ConnectionTo<Client>`。
- `batching.rs` 的 `ConnectTo::into_channel_and_future` 返回 `(Channel, Option<ConnectionDriver>)`，原 future 用 `ConnectionDriver` 包装。
- schema 调整：`ResourceLink.size` 为 `u64`；可用命令构造为 `AvailableCommand::Command(..)`；non-exhaustive 结构体改用 builder。
- 检查点顺序：SDK 3.6 的 v2 `PromptResponse::new(message_id)` 必须携带真实消息 ID，而现有 `session/prompt` 处理器在启动 run 之前就响应，拿不到 ID。因此 Kernel 侧的插入通知与等待者取消（A2 的 `InsertionNotice`、A4 的 `acquire_operation_for_prompt`）先在 SDK 2.0 上完成并单独验收；SDK 迁移与 `session/prompt` 改为插入后响应（A2 第 1–3 步）在同一检查点完成，不使用占位 ID。A2.1 之前 user-bash 还没有开始事件，Kernel 侧先在 `BashExecution` 落盘后临时触发插入，保证这两个检查点之间 `!bash` 不会在执行完后返回错误；实施 A2.1 时把触发点移到 `UserBashStarted` 并删除临时触发。
- HTTP 配置：`ServerOptions` 与 `CorsOptions` 已变为 non-exhaustive。`crates/acp/src/transport.rs`、`crates/app/src/{web,lib,main}.rs` 与 `crates/app/tests/web.rs` 中的字面量构造改为 `ServerOptions::default().with_path(..).with_cors(..).with_health_endpoint(..)`；对 `CorsOptions` 的 `match` 增加通配分支。

### A2 接受即插入

规范：`session/prompt` 在用户消息插入 ACP 会话后才成功响应，响应必须携带 `messageId`，且不得早于插入；插入前被拒绝返回 JSON-RPC 错误且不得发 user message 更新；插入后不得再返回错误。

Kernel：

```rust
/// One-shot report of whether a Run inserted its user message (ACP v2 prompt acceptance).
pub struct InsertionNotice(Mutex<Option<tokio::sync::oneshot::Sender<Result<MessageId, KernelError>>>>);

impl InsertionNotice {
    /// Creates a notice and the receiver the protocol adapter awaits.
    pub fn new() -> (Arc<Self>, tokio::sync::oneshot::Receiver<Result<MessageId, KernelError>>);
    /// Reports insertion once; later calls are ignored.
    pub(in crate::runtime) fn inserted(&self, message_id: &MessageId);
    /// Reports a failure that ended the Run before insertion; ignored after insertion.
    pub(in crate::runtime) fn rejected(&self, error: &KernelError);
}
```

- `Kernel::run_traced` 增加参数 `insertion: Option<Arc<InsertionNotice>>`，内部经 `run_with_source` 传递。
- `run_with_source` 包含普通输入、直接 Slash Command 与 user-bash 分流的完整错误边界；在插入前返回错误（含 A4 的 `PromptCancelled`、model preflight 或预压缩失败）时调用 `rejected(&error)`。通知被丢弃而既未插入也未拒绝时，接收端得到关闭错误，ACP 按 `-32603` 处理。
- 保留 input hook 的现有容错行为：handler 返回错误时报告 `ExtensionHandlerFailed` 并继续后续输入处理，不将其改为 prompt 拒绝。插入前失败测试使用真正向 Kernel 传播的 model preflight 错误；另测 input hook 错误仍能正常插入。
- 触发点：
  - 普通输入：`run.rs` 中首条用户消息 `persist_message` 之后；排队输入被 `consume_pending` 时（为子项目 2 预留，本期仅直接 prompt 使用）。
  - 直接执行的 Slash Command：`SlashCommand` Invocation 消息落盘之后。`execution.rs` 有两个 Invocation 落盘点，两处都要触发：正常路径，以及会话忙碌分支（`try_acquire_operation` 返回 `None` 时，先落盘 Invocation，再落盘 `Busy` 失败输出）。`persist_and_emit_slash_message` 也用于写输出消息，因此不在它内部无条件触发；由执行上下文携带 `InsertionNotice`，在两个 Invocation 落盘点之后调用 `inserted`。
  - 忙碌分支同样视为已插入：prompt 响应成功，失败通过已落盘的 Failed 输出消息呈现。该分支不发 `SlashCommandStart` / `SlashCommandEnd`，前台状态仍属于正在运行的 run，因此不发 `idle`，也不发 error 停止原因。
  - `!bash` / `!!bash`：执行开始时，见 A2.1。
  - 被扩展 input hook 返回 `Handled` 的输入：生成一个消息 id，发出仅推送不落盘的 `UserMessage` 更新后触发（规范的「本地处理命令插入 live-only 消息」）。
- 插入前的 hook、预压缩等工作仍属前台工作：`running` 可以早于插入（规范允许）。
- 插入点复查运行令牌：获得 gate 并安装运行令牌之后、插入之前，仍会执行 input hook、`before_agent_start` / `agent_start` hook、预压缩、model preflight 与 `turn_start` hook，这些路径本身不检查取消。因此带 `InsertionNotice` 的 run 在首条用户消息落盘与发出 `UserBashStarted` 之前，必须再次检查本次运行令牌（内部来源不做此复查，见 A4「取消范围」）；已取消则返回 `KernelError::PromptCancelled`，不插入消息、不启动 shell。插入前由取消引起的错误（如预压缩返回 `ModelError::Cancelled`）同样转换为 `PromptCancelled`，ACP 返回 `-32800` 而非 `-32603`。
- 直接执行的 Slash Command 不做插入前复查：从安装运行令牌（`execution.rs:129`）到 Invocation 落盘（`execution.rs:150`）之间只发一次 `SlashCommandStart`，没有 hook 或模型调用。取消落在这个窗口时按「已插入、随后取消」处理，同样符合协议，且 Kernel 侧插入通知不依赖 A3 的 `SlashCommandStatus::Cancelled`。
- 插入前被拒绝时，若 `RunStart` 已投影 `running`，该 run 仍只结算一次终态：`PromptCancelled` 结算为 `AgentOutcome::Cancelled`，映射 `idle(cancelled)`；其他错误结算为 `Failed`，映射 `idle(error)`。`SlashCommandStart` 之后、Invocation 落盘之前的错误以 `SlashCommandEnd { status: Failed }` 结束。客户端收到 JSON-RPC 错误后不会停留在运行中。

ACP `session/prompt` 处理：

1. 校验参数（`-32602`）与会话活跃性（`-32002`，见错误码）。
2. 创建 `InsertionNotice`，用 `tokio::spawn` 派生分离的 run 任务（保持 run 属于 Kernel Session 而非连接）。
3. 把 responder 移入 `connection.spawn` 派生的响应任务，由它等待接收端：`Ok(id)` → `PromptResponse::new(message_id)`；`Err(PromptCancelled)` 或插入前取消 → `-32800`；其他 `Err` → 映射后的 JSON-RPC 错误；通道关闭 → `-32603`。处理回调在派生后立即返回，不得在回调内等待插入：SDK 3.6 每个连接只有一个分发循环，回调执行期间该连接的其他消息都不会被处理（见 SDK `concepts/ordering.rs`），在回调里等待会挡住同一连接的 `session/cancel` 与 elicitation 回复，甚至形成死锁。
   - 本期不保证多个等待 gate 的 prompt 按提交顺序插入：它们由各自的任务竞争 gate，tokio 不保证到达顺序。每个 prompt 都能插入并得到自己的 `messageId`；排队顺序由子项目 2 的队列提供（届时会话忙碌时的 prompt 进入队列，不再竞争 gate）。
4. 插入后结束前台工作的失败，只通过该 run 的终态 `idle(error)` 体现（A3）；会话忙碌时的 Slash Command 只落盘 Failed 输出消息，不改变当前前台状态。

### A2.1 `!bash` 的插入、状态与回放

`!bash` 目前在执行结束后才持久化 `BashExecution` 消息。为使插入发生在开始时，本阶段即引入：

- 普通 prompt 与 `!bash` / `!!bash` 共用 A4 的可取消 gate 等待与运行令牌安装；尚未获得 gate 时取消，不分配插入消息、不启动 shell。
- 获得 gate 后分配消息 id，发出 `AgentEventPayload::UserBashStarted { run_id: RunId, message_id: MessageId, command: String, cwd: PathBuf, exclude_from_context: bool }`，投影成功后触发插入；`cwd` 是会话绝对路径。执行结束时以同一 id 持久化 `BashExecution` 消息。
- ACP 将 `UserBashStarted` 依次映射为 `running` 与 `UserMessage { messageId, content: [text "!cmd" 或 "!!cmd"], _meta.clawcode.origin: "user_bash" }`。`ProjectionSink` 以其 `run_id` 开始 `OperationKind::UserBash` journal，捕获执行前 baseline，保留开始消息及执行期间更新。
- 新增 `AgentEventPayload::UserBashFinished { run_id: RunId, message_id: MessageId, outcome: AgentOutcome, result_emitted: bool }`。正常完成或取消时，先持久化并同步 `BashExecution`、发出 `MessageEnd`，再以 `result_emitted: true` 发此终态；ACP 映射 `idle` 与对应停止原因，并关闭同一 journal。未成功发出结果消息时设置 `result_emitted: false`：执行器明确返回取消则结算为 `Cancelled`，其他执行器或持久化错误结算为 `Failed`。已接受的 prompt 不再返回 JSON-RPC 错误，持久化失败不得伪造成功，日志保留原始错误。
- 运行中 resume 使用 baseline + journal 重建同 id 的 user message，并按 A5 发送当前 `running` 快照；结束后的持久化回放同样生成同 id 的 `UserMessage`。连接断开不取消执行；每个已发出的开始事件必须有且仅有一个终态，映射或发送失败走标准 error 终态兜底并记录错误。
- 本阶段 `BashExecution` 的展示仍沿用 `_clawcode/event` 投影；阶段 E 改为 terminal 实体并删除该投影。

### A3 停止原因

- `AgentOutcome::Failed { message }`（含 `UserBashFinished`）与 `SlashCommandEnd` 携带的失败映射为 `StopReason::Error(ErrorStopReason { error: Error { code: -32603, message } })`。会话忙碌分支不产生 `SlashCommandEnd`，不在此列（见 A2）。
- `AgentEventPayload::SlashCommandEnd` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] message: Option<String>`，失败时携带脱敏后的失败摘要作为上述 `message`；缺失时使用 `Slash Command failed`。
- 取消映射为 `cancelled`（规范要求确认取消时必须使用）。
- `SlashCommandStatus` 增加 `Cancelled`。Slash Command 执行期间运行令牌被取消（cancel、close），或命令返回 `ModelError::Cancelled`（如 `/compact` 的摘要请求被中止）时，输出消息与 `SlashCommandEnd` 的状态为 `Cancelled`，映射为 `cancelled` 停止原因，不再统一转为 `Failed`；其余错误仍为 `Failed`。旧记录没有该值，读取不受影响；WebUI 对 `Cancelled` 输出按已取消展示。
- 删除自定义停止原因 `_clawcode/error`（`ProductIdentity::ACP_ERROR_STOP_REASON`）。

### A4 cancel 与 close 覆盖未插入 prompt

- `SessionExecution` 增加等待者取消令牌 `waiters: Mutex<CancellationToken>`（与 `close_requested` 同一模式），提供 `acquire_operation_for_prompt(cancellation_parent: Option<&CancellationToken>, waiters: WaiterCancellation) -> Result<(MutexGuard<'_, ()>, CancellationToken), KernelError>`。普通 `run_with_source` 与 `execute_user_bash_operation` 都经此方法等待 gate 并安装运行令牌，普通维护操作保留 `acquire_operation`。
- 等待时同时监听关闭令牌与可选父取消令牌，`WaiterCancellation::Reject` 时还监听等待者令牌，取消优先；拿到 gate 后再次检查该代令牌。等待者代际检查与运行令牌安装在同一临界区内完成，并与 `cancel()` 使用一致锁序，避免取消恰好落在获锁与安装之间。插入前的取消或 close 统一返回 `KernelError::PromptCancelled`，不插入消息、不启动 shell。
- `SessionExecution::cancel()` 取消当前 run 令牌并取消、替换等待者令牌。因此 `cancel_session` 同时取消前台工作与等待 gate 的 prompt，后者由 ACP 返回 `-32800`。
- 取消范围：只有带 `InsertionNotice` 的 run（有外部客户端在等待结果，被拒绝时收到 `-32800`，文本仍在客户端）参与等待者取消与插入前复查。内部来源（`Kernel::submit`，包括后台 subagent 回报 `background.rs:216`；扩展 `send_user_message`，`host.rs:262`）被拒绝时没有任何一方持有输入文本，因此：
  - `WaiterCancellation` 由 `impl From<Option<&InsertionNotice>> for WaiterCancellation` 得出：有通知为 `Reject`，否则为 `Ignore`。内部来源等待 gate 时不响应 `cancel_session`，仍响应自己的 `cancellation_parent` 与 close。
  - 内部来源不做插入前复查；在插入前因取消结束（如预压缩返回 `ModelError::Cancelled`）时，先以预分配的消息 id 落盘首条用户消息并发 `MessageEnd`，再以 `AgentOutcome::Cancelled` 结算，保证输入留在历史中，不会悄悄丢失。
- `cancel()` 的调用方还包括 `background_subagent stop`、关闭流程与 `Kernel::submit` 的父取消分支。`Kernel::submit` 的空闲分支仍可能因 `cancellation_parent` 或 close 得到 `KernelError::PromptCancelled`，必须转换为 `AnswerOutcome::Cancelled`，使前台 subagent 报告「Subagent was stopped before answering.」或 `Cancelled`，Reporter 跳过回报，而不是当作基础设施失败。
- `session/close`：先保留当前连接的 watcher 发送确认句柄，再调用 `Kernel::close_session` 沿所有权级联关闭子会话。取得关闭后的投影 sequence，按 A4.1 等待当前连接已发送至该位置，之后才发送响应、清理 watcher 与 projection；有被取消的前台工作时，这确保 `idle(cancelled)` 排在响应前。已空闲会话不伪造取消终态；等待中的 prompt 返回 `-32800`。
- close 的等待与响应放在 `connection.spawn` 派生的任务中，处理回调完成活跃检查后立即返回。原因同 A2 第 3 步：`Kernel::close_session` 的 `acquire_close().await`（`session.rs:383`）要等当前 run 真正退出，现有处理器在回调内等待（`server.rs:1010`），run 卡在不响应取消的 hook、工具或 MCP 调用时，整个连接（该标签页的所有会话）的 prompt、cancel 与 elicitation 回复都会被挡住。回调返回后，同一连接上紧随其后的同会话请求可能先于关闭完成被处理，由 Kernel 的 `SessionClosing` 等现有错误拒绝。
- close 宽限期：派生任务最多等待 `Kernel::close_session` 10 秒（ACP 常量 `CLOSE_GRACE`）。超时后先以 `-32603`、`data = { sessionId, closing: true }` 响应并记录 warn，任务继续等待关闭完成，完成后再清理 watcher 与 projection；超时响应不保证 `idle(cancelled)` 在前。WebUI 目前不调用 `session/close`（`protocol.ts` 只声明了方法名），宽限期主要保护其他客户端。
- `shortcut:` 宽限期只放弃等待，不强制终止卡住的 run：从外部丢弃 run 的 future 可能留下写了一半的持久化状态，进程内扩展也无法被安全强杀。升级路径是为 run 提供可中止句柄，中止时把会话标记为需要恢复。
- 排队 follow-up 的撤回语义留给子项目 2（届时 follow-up 也经 `session/prompt` 提交）。

### A4.1 连接侧发送确认

- Kernel 的 `emit().await` 仅代表事件已投影/广播，不代表连接 watcher 已发送通知。`SessionProjection` 为所有事件组记录 `last_sequence: Option<Sequence>`，包括空闲期没有 operation journal 的配置事件；它与恢复游标分开，由 `ProjectionRegistry::last_sequence(&SessionId) -> Result<Option<Sequence>, ProjectionError>` 提供。没有目标 sequence 时跳过等待。
- 当前连接每个 Session 的 watcher 持有 `SessionDelivery` 发送进度：用 `tokio::sync::watch` 发布已发送的 `Option<Sequence>`，仅在该组所有通知均成功调用同一 SDK 连接的 `send_notification` 后推进。提供 `async fn wait_through(&self, sequence: Sequence) -> Result<(), agent_client_protocol::Error>`；响应处理器等待目标位置后才调用 `responder.respond`。确认的是同一出站队列的顺序，不要求客户端额外回执。
- `SessionDelivery` 另提供发送许可 `async fn send_permit(&self) -> SendPermit<'_>`：watcher 发送每组通知时持有同一把锁，持有许可期间该连接不发送新通知。调用方必须先 `wait_through` 再取许可（反之会因 watcher 无法推进而死锁），持有许可期间只做内存读取与 `responder.respond`。
- 回放/订阅的初始位置与发送进度原子衔接，不把订阅前无需重发的历史事件当成永久等待项。断线、watcher 退出或无法补齐的 lag 必须结束等待并返回错误，不能把跳过未发送事件当成确认。不得先删除 projection 或取消 watcher 再等待。
- `SessionDelivery` 放在 `crates/acp/src/delivery.rs`。`BatchComponent` 必须保持通知在响应之前的帧顺序。close 与 D2 的 config 修改复用此机制。
- 测试分两层。集成测试只能使用公开 API，拿不到 `pub(crate)` 的发送许可，而 CLAUDE.md 第 10 条禁止为测试加生产钩子；让客户端停止读取也无法阻塞 watcher，因为 `send_notification` 只是入队。因此「watcher 尚未发送」的场景放在 `delivery.rs` 的 `#[cfg(test)] mod tests`，用发送许可暂停 watcher、丢弃发布端模拟 watcher 退出；集成测试只断言端到端帧顺序。

### A5 resume 当前状态同步

- 回放日志时过滤历史 `StateUpdate`（规范禁止将历史状态当作当前活动重放），包括 `start` 回放与 `_clawcode/event` 游标回放。
- `SessionProjection` 记录最后一次投影的前台状态（`running`、`requires_action` 或 `idle`），在建立订阅的同一临界区内读出。回放完成后、响应之前，发送该状态的 `state_update` 快照；会话没有投影记录时快照为 `idle`。idle 快照不携带新的完成或取消语义。
- 快照与订阅原子获取，之后的实时状态从订阅中按序投递，因此不会出现旧快照覆盖较新状态。

### A6 initialize 与基础校验

- 版本协商：选择不高于请求版本的最高支持版本；无可选版本返回 `-32022`，`data = { requested, supported }`。SDK 3.6 已实现大部分规则，服务端仅保证配置与响应正确。
- `null` 请求 ID、初始化前的请求（`-32600`）由 SDK 3.6 处理。
- 可用命令按 `AvailableCommand::Command` 发送；`AvailableCommandsUpdate` 结构随之调整。

### A7 WebUI

- `session/prompt` 结果读取 `messageId`；若 `user_message` 更新先到，按 id 对齐，不重复创建。
- 输入框：A2 之后 prompt 响应要等到插入，预压缩等插入前工作可能持续数十秒。现有 Composer 在响应成功后才清空输入框（`Composer.tsx:114-120`），而等待期间输入框仍可编辑，用户在此期间继续输入的内容会被一并清掉。改为提交时立即清空输入框并保留本次提交的副本：
  - 成功：丢弃副本。
  - `-32800`（插入前被取消）：把副本放回输入框，输入框已有新内容时副本放在前面并以空行分隔，资源与图片合并（与 pi 的 `restoreQueuedMessagesToEditor` 一致）；提示「已取消，内容已放回输入框」，不显示为错误。
  - 其他错误：同样放回，并显示错误。
  - 草稿存储随输入框内容更新，不在成功后统一清除。
- 可用命令按 `type` 解析，仅渲染 `command`，其余保留不展示。
- `state_update`：`running` 与 `requires_action` 都视为运行中，`idle` 为空闲；未知状态不改变运行判断。
- `_meta.clawcode.origin === "user_bash"` 的 `UserMessage` 不单独渲染，由对应的 bash 卡片展示，避免同一条 `!bash` 出现两次。
- initialize 接受 1.13 响应结构。

## 阶段 B：状态与实体对齐

### B1 requires_action

- Kernel 新增事件 `AgentEventPayload::ActionRequired { request_id }` 与 `ActionResolved { request_id }`：有 active run 且挂起的 elicitation 数从 0 变 1 时发出前者，回到 0 且 run 仍在进行时发出后者。没有 active run 的 elicitation 属于后台活动，不改变状态。
- 映射为 `StateUpdate::RequiresAction` 与 `StateUpdate::Running`，并由 A5 的 `SessionProjection` 记录为最后前台状态，resume 快照因此能报告 `requires_action`。

### B2 setup 响应初始状态

- `session/new` 与 `session/resume` 响应携带 `availableCommands`（空列表时省略，不发 `null`），阶段 D 起同时携带 `configOptions`。
- resume 回放完成后、响应之前依次发送 `session_info_update`（当前标题）与 A5 的状态快照。

### B3 工具调用

- 工具开始时设置 `name` 为真实工具名，`title` 保持不变。
- `kind` 映射，由 ACP 侧的 `ToolKindOf<'a>(&'a ToolCall)` 包装类型实现 `From<ToolKindOf<'_>> for wire::ToolKind`：

| 工具 | kind |
|---|---|
| `read` | `read` |
| `write`、`edit` | `edit` |
| `bash` | `execute` |
| `tool_search` | `search` |
| 其他（含 `subagent`、`background_subagent`、MCP 工具） | `other` |

- `locations`：`read`、`write`、`edit` 设置 `[{ path }]`，path 按会话 cwd 解析为绝对路径；`read` 有 `offset` 时设置从 1 开始的 `line`。
- `protocol::ToolResult` 与持久化的 `MessageContent::ToolResult` 都新增 `#[serde(default, skip_serializing_if = "std::ops::Not::not")] cancelled: bool`（前者为公开字段，builder 默认 `false`）。旧记录缺字段按 `false` 读取。
- 工具批次在工具返回 `ToolError::Cancelled` 或 Turn 取消令牌已触发时设为 `true`，构造结果消息时原样复制；扩展结果替换和所有重建路径必须保留该标记。`ToolExecutionEnd`、后续实时 `MessageEnd` 与持久化回放统一优先判断 `cancelled`，再判断 `is_error`，避免终态从 cancelled 退回 failed/completed。

### B4 compaction 实体

- `CompactionStart` → `CompactionUpdate { compactionId, status: in_progress }`。
- `CompactionEnd` → 按 `CompactionOutcome` 输出 `completed`（`summary` 为摘要内容块）、`failed`（`error` 为失败文本）或 `cancelled`。规范要求报告过的 compaction 结束时必须给出终态。
- `compactionId` 由该压缩操作的 run_id 派生，同一会话内不重复使用。
- WebUI 改读标准 `CompactionUpdate`；`_clawcode/event` 通用透传保留，WebUI 不再从中解析 compaction。

### B5 Session notices

- 以下事件映射为 `Notice { severity: warning, title, description }`：`ExtensionHandlerFailed`、`SkillDiagnostic`、Session 启动时的 prompt 诊断、MCP server 连接失败（来自 `McpSessionEvent` 状态变化）。
- notice 只实时推送：投影将其标记为不进入回放日志。
- Session 启动时的 prompt 诊断在 `session/new` 或 `session/resume` 响应之后发送，确保客户端已知该会话 id。
- 内容不得包含凭据或敏感值；description 复用现有脱敏诊断文本。title 使用简短英文，例如 `Extension handler failed`、`Skill expansion failed`、`MCP server unavailable`。
- WebUI 新增轻量 toast 区域展示 notice。

### B6 idle 用量

- `AgentSettled` 增加 `usage: ModelUsage`：本次 run 全部已结算 assistant attempt 的用量之和，包含有已知用量的失败、取消与重试 attempt；不混入其他 run 或独立 compaction 的用量。
- `run_with_source` 在内部 `execution` future 之外持有本次 run 的 `ModelUsage` 累加值；每个 assistant attempt 结算后，在扩展 message-end 校验、持久化和其他可能失败的操作前累加一次。`Ok`、`Err` 与取消结算都把同一累加值传给 `RunSettlement` 的必填 `usage` 字段，由它传给 `AgentSettled`；不得依赖错误路径默认为空的 `messages`，不得通过扫描整个会话代替本轮统计。
- 映射为 `IdleStateUpdate.usage`：`total_tokens`、`input_tokens`、`output_tokens`、`thought_tokens`、`cached_read_tokens`、`cached_write_tokens`。`thought_tokens` 来自 `reasoning_tokens: Option<u64>`：所有回复都没有该值时为 `None`，否则对有值的回复求和。
- ACP 定义本地 `UsageOf<'a>(pub &'a ModelUsage)`，实现 `From<UsageOf<'_>> for wire::Usage`。
- WebUI 现有按 run 汇总用量的逻辑不变。

## 阶段 C：标准 elicitation

### C1 Kernel

- 保留挂起表、oneshot 回复与 Turn 取消转 `Cancel` 的行为；保留 `mcp_elicitations` 快照用于重连重建。
- `McpRequestContext` 增加 `#[serde(default)] tool_call_id: Option<ToolCallId>`，MCP 工具执行时填入。
- `resolve_mcp_elicitation` 对已结算的请求返回 `KernelError::ElicitationResolved(String)`，用于区分「已被其他客户端回答」。

### C2 ACP：ElicitationBridge

```rust
/// Per-connection bridge between Kernel MCP elicitations and ACP `elicitation/create`.
pub(crate) struct ElicitationBridge {
    modes: ElicitationModes,
    outstanding: Mutex<HashMap<(SessionId, String), RequestCancellationHandle>>,
}
```

- initialize 时从客户端 `capabilities.elicitation` 记录支持的模式（form、url）。
- ACP 定义本地 `ElicitationOf<'a> { pub session_id: &'a SessionId, pub request: &'a McpElicitationRequest }`，实现 `From<ElicitationOf<'_>> for wire::CreateElicitationRequest`；不使用包含外部类型的 tuple 作为孤儿规则的规避方式。
- 数据来源：`EventGroup` 增加带类型的旁路字段 `elicitations: Vec<ElicitationChange>`（`Requested(McpElicitationRequest)` 或 `Resolved { request_id }`），由 `ProjectionSink` 在映射 `McpElicitationRequested` / `McpElicitationResolved` 时填入。连接的会话 watcher 只处理实时组中的该字段；日志回放时忽略它，避免对已结算请求重发。断线重建统一使用 `kernel.mcp_elicitations()` 快照。
- watcher 收到 `Requested`：若连接支持该模式，发送 `elicitation/create`，参数为 `sessionId`、可选 `toolCallId`、`mode`、`message`，form 模式附 `requestedSchema`，url 模式附 `url` 与 `elicitationId`。使用 SDK 3.6 的请求取消句柄发送并记录。watcher 不得原地等待回复，回复由 `connection.spawn` 派生的任务处理；否则用户填表期间该会话的通知全部停住，`$/cancel_request` 也发不出去。
- 发送前编译 `requestedSchema`：无法编译时不发送请求，直接以 `cancel` 回复 kernel 并记录 warn（客户端同样无法渲染，发送只会得到 `-32602`，而请求失败不改变挂起状态，run 会一直停在 `requires_action`）。
- 收到响应：未知 action 按 `cancel` 处理；`accept` 时按 `requestedSchema` 校验 `content`，不合法则按 `cancel` 回复并记录 warn；随后调用 `kernel.mcp_respond_elicitation`。`ElicitationResolved` 表示已被其他连接回答，忽略。校验使用根依赖表已声明的 `jsonschema`，`crates/acp/Cargo.toml` 增加 `jsonschema = { workspace = true }`。
- watcher 收到 `Resolved`（任一连接回答或 Turn 取消）：对本连接仍在等待的同一请求发送 `$/cancel_request`。
- 连接 resume 会话：在回放、状态快照之后，把 `kernel.mcp_elicitations(S)` 中仍挂起的请求重新发给该连接。
- 没有连接支持该模式或没有连接附着时，请求保持挂起，直到支持的连接附着或 Turn 取消。
- 请求失败或连接断开时不改变 kernel 挂起状态。
- 删除自定义方法 `_clawcode/mcp/elicitation/list` 与 `_clawcode/mcp/elicitation/respond` 及其注册。
- `shortcut:` 不发送 `elicitation/complete`：mcp crate 尚未处理 MCP URL 完成通知；升级路径是在 MCP 客户端接收完成通知后转发。

### C3 WebUI

- initialize 声明 `capabilities.elicitation: { form: {}, url: {} }`。
- `AcpConnection` 增加 agent 请求处理注册表：注册 `elicitation/create`；处理 `$/cancel_request` 通知；未注册方法仍回 `-32601`。
- 表单弹窗按规范受限 schema 子集渲染：文本（`minLength`、`maxLength`、`pattern`、`format`）、单选（`enum` 或 `oneOf`）、数字与整数、布尔、多选；预填合法的 `default`；提交前校验。无法渲染（必填字段类型未知、`required` 无对应字段、同时设置 `enum` 与 `oneOf`）时回复 `-32602`。
- URL 弹窗显示目标 host，用户确认后 `window.open` 并回复 `accept`。
- 弹窗标明会话与 MCP server，提供「拒绝」「取消」；多个请求排队、一次一个；收到 `$/cancel_request` 时关闭对应弹窗。
- 删除旧的 `McpElicitationDialog` 拉取流程与 `_clawcode/event` 中的 elicitation 解析。

## 阶段 D：session config options

### D1 Kernel

```rust
/// Borrows one live Session for persisted model or thinking changes.
pub(in crate::runtime) struct SessionSettingsUpdate<'a> {
    kernel: &'a Kernel,
    session: &'a Arc<Session>,
}

impl SessionSettingsUpdate<'_> {
    /// Persists and selects one configured model for future Turn snapshots.
    async fn model(&self, provider: &str, model_id: &str) -> Result<ModelProfile, KernelError>;
    /// Persists and selects one thinking level for future Turn snapshots.
    async fn thinking_level(&self, level: ThinkingLevel) -> Result<ThinkingLevel, KernelError>;
}
```

- 由 `KernelExtensionHost::set_model` / `set_thinking_level` 抽出，二者改为调用它；写入使用 `SettingChange`；继续发出扩展 `model_select` 事件。
- 公开 API：`Kernel::session_settings(&SessionId) -> SessionSettings { model, thinking_level, models }`（`models` 来自 `ModelCatalog::profiles()`）、`Kernel::set_session_model`、`Kernel::set_session_thinking_level`。
- 新增 `AgentEventPayload::SettingsChanged { model, thinking_level }`，经 `session_sink` 发出；扩展触发的修改同样经过它。
- 运行中允许修改，自下一次模型请求生效（Turn 开始时读取设置快照的现有行为）。

### D2 ACP

- ACP 定义本地 `ConfigOptionsOf<'a>(pub &'a SessionSettings)`，实现 `From<ConfigOptionsOf<'_>> for Vec<wire::SessionConfigOption>`，按优先级输出两个 `select`：
  1. `model`：category `model`，值为 `"{provider_id}/{model_id}"`，name 为 display name，description 为 provider 与上下文长度。解析时不拆分字符串，而是与模型目录中每项的同格式值整体比较，因此 model id 本身含 `/`（如 `deepseek/deepseek-chat`）也不会歧义。
  2. `thinking_level`：category `thought_level`，值为 `ThinkingLevel` 全部变体。`shortcut:` `ModelProfile` 无推理能力字段，所有模型提供全部等级；升级路径是 provider 暴露推理能力后按模型过滤。
- `session/new` 与 `session/resume` 响应携带 `configOptions`。
- `session/set_config_option`：校验 `configId`、`type` 与可选值（不合法返回 `-32602`），调用 kernel，返回完整列表。
- `SettingsChanged` 映射为 `ConfigOptionUpdate`。处理器在 Kernel 修改完成后读取投影 `last_sequence`，用 A4.1 的 `SessionDelivery::wait_through` 等待当前连接发送至该位置；随后取得该连接的发送许可，在许可内重新读取 Kernel 当前设置快照并经同一 SDK 连接发送响应，再释放许可。不能以 Kernel 已发出事件替代连接侧确认。值未变化且未产生新事件时不等待不存在的通知，但仍在发送许可内读取快照并响应；扩展修改仍向所有附着连接广播配置更新。
- 顺序保证：Kernel 先更新设置再投影事件，许可内读到的快照不旧于该连接任何已发出的 `ConfigOptionUpdate`；许可期间不会再发出更早的通知。因此另一连接或扩展的并发修改不会出现“通知 A → 通知 B → 请求 A 的旧响应”的回退，响应可能直接反映更新的值，其后到达的通知只会把状态推向更新的值。仅等待 `delivered >= sequence` 不能提供这一保证。

### D3 WebUI

- header 固定模型名替换为模型与 thinking 两个紧凑下拉框，数据来自 setup 响应的 `configOptions` 与 `config_option_update`。
- 切换时调用 `session/set_config_option`，请求中禁用下拉框，失败显示错误。
- 未知选项类型忽略；会话无配置项时回退显示原模型名。

## 阶段 E：terminal 实体

### E1 Kernel

- `ToolResultDetails::Bash` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] exit_code: Option<i32>` 与 `signal: Option<String>`。现有 `BashTermination::Exited` 不能还原实际信号：执行器必须在丢弃原始 `ExitStatus` 前读取 unix `signal()` 并转换为信号名称，读取代码放在 `#[cfg(unix)]` 下，其他平台为 `None`（`tools/src/bash.rs:281` 已有 `cfg(windows)` 分支，项目需在 Windows 上编译）；超时或中止由我们终止时记录 `SIGKILL`，未知时保持 `None`。
- `UserBashResult` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)] signal: Option<String>`，供共享 `BashExecutionOutput.result`、扩展替代执行结果与持久化 `BashExecutionMessage.result` 共用。工具适配器将其复制到 `ToolResultDetails::Bash`，不能只保留在进程内的临时结果中；旧消息无此字段时按 `None` 读取。
- 工具的非零退出、超时与中止也保留 output、exit_code、signal 及截断信息。不能先转为只含文本的 `ToolError::Execution` 再丢弃终态 details；在工具结果中保留原有错误文本与 `is_error`，取消标记按 B3 贯穿持久化。
- `!bash` 的实时事件统一为 `AgentEventPayload::UserBashOutput { run_id: RunId, message_id: MessageId, output: String }`；`output` 是执行器当前可见尾部的完整替换快照，不是增量，也不把 tools crate 的 `BashOutputSnapshot` 类型带进 protocol。它按 A2.1 的 `run_id` 写入 user-bash journal。
- 开始时的 cwd 来自 A2.1 的 `UserBashStarted.cwd`；`BashExecutionMessage` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)] cwd: Option<PathBuf>`，新消息保存绝对路径，旧消息缺失时回放省略 cwd，不根据当前目录猜测历史路径。

### E2 ACP：bash 工具

- 开始：`ToolCallUpdate { kind: execute, name: "bash", content: [terminal { terminalId: tool_call_id }] }`，随后 `TerminalUpdate { terminalId, command, cwd }`（cwd 为会话绝对路径）。
- 实时：每个节流快照只发送 `TerminalUpdate { output: base64(当前可见尾部) }`，使用权威替换快照；不再同时发送带 `content` 与 `raw_output` 的 in_progress `ToolCallUpdate`（现有映射 `mapping.rs:210-220` 每次快照把尾部发两份）。`shortcut:` 不发送 `terminal_output_chunk`，因为 kernel 推送的是被截断的尾部快照，无法计算增量；升级路径是 kernel 推送增量字节。
- 结束：`TerminalUpdate { output: 最终快照, exitStatus { exitCode, signal } }`，随后工具终态 `ToolCallUpdate`（completed、failed、cancelled）。输出被截断时 content 额外附一条说明完整输出路径的文本。
- 回放：由持久化的工具结果生成终态 `TerminalUpdate`。
- `terminalId` 使用 `tool_call_id`；不设置 `size`（不在 PTY 中运行）。

### E3 ACP：`!bash`

- 开始：A2.1 的 `UserMessage` 之后，发送 `ToolCallUpdate { toolCallId: message_id, kind: execute, name: "bash", title: cmd, status: in_progress, content: [terminal { terminalId: message_id }], _meta.clawcode: { origin: "user", excludeFromContext } }` 与 `TerminalUpdate { command, cwd }`。
- 实时：`UserBashOutput` → `TerminalUpdate { output }`。
- 结束：`TerminalUpdate { 最终输出, exitStatus }` 与工具终态 `ToolCallUpdate`。
- 上述正常结束更新来自持久化消息的 `MessageEnd`，之后才由 A2.1 的 `UserBashFinished` 发出 idle。`UserBashFinished { result_emitted: false, .. }` 负责结束尚无结果消息的 terminal/tool（未知退出信息为 `exitStatus: {}`，保留已有输出，不伪造 exit code 或 signal）：按 outcome 发 failed 或 cancelled 工具终态，再发 idle(error/cancelled)；`result_emitted: true` 时不重复终结工具。
- 运行中回放 journal 重建同 id 的消息、工具、terminal 与最新输出；结束后持久化回放依次生成同 id 的 `UserMessage`、`ToolCallUpdate` 终态与 `TerminalUpdate` 终态快照，cwd 与 signal 来自保存的消息。
- 删除 `BashExecution` 的 `_clawcode/event` 投影。

### E3.1 快照保留

- 问题：运行中操作的每个事件组都追加进 journal（`projection.rs:216`），没有上限；bash 每 100 ms 一次快照（`tools/src/bash.rs:16`），每次最多 50 KB 尾部（`truncation.rs:7`）。`!npm run dev` 这类命令持续输出一小时约 3.6 万个快照，journal 达到 GB 级，重连 resume 还会把它们全部重放。
- `EventGroup` 增加 `#[builder(default)] snapshot_of: Option<String>`，值为 `terminalId`，仅输出快照组（bash 工具与 `!bash` 的实时 `TerminalUpdate`）设置。journal 追加带该键的组时，先删除同键的旧组，再在末尾追加新组，保持 journal 按 sequence 有序。开始与终态更新不设置该键，始终保留。
- 游标恢复时，若游标指向已被替换的快照，按「sequence 不小于游标」取到的就是最新快照，因为快照本身是整体替换，结果等价。
- `shortcut:` 发送队列不合并快照：WebSocket 出站队列无界（http crate `connection.rs:39`），标签页停止读取但连接未断时快照仍会累积；升级路径是在 watcher 中按 `terminalId` 只保留尚未发送的最新快照。

### E4 WebUI

- 新增 `TerminalView`：按 `terminalId` 保存状态；`terminal_update` 字段为补丁（`null` 清空、具体值替换，`output` 整体替换）；`terminal_output_chunk` 用流式 `TextDecoder` 逐块追加。以 `<pre>` 渲染并剥离 ANSI 转义，不自动链接，不响应终端查询；显示退出码或信号。
- `ToolCallCard` 遇到 terminal 内容项渲染 `TerminalView`，运行中自动展开；`_meta.clawcode.origin === "user"` 的卡片使用「用户执行」样式，替代 `BashExecutionCard`；删除旧 `bash_execution` 解析。

## 错误码

| 情况 | 返回 |
|---|---|
| 参数非法、不支持的值（非法 `configId`、无法渲染的表单等） | `-32602` |
| 插入前被取消（cancel、close；含等待 gate 期间，以及获锁后、插入前） | `-32800` |
| 会话不存在或未在当前连接上活跃（该连接从未 new 或 resume 过它） | `-32002`，`data.sessionId` 为请求的 id |
| 版本协商失败 | `-32022`，`data = { requested, supported }` |
| close 超过宽限期仍未完成（后台继续关闭） | `-32603`，`data = { sessionId, closing: true }` |
| 插入后结束前台工作的失败 | 不返回 JSON-RPC 错误，仅 `idle` + `error` 停止原因 |
| 插入后因会话忙碌被拒绝的 Slash Command | 不返回 JSON-RPC 错误，通过 Failed 输出消息呈现，不发状态更新 |
| 其他内部错误 | `-32603` |

活跃会话由连接层现有的 session watcher 记录。`-32002` 规则只用于标准会话请求（`session/prompt`、`session/close`、`session/set_config_option`）。`session/cancel` 是通知，无法返回错误：对当前连接上不活跃的会话照常执行取消（后台面板的「停止」依赖此行为）。自定义 `_clawcode/*` 方法不受约束，例如后台面板轮询的 `_clawcode/session/runtime`。

## 日志

遵循 CLAUDE.md 第 13、14 条，只记录边界，不记录消息正文、表单内容或 terminal 输出：

- prompt 已插入（message id）、插入前拒绝或取消。
- user-bash 开始、结束与失败（run id、message id、结果）；等待 gate 时取消。
- 内部来源在插入前遇到取消后补落盘用户消息（session id、run id、input source）。
- resume 发送状态快照（状态值）。
- 发送确认失败或 watcher 关闭（session id、目标 sequence），不得伪报已发送。
- close 超过宽限期（session id），以及后台关闭最终完成或失败。
- elicitation 发送、回复、取消、重建，以及因 schema 不合法改为 cancel。
- config option 修改（configId 与新值 id）。
- 版本协商结果。
- notice 产生（severity 与 title）。

## 测试

按 TDD 实施；测试只放在 crate 的 `tests/` 或 `#[cfg(test)] mod tests`。ACP 测试使用 SDK 3.6 的真实客户端连接。

- 阶段 A
  - Kernel：`InsertionNotice` 在消息落盘时触发；Slash Command（含会话忙碌分支）、扩展接管的输入同样触发；model preflight 失败调用 `rejected`，input hook 错误仅发诊断且继续插入；`cancel_session` / close 取消等待 gate 的普通 prompt、`!bash` 与 `!!bash`，用临时文件断言 shell 未执行，并覆盖获锁与运行令牌安装边界；`Kernel::submit` 遇到 `PromptCancelled` 返回 `AnswerOutcome::Cancelled`；内部来源（后台 subagent 回报、扩展 `send_user_message`）等待 gate 时 `cancel_session` 不拒绝它们，插入前 hook 阻塞或预压缩期间取消时首条用户消息仍落盘并以 `Cancelled` 结算；获锁后阻塞在 hook 时 cancel，释放 hook 后返回 `PromptCancelled`、无用户消息，且结算为 `idle(cancelled)`；预压缩被取消同样返回 `PromptCancelled`；`/compact` 执行中 cancel 与 close 均以 `SlashCommandEnd { status: Cancelled }` 结束，其他失败仍为 `Failed`。
  - user-bash：开始时插入且结束后以同一 id 持久化；成功、取消、执行器错误均只结算一次；长命令运行中以 start 或游标 resume 重建开始消息和最新输出（输出自阶段 E 起），快照为 running；结束后快照为 idle；发起连接断开后执行继续。
  - ACP：prompt 响应 `messageId` 与 user message 更新 id 一致；插入前失败返回映射错误且无 user message 更新；等待中取消返回 `-32800`（cancel 与 prompt 在同一连接上发送，验证处理回调不占住分发循环）；两个等待 gate 的 prompt 都能插入并各带自己的 `messageId`（不断言顺序）；插入后失败报 `idle` + `error`（会话忙碌时的 Slash Command 除外：已插入，带 Failed 输出消息，不改变前台状态）；hook 阻塞期间取消的 prompt 返回 `-32800` 且无 user message，随后为 `idle(cancelled)`；`/compact` 执行中取消为 `idle(cancelled)`，close 时 `idle(cancelled)` 先于 close 响应；Slash Command 失败携带摘要；resume 回放（含游标）无历史 `state_update`，当前快照位于回放后与响应前；版本协商与 `-32022`；命令带 `type`；未活跃会话 prompt 返回 `-32002`，cancel 照常执行。
  - 发送确认（`delivery.rs` 单元测试）：进度推进前 `wait_through` 一直等待；持有发送许可时 watcher 不能发布；发布端丢弃时等待以错误结束。
  - 发送确认（集成测试）：close 的实际帧顺序为 idle(cancelled) → close response，batching 不改变顺序；空闲 close 不等待不存在的终态；run 卡在不响应取消的 hook 时，close 未完成期间同一连接上其他会话的请求仍被处理；超过宽限期返回 `-32603`（`data.closing = true`），hook 释放后后台关闭完成并清理 watcher。
- 阶段 B：`requires_action` 随 elicitation 挂起与解决切换；工具 `name`、`kind`、`locations`；取消标记通过 `ToolExecutionEnd` → `MessageEnd` → 持久化 → 回放始终为 cancelled，旧记录默认 false；compaction 实体终态；notice 不进入回放；idle usage 汇总本轮全部 attempt，覆盖已有消耗后内部失败、取消、重试不重计、无消耗为零及 reasoning 的 None/Some；setup 响应带 `availableCommands`；`UsageOf` 在 ACP crate 内编译通过。
- 阶段 C：`ElicitationOf` 转换在 ACP crate 内编译通过；声明能力的连接收到 `elicitation/create` 并回复后 kernel 得到结果；同会话另一连接收到 `$/cancel_request`；重连后重建请求；日志回放不重发已结算请求；未知 action 按 cancel；不合 schema 的内容按 cancel；无法编译的 `requestedSchema` 不发送请求，直接按 cancel；未声明能力的连接收不到请求。
- 阶段 D：`ConfigOptionsOf` 转换在 ACP crate 内编译通过；new 与 resume 带 `configOptions`；合法修改成功、非法返回 `-32602`；含 `/` 的 model id 正确匹配；`ConfigOptionUpdate` 先于响应；两个连接与扩展并发反复修改时，每个连接按到达顺序应用全部配置消息后，最终状态等于 Kernel 的最终设置（不断言具体帧顺序：请求 A 的响应可能落在更新 A 与更新 B 之间并直接携带 B）；相同值不等待新事件；扩展修改同样触发更新；resume 后设置保持。
- 阶段 E：bash terminal 事件顺序（开始 → 快照 → 带退出码终态 → 工具终态 → user-bash idle）；非零退出、超时、中止均保留 details；`!kill -TERM $$` 的 signal 为 SIGTERM，停止为 SIGKILL，持久化回放保持；`UserBashStarted.cwd` 与保存的 cwd 一致；旧消息缺 cwd/signal 可回放；`UserBashOutput.output` 替换尾部快照，运行中 resume 保留输出；执行器失败终结 terminal/tool 且不重复结算；同一 terminal 的多次快照在 journal 中只保留最新一份，开始与终态更新保留，游标指向被替换快照时重放最新快照；bash 进度只发 `TerminalUpdate`。

## 验收

按 CLAUDE.md 第 9 条，每个阶段使用生产 `app` 后端与已配置 provider 做 WebUI 端到端验收：

- A：发送 prompt；长时间 `!bash` 运行中刷新与重连后仍显示运行；普通 run 占锁时提交 `!bash` / `!!bash` 后停止，确认 shell 未执行；后台 subagent 回报触发父 run 后立即停止，回报仍出现在父会话历史中；提交后在插入前（如预压缩期间）停止，提交的内容回到输入框且等待期间新输入的内容保留；停止/关闭后的状态正确。
- B：工具取消后及刷新后均显示「已取消」；compaction 显示；notice toast。
- C：MCP 表单与 URL 弹窗（测试数据目录中配置一个会发起 elicitation 的 MCP server）；多标签页先答者生效、另一页弹窗关闭；刷新后重建。
- D：切换模型与 thinking，刷新后保持。
- E：bash 与 `!bash` 的 terminal 显示、退出码与停止；长命令运行中刷新恢复输出；信号退出后刷新仍显示原 signal。

## 后续

- 子项目 2：steer / follow-up 经 `session/prompt` 的 `_meta.clawcode.whenBusy` 切换，排队输入在被取出时插入并响应，按提交顺序插入（本期放宽的顺序保证在此补上）；cancel 撤回排队输入。
- 子项目 3：subagent 映射到 `unstable_subagents`。
- MCP OAuth 改为 URL 模式 elicitation；`elicitation/complete`；terminal 增量输出。

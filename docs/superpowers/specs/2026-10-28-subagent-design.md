# Subagent 设计

## 目标

让模型能把自包含任务委派给隔离上下文的子 agent，并支持具名的后台持久 subagent。技术决策与语义参考 pi-durable（`packages/durable/docs/spec.md` §2、§5.4、§5.5、§7.3、§12，`test/examples/21-subagent-foreground.ts`、`22-subagent-background.ts`），在 clawcode 现有 Session / Kernel / ACP 架构上落地。

成功标准：

- 模型调用 `subagent { task }`，子会话在隔离上下文中完成任务，答案作为工具结果返回。
- 模型通过 `background_subagent` 创建、投递消息、停止、查询具名后台 subagent；后台答案作为 follow-up 输入回投父会话。
- 父会话 abort 级联停止前台子会话；后台子会话处于 background 边界之外。
- 关闭、删除父会话级联处理其子会话；单独恢复子会话时先恢复 owner。
- 子会话共享 owner 会话的 MCP 连接，不重复拉起 MCP server。
- WebUI 在工具卡片内嵌子会话实时过程，可跳转到子会话继续追问，侧栏按 owner 嵌套。
- 用量只记在各自会话内，不重复计算。

## 与 pi-durable 的对应关系

pi-durable 把 harness 原语与 subagent 产品代码分开：harness 提供所有权、建会话、提交、级联、details；subagent 只是基于原语的扩展代码。clawcode 采用同样分层：

| pi-durable | clawcode |
|---|---|
| `ConversationRecord.owner`（与 fork `parent` 分离） | `V4Header.ownership`（与 `parent_session_id` 分离） |
| 任务拥有的会话复制 owner 的 `pi.agent` | `AgentSnapshot` 在子会话构建后、启动 extension 前应用 model / thinking / 工具选择 |
| `configure({ extensions: { remove: [Subagent] } })` | 子会话 `ToolSelection.disabled` 包含两个 subagent 工具 |
| `submit({ whenBusy })` + `Submission.wait()` | `Kernel::submit(SubmitRequest) -> SubmissionAnswer`（不持久化准入，因此合并为一步） |
| abort 沿所有权向下级联，background 为边界 | 子会话 run 的取消令牌派生自父 Turn 令牌；后台子会话不派生 |
| owner `completing` 直到名下工作排空 | 前台工具 await 子会话 run 直至其结束 |
| `api.details({ conversationId })` + 会话 watch | `ToolResultDetails::Subagent { session_id }` + ACP 按会话投影订阅 |
| `scanConversations({ ownerTaskId })` | 扫描 header `ownership` 指向 owner 的会话 |
| Anchor 任务（background，立即完成） | `SessionOwnership::Background { name }` |
| Reporter 任务 | tokio 任务 `Reporter`，由 owner 的 `SessionBackgroundWork` 跟踪 |
| `pi.usage` 按会话记账，工具结果不报子会话用量 | 用量按会话记录；`ToolResult` 无 usage 字段 |
| 内置扩展可被同名第三方扩展替换 | extension 已提供同名工具时 kernel 不注册自己的 subagent 工具 |

不移植 pi-durable 的持久任务调度器。clawcode 的 run 不支持崩溃续跑，所以 `replay: "safe"`、`requestId` 幂等和持久化 reporter 在本期不需要，见「本期不做」。

## 模块划分

```
protocol   SessionOwnership、SessionSummary.ownership、ToolResultDetails::Subagent
store      V4Header / OwnedSessionCreateOptions / SessionMetadata 记录 ownership；exists 查询 header
kernel/runtime/
  settings.rs      SettingChange：model / thinking 设置 entry 的唯一读写格式
  ownership.rs     OwnedSessionSpawn、AgentSnapshot、级联 close / delete / resume
  submit.rs        Kernel::submit、SubmitRequest、WhenBusy、SubmissionAnswer
  mcp.rs           McpLease::{Owned, Shared}；SessionMcpHost 按 session_id 路由
  subagent/        SubagentTool（前台）、BackgroundSubagentTool（后台）、Reporter、SubagentError
acp        ProjectionSinks 实现 SessionEventSinks；session/list meta 增加 ownership
web        SubagentPanel、侧栏嵌套、后台 subagent 面板、回报消息渲染
```

代码风格参考 docparse：用借用上下文的结构体承载流程（如 `OwnedSessionSpawn<'a>`），逻辑组织为结构体方法；类型转换用 `From` / `TryFrom`；超过 3 个字段的结构体使用 `typed-builder`。

## 所有权数据模型

```rust
/// Owner edge of a task-owned Session, separate from fork history (pi `ConversationRecord.owner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionOwnership {
    /// Foreground child owned by one tool call; parent abort cascades into it.
    Tool { session_id: SessionId, tool_call_id: ToolCallId },
    /// Named persistent child behind a background boundary; parent abort stops here.
    Background { session_id: SessionId, name: String },
}
```

- 类型定义在 `protocol`，`store`、`kernel`、`acp` 共用。
- `V4Header` 增加可选字段 `ownership`，序列化为 `"ownership": { "kind": ..., ... }`，缺省时不写。header 未设置 `deny_unknown_fields`，旧文件照常读取。
- `SessionMetadata`、`protocol::SessionSummary` 增加 `ownership: Option<SessionOwnership>`；普通 `SessionCreateOptions` 不变，委派创建使用 `OwnedSessionCreateOptions { create, ownership }` 与 `StoreFactory::create_owned`。
- 子会话的 cwd 与 owner 相同。`list(Some(cwd))` 扫描同一 cwd 目录即可找到全部子会话。
- 后台名称注册表不单独持久化，由「同 cwd 下 `ownership` 为 `Background { session_id: owner }` 的会话」派生。fork 出来的会话 id 不同，不会继承注册表，等价于 pi 文档的 `fork: "initial"`。
- live 会话的 owner 关系从 `Kernel.sessions` 中各 `Session.ownership` 扫描得到，不维护额外索引。代码中用 `ponytail:` 注释标注 O(n) 扫描的上限与升级路径（owner → children 索引）。
- fork 父会话后，fork 中历史工具结果的 details 仍指向原会话名下的子会话，UI 打开的是原子会话。这是预期行为。

## Kernel 原语

### 设置 entry 格式：`SettingChange`

ModelChange、ThinkingLevelChange 的 JSON 形状目前在 `extension/host.rs` 写、在 `session.rs` 的 `RestoredSessionSettings::load` 读。子会话复制配置会成为第三个写入点，因此先收敛为一个类型：

```rust
/// One persisted model or thinking selection; the only owner of their entry payload shapes.
pub(in crate::runtime) enum SettingChange {
    Model { provider: String, model_id: String },
    ThinkingLevel(ThinkingLevel),
}

impl From<&SettingChange> for (EntryKind, serde_json::Value) { … }
impl TryFrom<&SessionEntry> for SettingChange { … }
```

`KernelExtensionHost::set_model` / `set_thinking_level` 与 `RestoredSessionSettings::load` 改为使用它。工具选择继续由 `ToolSelectionUpdate` 负责。

### 创建子会话：`OwnedSessionSpawn`

```rust
/// Parent agent choices copied into a new owned Session (pi `pi.agent` copy-on-create).
pub(in crate::runtime) struct AgentSnapshot {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    tools: ToolSelection,
}

impl TryFrom<&Session> for AgentSnapshot { /* reads model, thinking, and tool selection */ }

impl AgentSnapshot {
    /// Adds tool names the child must not see, mirroring pi's `extensions: { remove: [Subagent] }`.
    fn without_tools(self, names: &[&str]) -> Self;
    /// Applies the copied choices to a built but not yet started child through the existing setting writers.
    fn apply(&self, child: &Session, ids: &dyn IdGenerator) -> Result<(), KernelError>;
}

/// Borrows the owner runtime and Kernel for one owned-Session creation.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct OwnedSessionSpawn<'a> {
    kernel: &'a Kernel,
    owner: &'a Arc<Session>,
    ownership: SessionOwnership,
    title: SessionTitle,
}
```

`OwnedSessionSpawn::create()` 返回子会话 `SessionId`，流程：

1. `AgentSnapshot::try_from(owner)`，再 `without_tools(&SUBAGENT_TOOL_NAMES)`。
2. 生成 `SessionId`，`store_factory.create_owned(OwnedSessionCreateOptions { create: SessionCreateOptions { cwd: owner.cwd, parent_session_id: None, .. }, ownership })`。
3. `build_session(store, cwd, SessionMcpSource::Share(&owner.mcp))`。
4. `snapshot.apply(&child)`：替换 `SessionModelState` 的 model 与 thinking，并通过 `SettingChange` 写入 entry；通过新增的 `ToolSelectionUpdate::adopt(ToolSelection)` 写入工具选择。此时不触发 `model_select` 事件。
5. 注册、`start_extensions(SessionStartReason::New)`、checkpoint。失败时沿用 `create_session` 现有的回滚路径。
6. `rename_session(child, title)`。前台标题为 `subagent: {task 前 60 个字符}`，后台标题为名称。

现有 `create_session` 改为共用 `build_session(store, cwd, SessionMcpSource)`，普通会话传 `SessionMcpSource::Connect`。

子会话继承的 extension 实例与普通会话相同（clawcode 的 extension 是进程级静态注册），会收到 `session_start`。

### 共享 MCP

- `SessionMcpState` 增加 `lease: McpLease { Owned, Shared }`，并保存 `Arc<SessionMcpHost>` 以便子会话挂载路由。
  - `Owned` 关闭：关闭 McpSession，并等待投影任务结束（现有行为）。
  - `Shared` 关闭：只中止并等待自己的投影任务，不触碰共享连接。
- 每个子会话有自己的投影任务和 elicitation 表，订阅同一个 McpSession，MCP 工具目录变化同步到子会话的工具集。
- `SessionMcpHost` 的 `runtime: OnceLock<Weak<Session>>` 改为 `routes: RwLock<HashMap<SessionId, Weak<Session>>>`：
  - `attach(&Arc<Session>)` 按会话 id 插入路由；`detach(&SessionId)` 在会话关闭时移除。
  - Elicitation、Sampling 按 `request.context.session_id` 路由；找不到路由时返回 `McpError::Host`。
  - Roots 继续使用 cwd（子会话与 owner cwd 相同）。
- MCP 管理操作（`mcp_reconnect`、`mcp_continue_authorization`）在 `Shared` 会话上返回 `KernelError::SharedMcpManagement`，提示到 owner 会话操作。只读的 `mcp_status`、`subscribe_mcp` 照常可用。

### `Kernel::submit`

```rust
/// Placement of input that arrives while a Session is busy (pi `whenBusy`).
pub enum WhenBusy { Steer, FollowUp }
impl From<WhenBusy> for QueueKind { … }

/// Kernel-initiated input for one Session (pi `submit`).
#[derive(typed_builder::TypedBuilder)]
pub struct SubmitRequest {
    session_id: SessionId,
    /// Always structured blocks: text input would run `!` user bash, direct Slash Commands, and Skill expansion.
    content: Vec<ContentBlock>,
    when_busy: WhenBusy,
    /// Owner Turn token the run token derives from (pi abort cascade); `None` is a background boundary.
    #[builder(default)]
    cancellation: Option<CancellationToken>,
    /// Trace inherited from the calling tool so child logs correlate with the parent call.
    #[builder(default)]
    trace_id: Option<TraceId>,
}

impl Kernel {
    /// Runs the input when idle or queues it when busy, then resolves with the run that answered it.
    pub async fn submit(&self, request: SubmitRequest) -> Result<SubmissionAnswer, KernelError>;
}
```

- 输入类型固定为 `RunInput::Blocks`。`RunInput::Text` 会经过 `!`/`!!` 用户 bash、直接 Slash Command 与 Skill/Template 展开，模型给出的任务不能走这些路径。Blocks 也不经过 extension input hook，所以不需要新的 `InputSource`；内部调用使用 `InputSource::Extension`。
- 会话空闲：以 `event_sinks.sink(session)` 为 sink 调用 `run_with_source`。`run_with_source` 增加内部参数 `cancellation_parent: Option<CancellationToken>`，`install_cancellation` 据此用 `parent.child_token()` 派生 run 令牌，否则新建。父令牌在子 run 开始前已取消时，派生令牌一开始就是取消状态，不存在「先取消、后换新令牌」的竞态。`trace_id` 缺省时生成新的 Trace。
- 会话忙：内部 `queue_message_with_receipt` 在发布队列项之前附加一次性 `SubmissionReceipt`。如果 run 恰好结束，回到空闲路径；回执注册与入队没有可丢失结算的时间窗口。
  1. 输入被实际消费时，回执转入当前 run 的 `SessionExecution.submissions`；该 run 结算时用同一份答案完成名下全部回执。
  2. 即使另一轮 run 先占用执行 gate，等待者仍收到消费自己输入的那一轮答案，不读取全局“最新结果”。
  3. 删除/清空尚未消费的输入时完成 Cancelled 回执；父取消还会取消当前 run、清空队列并等待其退出。
  4. 队列中未被消费而滞留的消息，在空闲时取回执行；异常退出释放未结算回执，不让调用者永久等待。
- `RunSettlement` 根据最终 `AgentOutcome` 和本轮消息，统一生成 `SubmissionAnswer { message_id, text, outcome }`。直接提交与排队提交使用同一结算，重试等待期间取消不会误用先前 Turn 的 Failed 状态。
- 答案来自本轮已有消息，不再使用 `last_settlement` 或遍历、克隆完整持久化历史。回执只在内存中，不引入 Pi 的持久 submission 表或崩溃续跑能力。
- 调用链 run → 工具批次 → subagent 工具 → `submit` → run 是异步递归，且 run future 较大（`clippy::large_futures`）。子会话 run future 必须 `Box::pin`。

### 级联

| 操作 | 行为 |
|---|---|
| abort | 前台子会话 run 的令牌派生自父 Turn 令牌，父取消即子取消；前台工具继续 await 子 run 直至结束（对应 pi completing）。子会话内的工具以同样方式继续向下级联。排队路径在等待中观察父令牌（见 `submit`）。后台子会话的 run 不派生父令牌，即 background 边界。 |
| close | `close_session(owner)`：① 触发 `SessionBackgroundWork.shutdown`；② 取消 owner 的 run、获取执行 gate、进入 closing（此后 owner 不会再运行工具，也就不会再创建子会话）；③ 递归关闭 owner 名下所有 live 子会话（两种 ownership），子会话 run 被取消，处于 deliver 阶段的 reporter 得到 `Cancelled`；④ 等待全部 reporter 结束（向 owner 提交回报会因 closing 失败）；⑤ 执行现有的 shutdown hook、MCP 关闭、store 同步与移除。 |
| delete | `delete_session(owner)`：先关闭（同上），再递归删除同 cwd 下 `ownership` 指向它的持久化会话，最后删除 owner。 |
| resume | `resume_session(child)`：读取 header `ownership`；owner 不在运行时先 `resume_session(owner)`；多个 child 遇到同一个正在启动的 owner 时等待现有构建预留完成，再以 `Share` 方式构建子会话。恢复前拒绝 ownership 环，避免等待自身。owner 的持久化文件不存在时退化为 `Connect` 并输出 warn 日志。 |

前台子会话执行期间，用户在子会话里追问会排进子会话当前 run 的队列，父工具会等这部分处理完才返回（对应 pi §12 「Owned work holds its owner」）。

### 事件 sink

```rust
/// Routes Kernel-initiated runs (child runs, reporter deliveries) to the host's per-Session projection.
pub trait SessionEventSinks: Send + Sync {
    /// Returns the live event destination for one Session.
    fn sink(&self, session_id: &SessionId) -> Arc<dyn EventSink>;
}
```

- `Kernel` 增加 `event_sinks: Arc<OnceLock<Arc<dyn SessionEventSinks>>>` 与 `install_event_sinks()`，重复安装返回 `KernelError::EventSinksInstalled`。
- 未安装时使用丢弃事件的 sink（例如不经 ACP 的测试）；测试也可安装 channel sink 断言事件。

## Subagent 模块

两个工具与 `ToolSearchTool` 一样在 `build_session` 中注册，持有 `Weak<Session>` 与一个 `Kernel` clone。`Kernel` clone 经 `sessions` 映射形成引用环，与 `KernelExtensionHost` 现状相同，由 `close_session` 从映射移除会话时断开。常量 `SUBAGENT_TOOL_NAMES = ["subagent", "background_subagent"]`，两个工具均为 `ToolExposure::Direct`。

`SessionToolState::register` 以 upsert 写入 extensions 分区，会覆盖同名 extension 工具。注册前检查 extensions 分区：若某个 extension 已提供同名工具，kernel 跳过自己的工具并输出 info 日志，对齐 pi 内置扩展可被替换的规则。

### 前台 `subagent { task }`

- 描述沿用 pi：「Delegate a self-contained task to a subagent with the same tools and get its answer back. Give it everything it needs to know; it does not see this conversation.」
- 执行：
  1. `OwnedSessionSpawn` 创建子会话，ownership 为 `Tool { session_id, tool_call_id }`，标题为 `subagent: {task 前 60 个字符}`。
  2. `context.publish` 一次部分结果，details 为 `ToolResultDetails::Subagent { session_id }`。
  3. 提交并等待：

     ```rust
     let request = SubmitRequest::builder()
         .session_id(child.clone())
         .content(vec![ContentBlock::Text { text: task }])
         .when_busy(WhenBusy::FollowUp)
         .cancellation(Some(context.cancellation.clone()))
         .trace_id(Some(context.trace_id.clone()))
         .build();
     // The child run token derives from this Turn's token, so awaiting it is pi's completing hold.
     let answer = Box::pin(self.kernel.submit(request)).await?;
     ```

  4. `SubmissionAnswer` 转换为工具结果：
     - `Completed`：最后一条 assistant 文本，为空时返回 `(no output)`。
     - `Cancelled` 且父 Turn 令牌已取消：`ToolError::Cancelled`。
     - `Cancelled` 但父 Turn 令牌未取消（用户在子会话中直接停止）：`is_error` 结果 `Subagent was stopped before answering.`
     - `Failed(msg)`：`is_error` 结果 `Subagent failed: {msg}`。
     - 最终结果 details 同样携带子会话 id。
- 前台工具使用现有 `ToolOutputPolicy::Bounded`，限制投给父会话的文本；完整答案保留在子会话。
- 工具返回后子会话保持 live，生命周期跟随 owner。

### 后台 `background_subagent { action, name?, message?, followUp? }`

后台参数由 `BackgroundAction` 的 `TryFrom<&ToolCall>` 一次校验，执行阶段只对已校验的枚举做 match 分发。

描述沿用 pi example 22：「Manage persistent subagents that work in the background. Actions: spawn (name, message), send (name, message; followUp: true queues it after the current answer instead of steering), stop (name: aborts its current work), status (name, or all subagents without one). Answers are reported back to you when they arrive.」

| action | 行为 |
|---|---|
| `spawn` | 名称已存在返回 `{name} already exists; use send.`；否则以 `Background { session_id, name }` 创建子会话（标题为名称），启动 Reporter，返回 `Started {name}.`，details 携带子会话 id。 |
| `send` | 子会话未运行时先 resume；启动 Reporter，`followUp = true` 用 `WhenBusy::FollowUp`，否则 `Steer`；返回 `Sent to {name}.` |
| `stop` | 子会话在运行时 `cancel_session` 并清空队列；未运行时视为已空闲，不报错。子会话仍可继续使用；返回 `Stopped {name}.` |
| `status` | 列出 owner 的后台子会话：`{name}: working` / `{name}: idle`。working 指子会话在运行且有 active run（对应 pi `pi.live.run`），未运行的子会话为 idle。无子会话时返回 `No subagents.` |

- 名称校验：`[A-Za-z0-9_-]{1,64}`。缺参数时返回 `{action} needs a name.` / `{action} needs a message.`；名称不存在返回 `No subagent named {name}.`。
- spawn 的第一条消息前加一行：`You are the subagent "{name}". Answer the main agent's requests.`。这是对 pi `instructions` 的偏离：clawcode 没有按会话的 instructions。
- 工具调用立即返回，不等待子会话回答。

### Reporter

```rust
/// Delivers one message to a background subagent and reports its answer to the owner (pi `app.subagent-reporter`).
#[derive(typed_builder::TypedBuilder)]
struct Reporter {
    kernel: Kernel,
    owner: SessionId,
    child: SessionId,
    name: String,
    message: Vec<ContentBlock>,
    when_busy: WhenBusy,
    background: Arc<SessionBackgroundWork>,
}

/// Background work owned by one Session: reporters, their shutdown signal, and reported answers.
struct SessionBackgroundWork {
    shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
    reported: Mutex<HashMap<SessionId, HashSet<MessageId>>>,
}
```

- deliver：`submit(child, message, when_busy)`，`cancellation` 为 `None`（background 边界），得到 `SubmissionAnswer`。
- 决定回报：
  - `Cancelled`：不回报（被 stop、撤回或因关闭取消）。
  - `Failed(msg)`：回报 `[subagent {name} failed: {msg}]`。
  - `Completed`：`message_id` 已在 `reported` 中则不回报（多条消息可能对应同一个回答）；否则记录后回报 `[subagent {name} answered, no reply needed] {text}`。
- report：`shutdown` 已触发时放弃；否则 `submit(owner, report, WhenBusy::FollowUp)`，`cancellation` 为 `None`。父会话忙则排队，空闲则由 reporter 驱动父会话的一个 run，事件经 `SessionEventSinks` 进入父会话投影。
- 任一阶段的 `submit` 返回 `KernelError`（例如子会话或 owner 已关闭）时，reporter 输出日志后结束，不重试。
- `SessionBackgroundWork` 挂在 owner `Session` 上，关闭顺序见「级联」。
- `ponytail:` reporter 与 `reported` 只在内存中，进程崩溃会丢失尚未回报的答案；此时 `status` 显示 idle，模型可再次 `send`。升级路径：在 owner 会话中以 lane record 持久化 reporter，owner resume 时恢复。

### 用量

子会话用量只记在子会话自身。`ToolResult` 没有 usage 字段，天然不会重复计算。

## ACP

- `server.rs` 构造 `ProjectionRegistry` 后，用 `ProjectionSinks(Weak<ProjectionRegistry>)` 实现 `SessionEventSinks` 并 `kernel.install_event_sinks(..)`。Kernel 发起的 run 与用户 prompt 一样被投影、记录、广播，WebSocket 断线恢复照常工作。
- 工具 details 无需新增映射：`ToolExecutionUpdate` / `ToolExecutionEnd` 已把完整 `ToolResult` 写入 `rawOutput`，WebUI 读取 `rawOutput.details = { "type": "subagent", "session_id": "…" }`。持久化回放的 `AgentMessage` 则在 `rawOutput.content.details` 携带 details；前端解码器统一保存 `subagentSessionId`，避免完成/刷新后丢失入口。
- `session/list` 的 `_meta.clawcode` 增加 `ownership`（`SessionOwnership` 的 JSON）。
- 子会话 resume 由 Kernel 先恢复 owner，ACP 无需感知。Shared 会话的 MCP 管理请求由 Kernel 拒绝，ACP 原样返回错误。

## WebUI

后台状态查询保留“持久化但未启动的会话为 idle”的行为。冷路径使用 `StoreFactory::exists`；JSONL 实现遍历文件名并仅解析匹配文件的 header，不为每次轮询读取、解析全部 transcript 正文。

- `ToolCallCard` 检测到 `rawOutput.details.type === "subagent"` 时渲染 `SubagentPanel`：工具运行中自动展开并 `controller.attachSession(childId)`，结束后自动折叠（与运行中 bash 自动展开一致）；复用 session resume、订阅与 updateRouter，用紧凑模式的 `Conversation` 渲染子会话实时消息、工具调用与 MCP elicitation；提供「打开会话」切换到子会话主视图。运行中自动展开保证子会话的 elicitation 对用户可见，否则前台工具会一直等待。
- `SessionSidebar` 按 `ownership` 把子会话折叠在 owner 下，显示子会话标题。
- 父会话视图增加后台 subagent 面板：列出 `ownership.kind === "background"` 且指向当前会话的子会话，状态复用 runtime 轮询，提供「打开」「停止」（对子会话发 ACP cancel）。已知限制：后台子会话的 elicitation 需要用户打开该子会话才能看到。
- 父会话中匹配 `^\[subagent (\S+) ([^\]]*)\] ?(.*)$` 的用户消息渲染为 subagent 回报样式（显示名称与正文），与 pi example 22 一致。
- 子会话的 MCP 面板只读，提示连接由 owner 会话管理。

## 错误处理

- 模型可自行纠正的用法问题（缺参数、名称不存在、名称重复、名称非法）以普通文本回复返回，不标错误，与 pi example 22 一致。
- 基础设施失败使用 `SubagentError`（thiserror，`#[from] KernelError`，携带工具名），经 `From<SubagentError> for ToolError` 转为 `ToolError::Execution { tool, message }`。
- `KernelError` 新增：`OwnerUnavailable(SessionId)`、`SharedMcpManagement(SessionId)`、`EventSinksInstalled`。
- 子会话自身失败或被直接停止成为父工具的错误结果，不使父会话 run 失败；只有父 Turn 被取消时前台工具才返回 `ToolError::Cancelled`。

## 日志

遵循 CLAUDE.md 第 13、14 条：全路径宏、正文格式参数、不记录任务正文、回报正文或 prompt。记录点：

- 子会话创建开始 / 完成 / 失败（owner、child、ownership 类型）。
- subagent 工具因 extension 同名工具而跳过注册。
- 前台 subagent 开始 / 完成 / 取消 / 被直接停止 / 失败。
- 级联 close / delete 中每个子会话的开始与结果。
- resume 先恢复 owner；owner 缺失退化为 `Connect` 的 warn。
- `submit` 的放置结果：直接 run、排队、取回滞留消息。
- MCP Shared 路由挂载 / 卸载，路由缺失错误。
- Reporter 阶段：deliver 开始、子会话回答结果、回报已提交或跳过（含原因）、因关闭放弃。

## 测试

按 TDD 实施。

- `crates/store/tests`：带 `ownership` 的 header 读写；无该字段的旧 header 正常解析；`list` 返回 ownership。
- `crates/kernel/tests/subagent.rs`，使用按最后一条消息路由的 `ScriptedModel`：
  1. 前台：工具结果等于子会话回答；子会话 header 为 `Tool` ownership；复制了父会话 model / thinking；禁用两个 subagent 工具；子会话标题为 `subagent: …`。
  2. 输入安全：以 `!echo x`、`/compact` 开头的 task 作为普通用户消息进入子会话，不执行用户 bash 或 Slash Command。
  3. abort 级联：子会话模型挂起时取消父会话，子 run 被取消，父工具返回 Cancelled，父会话正常结束；父令牌在子 run 开始前已取消时子 run 同样被取消。
  4. 直接停止子会话：父工具得到错误结果 `Subagent was stopped before answering.`，父会话继续运行。
  5. close / delete 级联：关闭父会话关闭 live 子会话；删除父会话删除子会话 store；后台子会话挂起、reporter 处于 deliver 阶段时关闭父会话不会卡住，且不产生回报。
  6. resume：恢复子会话先恢复 owner；计数 MCP factory 断言只创建一次。
  7. MCP 路由：子会话中 MCP 工具触发的 elicitation 路由到子会话（复用 `mcp_host.rs` 测试替身）。
  8. `submit`：空闲直接 run；忙时排队后得到回答；构造滞留消息并验证被取回执行；等待期间会话再次忙碌时继续等待。
  9. 注册优先级：extension 已注册 `subagent` 时 kernel 不覆盖。
  10. `SettingChange`：写入的 entry 能被 `RestoredSessionSettings::load` 恢复；extension host 的 set_model / set_thinking_level 行为不变。
  11. 后台：spawn 后父会话收到 `[subagent x answered` 并自动运行一轮；send 的 steer / followUp；stop 后不回报；对未运行子会话 stop / status 返回 idle；status 输出；重复名称提示；同一回答只回报一次。
- `crates/acp`：安装 sink 后子会话 run 的事件记录在子会话 id 下；`session/list` meta 携带 `ownership`。
- WebUI：按 CLAUDE.md 第 9 条，用生产 `app` 后端与已配置 provider 做端到端验收：subagent 工具卡片运行中自动展开、「打开会话」、侧栏嵌套、后台 spawn 回报渲染与「停止」。

## 本期不做

- `abort({ background: true })` 式全量中止：需要时加在 `Kernel::abort_session`。
- Reporter 持久化与崩溃恢复：需要时用 lane record 实现。
- 父会话加子会话的用量合计：需要时对 owner 树内各会话记录求和。
- 按会话 `instructions`：需要时增加 Custom entry 并接入 prompt 构建。
- 命名 agent 定义（旧版 subagent 扩展的 `agents/*.md`）：pi-durable 无此机制。
- `SubagentStart` / `SubagentStop` hook：clawcode 尚无 hook 执行器。

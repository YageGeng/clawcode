# Codex 式多 agent 协作设计

## 目标

把 Codex `multi_agent_v2` 的协作语义搬到 clawcode：父 agent 非阻塞地派生具名子 agent，agent 之间经 mailbox 互发消息，子 agent 完成后把结果投回父 agent 的 mailbox，父 agent 用 `wait_agent` 显式等待。实现复用 clawcode 现有的 Session 所有权、上下文 note 与 Extension 消息，不移植 Codex 的 agent graph 存储、消息板或多层嵌套。

本设计替换 [原 subagent 设计](./2026-10-28-subagent-design.md) 中的前台 `subagent` 与后台 `background_subagent` 工具，保留其所有权、共享 MCP、级联关闭/删除/恢复与事件 sink 原语。不考虑任何历史数据兼容。

成功标准：

- 模型通过 `spawn_agent` 派生子 agent 并立即拿回规范名称 `/root/<task_name>`；子 agent 的每次回答自动作为 `result` 邮件投入 `/root` 的 mailbox。
- agent 之间的全部通信只走 mailbox，不使用用户的 steer / follow-up 队列：`send_message` 与完成通知只排队、不唤醒；`followup_task` 在对方空闲时唤醒、忙时进入 mailbox 并保证对方再跑一个 Turn。
- `wait_agent` 在收到邮件、用户插话或超时时返回。
- 子 agent 之间、子 agent 与 `/root` 之间可以互发消息；子 agent 不能派生、跟进或中断其他 agent，因此不存在互相唤醒的环。
- 不唤醒的邮件不会让一个本已结束的 run 多跑一个 Turn，也不会因 run 失败、取消或用户清空队列而丢失。
- 父 agent 的上下文用 `<subagents>` 名单 note 声明当前子 agent，名单不变时不重复追加。
- `fork_turns`、`model`、`reasoning_effort` 控制子 agent 的初始历史、模型与思考强度。
- WebUI 显示 agent 间消息卡片、带昵称的子 agent 标签栏，`spawn_agent` 卡片在子 agent 运行时展开其实时过程。

## 与 Codex v2 的对应关系

| Codex v2 | clawcode | 说明 |
|---|---|---|
| `AgentPath`（`protocol/src/agent_path.rs`） | `protocol::AgentPath` | 校验规则照搬；只支持 `/root` 与 `/root/<name>` 两层 |
| 昵称池 `assets/agent/agent_names.txt` + `reserve_agent_nickname` | `protocol::AgentNickname` + 同一份名字表 | 按 owner 分配，用尽后加序数后缀 |
| `AgentRegistry` | `Session.roster`（`AgentRoster`） | 首次使用时从 header 加载，spawn 时更新 |
| agent-graph-store（SQLite 边） | `SessionOwnership` header | 父子边持久化在子会话 header |
| `InterAgentCommunication` + `RolloutItem` | `AgentMail` → `MessageContent::Extension { custom_type: "agent_message" }` | 不新增 `MessageContent` 变体 |
| 会话输入队列中的 mailbox | `PendingQueue.mail` 内存通道 | 忙时暂存，空闲时直接写入记录 |
| `trigger_turn` | `MailKind::NewTask` | 只有 `new_task` 邮件会唤醒或延长 run |
| `notify_parent_of_terminal_turn` | `RunSettlement` 结算后的完成通知任务 | 取代 `Reporter`；`trigger_turn = false` |
| world state `<subagents>` | `SubagentRosterNote` | 与 `CommandSessionsNote` 共用 `StatedNote` trait |
| `fork_turns` + `keep_forked_rollout_item` | `ForkedHistory` | 过滤规则照搬 |
| root / subagent usage hint（`prompts/src/model_messages/multi_agent.rs`） | `list_agents` 的 System Prompt guidelines + 子 agent 的 `subagent_role` note | 见「角色说明」；子 agent 的 hint 照 Codex 追加在继承历史之后 |
| `max_concurrent_threads_per_session = 4`（含 root） | `subagent.max_agents = 3`（运行中的子 agent） | 偏离：Codex 按存活数计且 v2 无关闭工具；照搬会让第 4 次 spawn 永久失败 |
| 任意深度嵌套 | 只有一层 | 偏离：子 agent 没有 `spawn_agent` |
| 子 agent 拥有全部工具 | 子 agent 只有 `send_message` / `list_agents` / `wait_agent` | 偏离：消除唤醒环，无需跳数预算 |
| `wait_agent` 仅被 steer 的用户输入打断 | 用户 steer 与 follow-up 入队都打断 | 偏离：否则用户 follow-up 可能要等满 1 小时 |

## 名称与寻址

### `AgentPath`

```rust
/// Canonical agent address: `/root` or `/root/<agent_name>` (Codex AgentPath, one level deep).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentPath(String);
```

- `agent_name` 规则照搬 Codex `validate_agent_name`：非空；只含小写字母、数字与 `_`；`root`、`.`、`..` 保留；不含 `/`。错误文本照搬 Codex，例如 `agent_name must use only lowercase letters, digits, and underscores`、``agent_name `root` is reserved``。规则由 `protocol::AgentName` 在构造与反序列化时执行，`AgentPath::child(&AgentName)` 因此不会失败。
- 绝对路径只接受 `/root` 与 `/root/<agent_name>`；更深的路径返回 ``agent path `/root/a/b` is deeper than one level``。
- 解析目标照搬 Codex `AgentPath::resolve`：`/root` 指 owner；以 `/` 开头的引用按绝对路径校验；其余单段引用 `x` 解析为 `/root/x`。裸 `root` 按保留名拒绝。子 agent 调用时同样以 `/root` 为基准，因此子 agent 可用 `y` 或 `/root/y` 指兄弟。
- `AgentPath::root()`、`join(name)`、`name()`、`is_root()` 与 Codex 同义。

### 昵称

- `crates/protocol/assets/agent_names.txt` 复制 Codex 的 101 个名字；`THIRD_PARTY_NOTICES.md` 增加 `openai/codex`（Apache-2.0）条目。
- `AgentNickname::allocate(used: &HashSet<String>, index: usize) -> String` 在 `protocol` 中保持确定性：以「已用次数」决定轮次，从本轮未用名字中取第 `index % 可用数` 个；全部用尽时进入下一轮，序数后缀格式与 Codex `format_agent_nickname` 相同（`Euclid the 2nd`、`the 11th`、`the 21st`）。
- 随机下标由 kernel 以 `std::collections::hash_map::RandomState::new().hash_one(…)` 取得，不引入新依赖。
- 昵称只给用户看；模型只看到 `task_name`（Codex 默认 `hide_spawn_agent_metadata = true`）。

## 数据模型

```rust
/// Owner edge of a named child Session, separate from fork history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwnership {
    /// Owner Session (`/root`).
    pub session_id: SessionId,
    /// The child's `agent_name`, validated when read; its path is `/root/{name}`.
    pub name: AgentName,
    /// User-facing nickname allocated at spawn.
    pub nickname: String,
}
```

- 删除 `SessionOwnership::Tool` 与 `Background` 两个变体，改为上述结构体；JSON 不再带 `kind`。`V4Header.ownership`、`SessionMetadata`、`SessionSummary` 跟随类型变化。
- 所有依赖 ownership 的级联（close、delete、owner 优先的 resume、共享 MCP）只看 `session_id`，逻辑不变。
- `Session` 增加名单注册表 `roster: AgentRoster`，与只管后台任务和关停的 `SessionBackgroundWork` 分开：

```rust
/// An owner's named children and the lock that admits new ones.
struct AgentRoster {
    /// Serializes admission so names and running slots are checked atomically.
    spawning: tokio::sync::Mutex<()>,
    /// Cached children; `None` until first loaded from child headers.
    children: std::sync::Mutex<Option<BTreeMap<AgentName, ChildEntry>>>,
}

/// One child known to its owner.
struct ChildEntry {
    session_id: SessionId,
    nickname: String,
    /// Last settled state; a live active Run reports `running` instead.
    status: ChildStatus,
}

/// Codex agent status subset that survives between Runs; its serde form
/// (`snake_case`, externally tagged) is Codex `agent_status_output_schema`.
enum ChildStatus {
    /// Spawned; its first Run has not settled.
    PendingInit,
    /// Last Run completed with its truncated final answer, if known.
    Completed(Option<String>),
    /// Last Run failed with its error message.
    Errored(String),
    /// Last Run was cancelled before answering.
    Interrupted,
}
```

  - 访问接口为 owner 会话上的同步闭包 `Session::with_children(kernel, |children| …)`：header 扫描本身是同步的，同步接口避免把非 `Send` 的 guard 跨 `await` 持有。
  - 首次使用时扫描同 cwd 的 header 加载，状态为 `Completed(None)`；`spawn_agent` 插入 `PendingInit`；子 run 结算时更新为 `Completed`、`Errored` 或 `Interrupted`；子会话被删除时移除。
  - 名称唯一性、昵称分配、并发计数、`list_agents`、`<subagents>` note 都读这份注册表，模型请求前不扫盘；每个 owner 会话的生命周期内最多扫描一次。
  - 子 agent 调用 `send_message`、`list_agents` 等工具时，经 `kernel.session(&ownership.session_id)` 读取 **owner 会话**的注册表；子会话自己的 `roster` 始终为空。owner 在子会话运行期间一定活着。

## 存储

### 文件布局

主 agent 与子 agent 都是普通 v4 JSONL 会话，位于同一个 cwd 目录：

```
<data>/sessions/--home-isbest-proj--/
  1762750000000_<rootId>.jsonl     ← 主 agent（/root）
  1762750001234_<childId>.jsonl    ← 子 agent（/root/x）
```

### 主 agent

```jsonc
{"kind":"header","version":4,"id":"<rootId>","createdAt":…,"cwd":"/home/isbest/proj"}
// 新增的变更类型：
//  Message entry：assistant 的 spawn_agent 等工具调用；工具结果 details 为 {"type":"subagent","session_id":"<childId>"}
//  Message entry：Extension "agent_message"（来自子 agent 的 result / message）
//  Custom entry："subagent_roster" note，details 保存路径列表，重建上下文时还原为隐藏消息
```

主 agent 的 header 与日志不记录子 agent 列表；agent 邮件不写 `QueueEnqueued` / `QueueCancelled` 记录。

### 子 agent

```jsonc
{"kind":"header","version":4,"id":"<childId>","createdAt":…,"cwd":"/home/isbest/proj",
 "ownership":{"sessionId":"<rootId>","name":"x","nickname":"Euclid"}}
//  ModelChange / ThinkingLevelChange / ActiveToolsChange：继承的设置与 model / reasoning_effort 覆盖
//  Custom：子会话自己的 plan mode note
//  Name fact：标题为昵称
//  Message × N：fork_turns = all 时按 Codex 规则过滤后复制的父历史（新 MessageId / EntryId，usage 清零，摘要转为 User 文本）
//  Message：Extension "agent_message" new_task，即首个 run 的输入
//  其后为普通记录与收到的 agent_message
```

### 拓扑

- 唯一来源是子会话 header 中的 owner 边：随子会话文件以 `create_new` + fsync 原子写入，之后不再修改。
- 拓扑由 `StoreFactory::headers(Some(cwd))` 筛选 `ownership.session_id == owner` 派生；`Session.roster` 是它的内存缓存。
- 不在主 agent 侧另存一份：header 只在创建时写一次，无法列出之后派生的子 agent；在主 agent 日志里追加派生记录会形成第二个来源，两次写入之间崩溃会产生孤儿或悬空记录，删除子会话还需要墓碑。

### 操作的存储语义

| 操作 | 存储变化 |
|---|---|
| spawn | 先创建子会话文件（带 owner 边），再写入主 agent 的工具结果；中途崩溃时子 agent 仍出现在名单中，状态为 `{"completed": null}` |
| 删除子会话 | 删除子会话文件，并从 owner 的注册表移除；下一次模型请求前名单 note 更新 |
| 删除主 agent | 沿用现有级联：先删除全部子会话文件，再删除主 agent |
| fork 主 agent | 子会话的边仍指向原主 agent，fork 没有子 agent；fork 带过去的历史若含旧名单 note，下一次模型请求前追加空名单 note |
| fork 子 agent | `fork()` 经 `create` 写不带 ownership 的 header，结果是独立会话，不属于任何名单 |
| resume 子 agent | 沿用现有逻辑：先恢复 owner，再以共享 MCP 构建子会话 |
| 只在内存中 | `mail` 通道、名单注册表、`wait_agent` 信号、完成通知与唤醒任务 |

## 工具

工具注册方式与原设计相同：在 `build_session` 中注册，持有 `Weak<Session>` 与一个 `Kernel` clone；extension 已提供同名工具时 kernel 不注册自己的版本。全部为 `ToolExposure::Direct`，属于 kernel 自有工具，因此直接通过 `PlanModeGate`（`plan/gate.rs:56`）。

常量 `ROOT_ONLY_AGENT_TOOLS = ["spawn_agent", "followup_task", "interrupt_agent"]`；子会话经 `AgentSnapshot::without_tools` 禁用它们，保留 `send_message`、`list_agents`、`wait_agent`。

| 工具 | 参数 | 行为 | 返回 |
|---|---|---|---|
| `spawn_agent` | `task_name`, `message`, `fork_turns?`, `model?`, `reasoning_effort?` | 创建子会话并以 `new_task` 邮件启动其首个 run；不等待 | `{"task_name":"/root/x"}`；details 为 `ToolResultDetails::Subagent { session_id }` |
| `send_message` | `target`, `message` | `deliver_mail(Message)`，不唤醒 | `Message sent to /root/x.` |
| `followup_task` | `target`, `message` | 目标必须是子 agent；`deliver_mail(NewTask)`：空闲时唤醒，忙时进入 mailbox 并保证再跑一个 Turn | `Task sent to /root/x.` |
| `wait_agent` | `timeout_ms?` | 等待本会话的邮箱活动、用户插话或超时 | `Wait completed.` / `Wait interrupted by new input.` / `Wait timed out.`，钳制时附 Codex 的提示句 |
| `list_agents` | `path_prefix?` | 列出 `/root` 与全部子 agent；`path_prefix` 按 Codex 做不带结尾 `/` 的路径前缀过滤 | Codex 结构：`{"agents":[{"agent_name":"/root/x","agent_status":…}]}` |
| `interrupt_agent` | `target` | 目标必须是子 agent；`cancel_session` 当前 run。只有 `/root` 拥有该工具，而 `/root` 作为目标已被拒绝，所以不存在「中断自己」 | `{"previous_status":…}` |

`agent_status` 取值沿用 Codex `agent_status_output_schema`：活着且有 active run 的会话报告 `"running"`；否则子 agent 按 `ChildStatus` 报告 `"pending_init"`、`{"completed": <截断后的最终回答或 null>}`、`{"errored": "<message>"}`、`"interrupted"`。`/root` 有 active run 时报告 `"running"`，否则报告 `{"completed": null}`。

### 工具描述

描述改写自 Codex v2（`multi_agents_spec.rs`），去掉嵌套相关语句：

- `spawn_agent`：Spawns an agent to work on the specified task. If you spawn_agent with task_name "task_3" the agent will have canonical task name `/root/task_3`; refer to it as `task_3` or `/root/task_3`. The spawned agent can send you and other running agents messages, and its final answer will be provided to you when it finishes. It cannot spawn agents of its own. Passing `fork_turns="none"` will not pass any surrounding context to the spawned agent, whereas `fork_turns="all"` (the default) provides it with all surrounding context.
- `send_message`：Send a message to an existing agent. The message will be delivered promptly. Does not trigger a new turn.
- `followup_task`：Send a follow-up task to an existing non-root target agent and trigger a turn if it is idle. If the target is already running, deliver the task promptly at message boundaries or after the pending tool call completes.
- `wait_agent`：Wait for a mailbox update from any live agent, including queued messages and final-status notifications. The wait also ends early when new user input arrives. Does not return the content; returns either a summary of which agents have updates, an interruption summary for new input, or a timeout summary.
- `list_agents`：List live agents in the current root thread tree. Optionally filter by task-path prefix.
- `interrupt_agent`：Interrupt an agent's current turn, if any, and return its previous status. The agent remains available for messages and follow-up tasks.

### 参数校验

参数在 `TryFrom<&ToolCall>` 中一次校验，执行阶段只处理已校验的值。

- `message`：去掉首尾空白后不能为空，否则返回 `message must not be empty`。
- `target`：按「名称与寻址」解析；`send_message` 与 `followup_task` 的目标是调用者自己时返回 `an agent cannot message itself`。
- `fork_turns`：缺省、空串、`all`（大小写不敏感）或正整数都视为全量继承（Codex 接受旧数值写法）；`none` 不继承；其余返回 ``fork_turns must be `none` or `all` ``。
- `model`：`provider/model` 引用，经 `session.model.resolve_reference` 解析；省略时继承。
- `reasoning_effort`：`none | minimal | low | medium | high | xhigh` 映射为 `ThinkingLevel::{Off, Minimal, Low, Medium, High, XHigh}`；省略时继承。
- `timeout_ms`：缺省 30 000；大于 3 600 000 返回 `timeout_ms must be at most 3600000`；小于 10 000 钳到 10 000。

## 消息

```rust
/// One inter-agent message; the only owner of its model-facing text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct AgentMail {
    pub author: AgentPath,
    pub recipient: AgentPath,
    pub kind: MailKind,
    pub payload: String,
}

/// Codex InterAgentMessageType subset used here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind { Message, NewTask, Result }
```

- `MailKind::triggers_turn()` 只对 `NewTask` 为真，对应 Codex 的 `trigger_turn`。
- `AgentMail` 定义在 `protocol`，因为它要作为 `RunInput` 启动 run。`impl From<AgentMail> for ExtensionMessage` 是生成持久化消息的唯一入口。
- `protocol::RunInput` 增加 `Mail(AgentMail)` 变体，只用作唤醒时新 run 的首条输入，从不进入用户队列。`PromptInputExpansion` 增加对应分支：`message_content()` 返回下述 Extension 消息，`text()` 返回 payload 作为 extension 事件的文本投影；不经过 `!` 用户 bash、Slash Command 或 Skill 展开。ACP prompt 映射从不产生该变体。

- 模型可见文本照搬 Codex `InterAgentMessage` 的正文：

  ```
  Message Type: message | new_task | result
  Task name: <recipient path>
  Sender: <author path>
  Payload:
  <payload>
  ```

- 持久化为 `MessageContent::Extension`：`extension_id` 为产品 slug，`custom_type = "agent_message"`，`blocks` 为上述文本，`display = true`，`include_in_context = true`，`details = {"kind","author","recipient"}`。provider 已把 include_in_context 的 Extension 消息作为 user 角色发送（`provider.rs:586`），压缩也保留它们（`compaction/context.rs:400`）。
- 类型：`spawn_agent` 与 `followup_task` 为 `new_task`；`send_message` 为 `message`；完成通知为 `result`。

## 投递

agent 之间的全部通信经 `Kernel::deliver_mail` 进入接收方 mailbox，不使用 steering / follow-up 队列。

### `Kernel::deliver_mail`

```rust
/// One inter-agent delivery request.
#[derive(typed_builder::TypedBuilder)]
pub struct MailDelivery {
    pub recipient: SessionId,
    pub mail: AgentMail,
    /// Trace of the sending Run; a woken recipient Run links to it.
    #[builder(default)]
    pub link_trace: Option<TraceId>,
    /// Sender Turn token that aborts a wait for an idle recipient's gate.
    #[builder(default)]
    pub cancellation: Option<CancellationToken>,
}

/// Where one delivered mail landed.
pub enum MailPlacement { Queued, Written, Woken }

impl Kernel {
    /// Queues mail into a running recipient, or writes it into an idle one and wakes it for a new task.
    pub async fn deliver_mail(&self, delivery: MailDelivery) -> Result<MailPlacement, KernelError>;
}
```

`deliver_mail` 与 `create_owned_session` 同为公开的 kernel 原语，工具与集成测试都经它投递。

1. payload 截断到 `tools::TOOL_TEXT_BYTES`，截断时追加 `\n\n[message truncated]`。完成通知已按自己的规则截断，到这里不会再截断。
2. 目标子会话已持久化但未加载时，先 `resume_session`。
3. 锁住接收方 `execution.queue`。`open` 为真（接收方有 agent run 在跑）时把邮件推入 `mail` 通道，递增接收方邮箱计数，返回 `MailPlacement::Queued`。
4. 否则释放队列锁：
   - 不唤醒的邮件：`acquire_operation().await`（与调用方的取消令牌 `select`）。拿到执行 gate 后接收方没有 run，用新生成的 `TurnId` 以 `persist_and_emit` 写入记录（与直接 Slash Command 的 `persist_and_emit_slash_message` 相同路径，事件经 `session_sink` 发往接收方投影），返回 `MailPlacement::Written`。
   - `new_task` 邮件：在**接收方自己的** `SessionBackgroundWork` 中提交唤醒任务，以 `run_with_source(RunInput::Mail(mail), session_sink, RunInvocation { input_source: Extension, link_trace: Some(调用方 trace) })` 启动接收方的新 run，返回 `MailPlacement::Woken`。关闭接收方会 join 该任务；关闭 owner 经级联关闭子会话。若接收方此刻已被别的 run 占用，该任务按 gate 的 FIFO 顺序在其后运行。
   - 唤醒任务的 run 在开始前就失败（例如接收方正在关闭）时，若接收方是子 agent 且注册表状态仍为 `PendingInit`，改为 `Errored(error)`，以免 `PendingInit` 永久占用并发名额。

`ponytail:` 第 3 步看到队列关闭、第 4 步拿 gate 之间若接收方开始新 run，不唤醒的邮件会在该 run 结束后写入，不丢失；需要更及时时在等待 gate 时同时监听队列打开。

### `mail` 通道

- `PendingQueue` 增加 `mail: VecDeque<AgentMail>`，只在内存中，不写 `QueueEnqueued`，不出现在 `PendingMessages` 与 WebUI 队列中。
- 消费：`TurnExecution::model_request`（`turn.rs:460`）在 `refresh_command_sessions` 之前排空 `mail`，逐条写入记录，再刷新 `<subagents>` note。每次模型请求前都会执行，包括 run 的第一个 Turn。
- Turn 结束时 `claim_next_or_close` 在同一把队列锁内判断：没有用户队列项、但 `mail` 中有 `new_task` 邮件时保持队列打开并继续一个 Turn；只有不唤醒的邮件时照常关闭。因此只排队的邮件不会延长一个已准备结束的 run，`new_task` 一定会被处理。
- run 关闭：在 `close_queue()`（`turn.rs:1398`）之后、`withdraw_pending` 与 `RunSettlement` 之前，把 `mail` 中剩余邮件以本 run 最后一个 Turn 写入记录，`sync_store` 后再继续结算；成功、失败、取消、达到 `max_turns`（`KernelError::TurnLimit`）一律执行。不唤醒的邮件全部写入记录；剩下的 `new_task` 按 run 的结局处理：
  - run 被取消：写入记录、不唤醒，避免 `interrupt_agent` 或用户停止后子 agent 立刻重新运行。
  - 其他结局（失败、达到 `max_turns`、在 `claim_next_or_close` 之后才到达）：只用**最后一条** `new_task` 唤醒一次，其余剩余邮件先写入记录，新 run 的首个模型请求一并读到；排在它之后的纯消息会写在它前面。新 run 把它作为首条输入消费掉，不会出现重复，也不会循环，多条 `new_task` 也只开一个 run。
- `withdraw_pending`、`clear_pending_messages`、`remove_pending` 只处理用户的 steering 与 follow-up 通道；邮件从不进入这两个通道，因此不会被撤回或清空。

`ponytail:` 忙时邮件只在内存中，进程崩溃会丢失；此时接收方 run 同样丢失。需要崩溃恢复时为 `mail` 写 lane record。

### 邮箱信号与 `wait_agent`

- `SessionExecution` 增加 `mailbox: tokio::sync::watch::Sender<MailboxActivity>`，值为单调递增的邮件计数与用户输入计数：邮件进入 `mail` 通道时递增邮件计数，用户 steer / follow-up 入队时递增用户输入计数。
- `wait_agent` 订阅后先检查：`mail` 非空立即返回 `Wait completed.`；否则 `select` 信号变化、`tokio::time::sleep(timeout)` 与本 Turn 取消令牌。邮件计数变化返回 `Wait completed.`，用户输入计数变化返回 `Wait interrupted by new input.`，超时返回 `Wait timed out.`。
- 返回后工具批次结束，下一次模型请求前 `mail` 被排空，内容自然进入上下文。

## 完成通知

- `RunSettlement::settle` 发出 `AgentSettled` 之后，若会话带 `ownership`，向 owner 的 `SessionBackgroundWork` 提交一个通知任务。任务在独立 tokio 任务中执行，不持有子会话的 gate；子 run 随后正常释放 gate。
- 内容：`Completed` 为最后一条 assistant 文本（按字节截断到 `TOOL_TEXT_BYTES`，提示文字计入上限，截断时附 `[answer truncated; open the agent session for the full answer]`，空答案为 `(no output)`）；`Failed(msg)` 为 `Agent errored: {msg}`；`Cancelled` 不投递。
- 注册表状态同时更新为 `Completed(Some(截断文本))`、`Errored(msg)` 或 `Interrupted`。
- 投递一律为 `deliver_mail(Result)`：owner 忙时下一次模型请求前进入上下文且不延长 run；owner 空闲时写入记录，不唤醒。
- 用户直接在子会话里对话产生的 run 同样触发完成通知，与 Codex 一致。
- `shutdown` 已触发或投递返回 `KernelError` 时记 warn 后放弃，不重试。
- 删除 `Reporter` 与 `reported` 去重表：每个 run 只结算一次，天然只通知一次。

## spawn

1. 校验参数。
2. 进入 owner 的 `spawning` 锁，后续检查与插入都在锁内完成：
   - `task_name` 在注册表中已存在时返回 `{task_name} already exists; use followup_task.`。
   - 「有 active run 或状态为 `PendingInit` 的子 agent」数量达到 `subagent.max_agents` 时返回 `agent limit reached: {n} agents are running; wait for one to finish.`。
3. 分配昵称，以 `SessionOwnership { session_id: owner, name, nickname }` 经 `OwnedSessionSpawn` 创建子会话，标题为昵称。
4. `AgentSnapshot::try_from(owner)` 后 `without_tools(&ROOT_ONLY_AGENT_TOOLS)`，再应用 `model` / `reasoning_effort` 覆盖；子会话以父会话当前的 plan mode 开始，之后各自独立切换。
5. `fork_turns` 为全量时，`ForkedHistory::from(&owner_history)` 过滤后逐条以新的 `MessageId` / `EntryId` 写入子会话，保持原顺序与原时间。过滤规则照搬 Codex `keep_forked_rollout_item`：
   - 保留：`User`、`ExpandedUser`，以及只保留文本块后非空的 `Assistant`。
   - `CompactionSummary` 与 `BranchSummary` 转为 `MessageContent::User { blocks: [Text(model_text())] }`。clawcode 不把这两种摘要存为消息，而是从 `Compaction` / `BranchSummary` entry 现场生成（`replay.rs:67`、`:116`）；原样复制会留下指向父会话的 `entry_id`，并被子会话的压缩逻辑误当作自己的检查点（`compaction.rs:110`）。`model_text()` 正是 provider 发给模型的文本，模型看到的上下文不变（Codex 同样保留压缩结果）。
   - 复制的 `Assistant` 只保留文本块，`metadata.usage` 清零，`model_message` 置为 `None`，避免 `/session` 统计重复计入父会话用量，也避免 provider 回放原生数据中的工具调用与 reasoning（Codex：子 thread 继承模型上下文，不继承累计用量）。
   - 丢弃：`System`、`ToolResult`、`BashExecution`、`SlashCommand`、所有 `Extension`（agent 消息、名单与 plan mode 等隐藏 note）、带错误的 assistant 尝试。
   - 无论是否继承，历史末尾都追加该子 agent 的 `subagent_role` note（见「角色说明」）。
   - 子会话经 `create_owned_session(OwnedSessionRequest { ownership, title, model?, thinking_level?, history })` 创建；`OwnedSessionRequest` 用 typed-builder，继承历史在应用设置之后、注册之前写入。
6. 注册表插入 `PendingInit` 的子 agent 后释放 `spawning` 锁，以 `deliver_mail(NewTask)` 唤醒子会话启动首个 run。投递返回错误时把该子 agent 从注册表移除并把错误返回给模型。
7. 返回 `{"task_name":"/root/x"}`。

`followup_task` 的目标空闲时，在同一把 `spawning` 锁内做同样的并发上限检查；目标正在运行时不计入新增。

## `<subagents>` 名单

- `SubagentRosterNote` 与 `CommandSessionsNote` 都实现 `StatedNote`（`ClawcodeEntry` 之上的 trait，默认实现消息构造、从历史读回声明、重放重建，`Session::refresh_note` 与 `SessionTranscript::append_note` 为两者共用）：`CUSTOM_TYPE = "subagent_roster"`，渲染为 `display = false`、`include_in_context = true` 的 Extension 消息，details 保存路径列表。
- `Session::refresh_subagent_roster` 在每次模型请求前比较注册表当前名单与历史中最后一条 note 的声明；不同时追加一条新 note。没有子 agent 且从未声明过时不追加；名单由非空变为空时追加空名单 `<subagents>\n</subagents>`。
- 格式与上限照搬 Codex `format_agent_paths`：最多 8 条、1024 字节。

  ```
  <subagents>
    <agent name="/root/x" />
  </subagents>
  ```

- 只有 owner 有名单；子会话没有子 agent，不追加。

## 角色说明

Codex 给 `/root` 与子 agent 各一段 usage hint。缺少它时，继承了父会话历史的子 agent 会把父会话里的用户请求当成自己的任务：实测 deepseek-flash 的子 agent `probe` 继承了「spawn probe，然后等到 probe 的结果」，于是自认为 `/root`，反复 `wait_agent` 等自己的结果，`/root` 随之永远等待。

两段说明的位置都以不破坏 prompt cache 为前提：

- **团队说明**：`list_agents` 的 `prompt_contribution` 给出两条 guideline，进入 System Prompt。`/root` 与子 agent 都有 `list_agents`，所以两者的 System Prompt 逐字相同；措辞因此不针对角色：

  - Agents are addressed by task path: `/root` is the primary agent working with the user, and the agents it spawns are `/root/<task_name>`; you are `/root` unless your context says you were spawned as another agent
  - Messages between agents arrive as user text starting with `Message Type:` (`new_task`, `message`, or `result`), then `Task name:` (the recipient), `Sender:`, and `Payload:`; a `result` is the final answer a spawned agent sends `/root` when it finishes a task

- **子 agent 角色 note**：`ForkedHistory::with_role_note` 在 spawn 时把一条 `display = false`、`include_in_context = true`、`custom_type = "subagent_role"` 的 Extension 消息写在继承历史之后、首条 `new_task` 之前（Codex 同样把 subagent hint 追加在 forked history 之后）。它只写一次，继承部分保持父会话发出时的样子，子 agent 自己的前缀之后也不再变化。`ForkedHistory` 丢弃所有 Extension，note 不会继续传递。正文（`{path}` 为子 agent 路径）：

  > You are `{path}`, an agent spawned by `/root` in a team of agents collaborating on the user's goals. Messages before this note, if any, are `/root`'s conversation copied for context: the requests and tasks in them were given to `/root`, not to you, and their tool calls are historical records whose live processes you do not have. Your task is the `new_task` message addressed to `{path}`, and later `new_task` messages bring follow-up tasks. When you finish a task, your final answer is delivered to `/root` automatically, so never wait for your own result. You can use `send_message` to message `/root` or other agents, `list_agents` to see the team, and `wait_agent` to wait for messages, but you cannot spawn agents or assign tasks.

- 不把角色文字放进 System Prompt：那会让 `/root` 与子 agent 的 System Prompt 不同。

## 并发与死锁

- 持有自己 gate 时等待其他会话 gate 的唯一情形，是工具调用经 `deliver_mail` 把不唤醒的邮件投给空闲或正在结算的接收方；该等待与本 Turn 取消令牌 `select`。
- 完成通知与唤醒任务都在独立任务中执行，不持有发起方 gate 等待接收方。
- 非 run 操作（压缩、Slash Command）从不投递邮件。
- 因此不存在持有 gate 的等待环。`max_agents` 只在 spawn 与唤醒空闲子 agent 时检查，用户直接在子会话发起的 run 不计入。

## 删除与替换

- 删除 `subagent/foreground.rs`、`tests/subagent/foreground.rs`、`background.rs` 中的 `BackgroundAction`、`Reporter`、`reported`；`SUBAGENT_TOOL_NAMES` 改为 `ROOT_ONLY_AGENT_TOOLS`。
- 删除 `Kernel::submit`、`SubmitRequest`、`SubmissionReceipt`、`QueueWaiter::Receipt`、`RunInvocation.cancellation_parent` 及其在队列、结算与 lifecycle 中的处理：它们的生产调用方只有被删除的两个 subagent 工具。`QueueWaiter` 只剩一个变体，改为 `Option<Arc<InsertionNotice>>`。`SubmissionAnswer` 与 `AnswerOutcome` 一并删除：结算直接把 `AgentOutcome` 与最后一条 assistant 文本交给完成通知，`run_with_source` 只返回 `RunResult`。`WhenBusy` 与 `config::QueuePlacement` 继续服务 ACP 用户 prompt，保留。约 50 处以 `submit` 驱动 run 的测试改用 `Kernel::run`。
- 删除前台 Turn 令牌派生路径：所有子 run 都在 background 边界之外，父取消不级联到子 agent。
- 删除前台「继承父 trace」路径与整个 `TraceOrigin`：所有 run 新开 trace（`Kernel::new_trace_id`），`RunInvocation.link_trace: Option<TraceId>` 只决定开始行是否写 `as subagent linked to trace {parent}`。
- 删除 `subagent.reply_as` 配置与 `Kernel.subagent_reply_as`。
- 保留 `OwnedSessionSpawn`、`AgentSnapshot`、共享 MCP、级联 close / delete / resume、`SessionEventSinks`、`ToolResultDetails::Subagent`、`SessionBackgroundWork` 的 `JoinSet` / `shutdown`（`spawning` 移入 `AgentRoster`）。
- 模块：`subagent/` 下按职责拆分为 `path.rs`（工具参数到 `AgentPath` 的解析）、`mail.rs`（`deliver_mail`、`mail` 通道写入与唤醒）、`roster.rs`（注册表与名单 note）、`fork.rs`（`ForkedHistory`）、`tools.rs`（六个工具），沿用原模块的 `SubagentError`。

## 配置

```toml
[subagent]
# Children allowed to run at the same time.
max_agents = 3
```

`SubagentConfig` 只有 `max_agents: NonZeroUsize`（默认 3，与 `config::app` 的 `max_batch_size` 一致，`0` 由反序列化拒绝）。wait 超时常量 10 s / 30 s / 1 h 写死在工具中。

## ACP

- `session/list` 的 `_meta.clawcode.ownership` 输出新结构 `{ "sessionId", "name", "nickname" }`。
- agent 消息与名单 note 是普通 Extension 消息，现有投影原样处理；`spawn_agent` 的 details 沿用 `ToolResultDetails::Subagent`。
- `projection.rs` 中以 `Kernel::submit` 驱动 kernel 发起 run 的测试，改为经 `deliver_mail(NewTask)` 唤醒会话的路径验证。

## WebUI

- `model.ts` 的 `SessionOwnership` 改为 `{ sessionId, name, nickname }`，`controller.ts` 解码器同步。
- `BackgroundSubagents`：筛选 `ownership.sessionId === 当前会话`；标签显示昵称，悬停提示 `/root/<name>`；标题文字改为「子 agent」；轮询、打开、停止不变。
- `ToolCallCard`：带 `subagentSessionId` 的卡片在子会话运行中（`runningSessionIds`）自动展开 `SubagentPanel`，停止运行后折叠；用户手动切换在子会话运行状态再次变化前保持。运行中的子 agent 的 MCP elicitation 因此直接可见。运行状态来自标签栏每 5 秒一次的轮询，展开最多滞后 5 秒。
- `SubagentPanel` 的紧凑视图隐藏首条 `new_task` 之前的继承历史，代之以一行「继承了 N 条父会话历史」；「打开会话」后的主视图完整显示。
- 新增 `AgentMailCard` 渲染 `custom_type === "agent_message"` 的 Extension 消息：标题为 `author → recipient` 与类型标签（消息 / 任务 / 结果），正文为 `\nPayload:\n` 之后的文本，以 Markdown 渲染；样式复用 `extension-card`。
- 删除 `MessageView` 中 `[subagent …]` 回报正则与 subagent 回报样式。
- `SessionSidebar` 嵌套、`McpPanel` 共享检测不变。

## 错误处理

- 模型可自行纠正的用法问题以文本工具结果返回并标 `is_error`（Codex 把 `RespondToModel` 返回为 `success: false`），模型照样读到原文自行纠正，WebUI 把该工具卡片显示为失败；措辞照搬 Codex：名称校验失败、名称重复、``live agent path `/root/x` not found``、`root is not a spawned agent`、`an agent cannot message itself`、空消息、并发上限、参数非法、未知模型。
- 基础设施失败使用 `SubagentError`（`#[from] KernelError`），经 `From<SubagentError> for ToolError` 转为 `ToolError::Execution`。
- 子 agent 失败不会让父 run 失败，只以 `result` 邮件告知。

## 日志

遵循 CLAUDE.md 第 13、14 条：全路径宏、正文格式参数、不记录任务、消息或回答正文。

- spawn 开始 / 完成 / 失败：owner、child、名称、fork 模式。
- 投递：类型、author → recipient、放置方式（Queued、Written、Woken）。
- 完成通知：已投递，或跳过及原因。
- `wait_agent` 结束原因：mailbox、interrupted、timed_out。
- interrupt 与并发上限拒绝。
- 上下文 note 更新：note 类型与 Turn（名单 note 与 `<command_sessions>` note 共用 `StatedNote` 机制）。
- run 关闭时写回剩余邮件：写入条数，是否唤醒了下一个 run。

## 测试

按 TDD 实施；测试只位于 crate 的 `tests/` 目录或 `#[cfg(test)] mod tests`。新测试以 `Kernel::run` 与 `Kernel::deliver_mail` 驱动，不使用将被删除的 `Kernel::submit`。

- `protocol`：`AgentPath` 校验与解析（含裸 `root`、保留名、深度、大写、`/`）；`AgentNickname::allocate` 的确定性选择与用尽后的序数后缀；`AgentMail` 渲染的 Codex 文本与 `ExtensionMessage` 转换。
- `store`：带 `SessionOwnership` 结构体的 header 读写与 `list`。
- `kernel/tests/subagent/`（重写现有用例，使用 `ScriptedModel`）：
  1. spawn 立即返回；子会话 ownership、昵称标题、`new_task` 作为首个 run 的 Extension 输入；`fork_turns=all` 只复制过滤后的历史且复制的 assistant usage 为零、无 `model_message`，压缩与分支摘要成为内容等于 `model_text()` 的 `User` 消息，`none` 不复制；`model` / `reasoning_effort` 覆盖；子会话不含 `ROOT_ONLY_AGENT_TOOLS`。
  2. `RunInput::Mail` 不触发 `!` 用户 bash、Slash Command 或 Skill 展开，且从不出现在 `PendingMessages`。
  3. `send_message`：空闲接收方写入记录且不开始 run；忙接收方在下一次模型请求前进入上下文；run 结束时剩余邮件写入且不追加 Turn；run 失败、取消、用户清空队列后邮件仍在记录中。
  4. `followup_task`：空闲时唤醒；忙时进入 mailbox，模型本已准备结束时也再跑一个 Turn；run 被取消时剩下的 `new_task` 写入记录且不自动再唤醒；run 因 `max_turns` 结束时剩下的 `new_task` 唤醒新 run，且记录中只出现一次。
  5. 完成通知：owner 空闲时写入且不唤醒；owner 忙时不延长 run；失败投递 `Agent errored`；中断不投递并把状态记为 `interrupted`；用户在子会话对话同样通知。
  6. `wait_agent`：邮件、用户 steer、用户 follow-up、超时四种返回；已有邮件立即返回；钳制提示。
  7. `list_agents` 状态取值（含 `pending_init`、root 空闲时的 `{"completed": null}`）；名单 note 只在变化时追加。
  8. `interrupt_agent`；目标 root 被拒绝；`send_message` / `followup_task` 发给自己被拒绝；空消息被拒绝。
  9. 并发上限：连续与同一批并行的 spawn 不超过 `max_agents`；`followup_task` 唤醒空闲子 agent 同样受限；关闭一个尚未完成首个 run 的子会话后名额释放，不会永久停在 `PendingInit`。
  10. 子 agent 给兄弟与 `/root` 发消息，`list_agents` 在子 agent 中读到 owner 的名单。
  11. 无死锁：root 向结算中的子 agent `send_message` 的同时，该子 agent 的完成通知投向 root，带超时断言均完成。
  12. 关闭 owner 时挂起的完成通知、唤醒任务与子 run 不会卡住。
  13. 角色说明：继承与不继承的子 agent 都以 `subagent_role` note 紧接在首条 `new_task` 之前开始，继承历史位于 note 之前；子 agent 请求的 System Prompt 与 `/root` 的逐字相同且含团队说明。
  14. 存储语义：子会话 header 带 owner 边而主 agent 日志不含子 agent 列表；删除子会话后名单与 note 更新；fork 主 agent 后追加空名单 note；fork 子 agent 得到不带 ownership 的独立会话；重启后注册表从 header 重建。
- `acp`：`session/list` ownership meta；`agent_message` Extension 的投影；kernel 发起的唤醒 run 经已安装的 sink 投影。
- WebUI：按第 9 条用生产 `app` 后端与已配置 provider，新增 `crates/app/tests/web/subagent.e2e.mjs`（参照 `exec.e2e.mjs`）：spawn 两个子 agent 后 `wait_agent`，断言结果卡片、昵称标签、运行中自动展开的 spawn 卡片与紧凑视图隐藏继承历史。

## 文档

README 中 `[subagent]` 配置、subagent 工具说明、tracing 与 plan mode 中提到前台 subagent 的段落同步更新。

## 本期不做

- 多层嵌套与跨层路径：需要时为子 agent 开放 `spawn_agent`，`AgentPath` 放宽深度，注册表改为按 root 统计全树。
- Codex agent 角色（`agent_type`、`agent-roles`）与按角色的昵称池。
- 消息板（`agent_message_board`）与 `wake_on_agent_messages` 实验特性。
- `mail` 通道与完成通知的崩溃恢复。
- 子 agent 用量合计到 owner。

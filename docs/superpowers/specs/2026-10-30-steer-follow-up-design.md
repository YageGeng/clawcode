# Steer / follow-up 切换设计

## 目标

会话运行中发送的消息进入 Kernel 队列，用户可以选择 steer（当前 Turn 结束后、下一次模型调用前插入）或 follow-up（本轮本该结束时再处理）。默认 follow-up，`claw.toml` 可改为默认 steer。处理顺序由接纳顺序与队列保证；停止时未执行的输入退还给提交方；follow-up 队列中的项可随时转为 steer。

成功标准：

- 同一会话的 `session/prompt` 按到达服务端的顺序接纳，Agent Run 期间入队的消息按此顺序插入，每条在插入时返回自己的 `messageId`。程序提交的输入（subagent 委派与回报、扩展消息）不参与这一顺序。
- 停止后，提交方仍连接着的未执行输入按提交顺序回到其输入框，不执行；提交方已断开的项保留在队列中不丢失；程序提交的输入结算为未处理。
- 任一条消息最终只有一种结果：插入、退还、被拒绝或结果未知；不会既退还又插入。
- follow-up 项可可靠地转为 steer，进程中途退出也不会丢失；刷新、重连、多标签页下队列状态一致。

约束：

- 沿用子项目 1 的「接受即插入」：`session/prompt` 只在消息真正插入时应答。
- 忙时与空闲时对同一条 prompt 的扩展语义一致（input hook 的 Transform / Handled）。
- WebUI 是唯一 ACP 客户端，与后端同步升级。
- 代码风格沿用 docparse 与 CLAUDE.md：超过 3 个字段的结构体使用 `typed-builder`，转换用 `From` / `TryFrom`，日志使用完全限定的 `tracing::` 宏和普通格式参数。

非目标：

- steer 转回 follow-up（steer 项通常很快被取出，反向转换价值低且易与取出竞态）。
- 重启后恢复的队列项与客户端重新关联（保持现状：由下一个 Run 消费）。
- 后台 subagent 回报被撤回后的重投。
- 断线期间已插入的提交在重连后自动识别（仍显示结果未知，由用户核对）。
- `unstable_subagents` 映射（子项目 3）。

## 现状

- Kernel 已有两条持久化队列 `QueueKind::{Steering, FollowUp}`（`QueueEnqueued` / `QueueCancelled` 记录，`PendingQueue::from_records` 按记录顺序重放重建；Store 逐条追加，统一 sync），`messageId` 在入队时分配，取出时沿用。
- Run 在每个 Turn 结束后用 `next_pending` 复制下一项；下一个 Turn 开始时先发 `MessageStart`、执行 `message_start` / `message_end` hook，之后才调用 `consume_pending` 落盘，落盘前不检查该项是否仍在队列。
- input hook（`emit_input`）只在 `run_with_source` 中执行；队列路径只做 Skill / Template 展开。
- `Kernel::submit`（subagent 委派与回报）用 `SubmissionReceipt` 等待回答，并有一段「救回滞留项」循环处理入队与 Run 结束的竞态。
- ACP 每条 `session/prompt` 在分发循环中按到达顺序处理，但各自 `tokio::spawn` 后才调用 Kernel。
- WebUI 运行中发消息走自定义 `_clawcode/followUp`，只能 follow-up；队列面板只在自身操作后拉取 `_clawcode/session/pending_messages`；Composer 在 `submit` 完成前置 `submitting`，并在其 catch 中恢复被拒绝的输入。
- 停止（`cancel_session`）只取消 Run 与等锁者，残留队列项会被下一个 Run 消费。

## 1 Kernel：队列与停止

### 1.1 队列开放状态

`SessionExecution` 的队列增加开放标记，与队列内容在同一把锁内：

- Agent Run 进入 Turn 循环时打开队列。
- Run 最后一次「没有后续可做」的判断（没有可选中的项且无工具续跑）与关闭队列在同一次加锁内完成。
- Run 以失败、取消或超出 Turn 上限结束时关闭队列，并按 1.5 撤回；正常结束时最后一次判断已确认队列为空。
- 非 Agent 的操作（Slash Command、`!bash`、独立 compaction）从不打开队列。

队列关闭时入队返回 `KernelError::SessionNotRunning`，调用方改为等待执行锁并自行发起 Run。由此消除「刚入队、Run 恰好结束」导致的滞留；`Kernel::submit` 中的救回循环随之删除。

### 1.2 队列项的等待者

`PendingQueueItem` 的 `receipt` 字段替换为：

```rust
/// Who is waiting for one queued item to be consumed or withdrawn.
pub(super) enum QueueWaiter {
    /// A client prompt answered with its message id at insertion.
    Insertion(Arc<InsertionNotice>),
    /// Delegated input answered when its Run settles.
    Receipt(Arc<SubmissionReceipt>),
}
// PendingQueueItem.waiter: Option<QueueWaiter>
```

- 插入（1.3）：`Insertion` 调用 `inserted(&message_id)`；`Receipt` 维持现有行为，在回答时结算。
- 撤回（1.5、1.6）：`Insertion` 调用 `rejected(&KernelError::PromptCancelled)`；`Receipt` 结算为 `SubmissionAnswer::cancelled()`。
- 没有等待者的项：扩展 `send_user_message` 入队的项，以及重启后从记录恢复的项。
- 存活判断：`InsertionNotice::is_listening()` 在其接收端已关闭（提交方连接断开、页面刷新）时返回 `false`。

### 1.3 选中、认领与插入边界

- Run 选中下一项时，在队列锁内把它从两条队列移到「已认领」位置（每个 Session 至多一项）。认领只发生在内存中，不写记录。已认领项不出现在队列快照中，撤回、移除、转换都把它视为已不在队列。
- 已认领项由一个认领守卫持有。以「是否已开始为该项写入持久化数据」为界，守卫的结局分三种：
  - **未写入，放回。** 选中后的 `MessageStart` 与 hook 照常执行。在开始写入之前的任何退出——Turn 开始时超出 Turn 上限、model preflight 失败、事件发送失败、hook 结束后发现已取消，以及任何其他错误——都还没有为该项写过任何数据。守卫在 Run 退出时把它放回原队列头部，不写记录（与磁盘上仍是待执行的状态一致），随后由 Run 异常结束时的撤回（1.5）按存活规则处理：提交方仍在等待则撤回并返回 `-32800`；提交方已断开、扩展入队、重启恢复的项留在队列。
  - **已提交，插入。** hook 结束且未取消时，守卫执行一次组合提交：在同一把 transcript 锁内先追加消息 entry（id 为入队时预留并已随 `QueueEnqueued` 持久化的 `entry_id`），再追加 `QueueCancelled(Consumed)`。一旦开始提交，认领即结束，守卫不再放回该项。两次追加都成功即为已插入，按插入结算等待者。
  - **结果不确定，不退还。** 提交中任一次追加失败时，消息是否已经写入无从判断。沿用现有设置写入失败（`commit_setting`）的语义：transcript 标记写入失败，此后本会话的写入一律返回 `KernelError::SessionPersistenceUncertain`，直到关闭并重新打开。该项不放回、不重试、不退还；`Insertion` 等待者以 `SessionPersistenceUncertain` 拒绝（`InsertionNotice::rejected` 经 `KernelError::for_insertion` 复制错误，该函数须保留这一变体，不得压成 `Protocol` 字符串，否则 ACP 无法映射出结果未知），`Receipt` 以同一错误结算；Run 以失败结束。重新打开时按 1.10 用 `entry_id` 核对。提交成功之后的同步失败不改变该项结局：内存中它已插入，磁盘无论停在哪种状态，重新打开时的核对都只让它在历史中出现一次。
- 进程在认领后、提交前退出：该项没有终态记录，消息也未写入，恢复后回到队列，由下一个 Run 消费。在提交中途退出，同样由 1.10 的核对处理。

**队列写入失败的统一规则。** 所有队列写入——入队、移除、清空、转换、撤回（追加记录并同步）与消费（消息 entry 加 `Consumed`）——都经 transcript 的提交方法在同一把锁内完成，内存队列只在提交成功后改变。任一次追加或同步失败，都无法判断记录是否已落盘：transcript 标记写入失败并返回 `SessionPersistenceUncertain`，本会话此后的写入一律拒绝，直到关闭并重新打开。对调用方：

- 入队失败的 prompt 以 `SessionPersistenceUncertain` 结束，客户端显示结果未知，不当作被拒绝放回输入框后重发。若记录其实已落盘，重新打开后该项按记录恢复到队列（无等待者），只出现一次。
- 移除、清空、转换失败时把该错误返回给调用方（ACP 与 prompt 共用同一映射，同样带 `outcomeUnknown`），面板提示需关闭并重新打开会话；重新打开后队列以磁盘记录为准。
- 撤回与消费的处理见 1.5 与上文。

### 1.4 接纳顺序与入队规则

**接纳序号。** `SessionExecution::admission_ticket()` 同步返回一个按会话递增的 `AdmissionTicket`。ACP 处理器在分发循环中领取序号，再交给后台任务。Kernel 按序号依次接纳；每条 prompt 一直持有接纳权，直到它已入队、已被 Handled，或已拿到执行锁，才放行下一条。

**取消代际。** 领取序号时，`AdmissionTicket` 捕获子项目 1 的等待者取消令牌（当时的代际），从等待接纳、执行 input hook、入队判断，一直带到入队、Handled 或拿到执行锁为止。`cancel_session` 取消这一代际：
- 仍在等待接纳：立即返回 `PromptCancelled`；
- 正在执行 input hook：hook 返回后检查该令牌，已取消则返回 `PromptCancelled`，不入队、不运行；
- 回退等执行锁：`acquire_operation_for_prompt` 使用这枚已捕获的令牌，而不是读取当时的新令牌。

`cancel_session` 会换上新一代令牌，若在回退时才读取，停止之前的请求就会继续执行。

一条 prompt 获得接纳后依次：

1. 输入是直接执行的 Slash Command 或 `!bash`：不执行 input hook、不入队，持有接纳权等待执行锁后照常运行（子项目 1 的插入语义不变）。因此排在它后面的 prompt 也要等它执行完毕。
2. 执行 input hook：`source` 为请求来源，`streaming_behavior` 为 `Steer` 或 `FollowUp`（`whenBusy` 或配置默认值）。是否属于直接命令只按**原始输入**判断一次（第 1 步）；hook 的结果一律作为普通输入，即使改写成以 `!` 或 `/` 开头的文本，也不会再被识别为命令。
   - Handled：按子项目 1 的 Handled 语义发一条只在实时流中出现的 user message，并以其 id 应答；不入队、不运行。
   - Continue / Transform：得到最终输入。
3. 队列开放：以 `QueueWaiter::Insertion` 入队，记录提交标识（1.8），调用返回，由 1.3 结算插入。
4. 队列关闭（空闲、非 Agent 操作进行中，或入队时恰好关闭）：用捕获的令牌等待执行锁并运行；`RunInvocation` 携带 `input_processed = true`，`run_with_source` 同时跳过直接命令识别（`!bash`、直接 Slash Command）与 input hook，只保留 Skill / Template 展开。这样同一输入不会执行两次 hook，改写结果也不会被当作命令执行。

入队时（第 3 步）同理：已处理的输入跳过入队路径中的直接 Slash Command 检查，只做 Skill / Template 展开。

`RunInvocation` 新增 `#[builder(default)] when_busy: Option<WhenBusy>` 与 `#[builder(default)] input_processed: bool`；子项目 1 的 `WaiterCancellation` 新增携带已捕获令牌的变体。

**其他入口。** `Kernel::submit` 使用同一入队规则，等待者为 `QueueWaiter::Receipt`，不领取接纳序号。扩展 `send_user_message` 也使用同一入队规则（无等待者），但不得阻塞调用方：会话空闲时照旧内联运行；会话不空闲且队列关闭（例如在 `!bash` 或 Slash Command 的 hook 中调用）时，在后台任务中等待执行锁并运行，调用立即返回。若内联等待，调用方自己持有的执行锁会造成死锁。

### 1.5 停止与撤回

`cancel_session` 在现有「取消 Run 与等锁者」之后（同时拒绝等待接纳者），以及 Run 以失败或取消结束时（1.1），执行撤回：

1. 关闭队列。
2. 选出要撤回的项：`Receipt` 等待者的项，以及 `is_listening()` 为真的 `Insertion` 等待者的项。没有存活等待者的项（提交方已断开的 prompt、扩展入队的项、重启后恢复的项）不撤回，留在队列中，在面板中可见，由用户移除、转换或交给下一个 Run。已认领项此时不在队列中；Run 退出时认领守卫把它放回队列头部，Run 异常结束时的撤回再按同一规则处理它。
3. 按队列顺序为每个被撤回项持久化 `QueueCancelled`，原因新增 `QueueCancellationReason::Withdrawn`，同步存储后再从内存移除。撤回记录与同步在同一把 transcript 锁内完成。若写入或同步失败，或因会话已处于持久化结果不确定状态（1.3）而被拒绝，就无法确认撤回记录是否落盘：transcript 同样标记写入失败，不返回 `-32800`（没有撤回记录的项在重新打开后会恢复并被执行），这些项留在内存队列，等待者以 `SessionPersistenceUncertain` 结算。
4. 结算被撤回项的等待者（1.2）。

`session/close` 经由同一取消路径，排队中的 prompt 同样收到 `-32800`，close 仍在相关更新送达后应答（子项目 1）。

后台 subagent 的回报被撤回时，`Reporter` 记录「未送达」并结束，不重投；子会话的最终结果仍可经 `background_subagent status` 查询。

### 1.6 移除与清空

`remove_pending_message` 与 `clear_pending_messages` 在持久化 `Removed` / `Cleared` 后结算被移除项的等待者，与撤回一致。找不到（含已认领）时保持现有的幂等成功。

### 1.7 转为 steer

```rust
impl Kernel {
    /// Moves one follow-up item to the end of the steering queue; `false` if it is no longer pending.
    pub fn promote_pending_message(&self, session_id: &SessionId, queue_id: &QueueId) -> Result<bool, KernelError>;
}
```

在队列锁内：

- 该项位于 follow-up：追加**一条**同一 `queue_id`、同一消息与 entry id 的 `QueueEnqueued(Steering)`，同步存储后在内存中移到 steering 末尾，等待者不变；返回 `true`。
- 已在 steering：不做任何修改，返回 `true`。
- 不在队列中（已认领、已插入或已撤回）：返回 `false`。

`PendingQueue::from_records` 重放 `QueueEnqueued` 时，若该 `queue_id` 已在队列中，先移除再加入目标队列（即「移动」）。转换只追加一条记录，持久化是原子的；进程在其前后任一时刻退出，恢复结果都是一个完整的队列项。存储格式不变。

### 1.8 提交标识

`QueuedMessage` 新增 `#[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)] submission_id: Option<String>`，随 `QueueEnqueued` 记录持久化并出现在队列快照中。它由客户端生成（2.1），只用于客户端把自己的提交与服务端队列项对应；不写入消息历史。

### 1.9 队列变化事件

新增 `AgentEventPayload::QueueChanged { pending: PendingMessages }`。入队、认领、认领放回、撤回、移除、清空、转换之后，经会话 sink 发出完整快照。

部分修改队列的公开方法是同步函数（如 `cancel_session`），事件因此在后台任务中发出。每个会话持有一把异步锁，后台任务在锁内读取快照并发送；读与发不在同一锁内时，先读到旧状态的任务可能后发出，使客户端停在旧快照上。串行之后，最后发出的一份总是最后一次修改之后的状态。

**关闭屏障。** 后台发布任务登记在会话的任务跟踪器中。`close_session` 拿到关闭锁之后（Run 已退出，其撤回与放回对应的发布都已登记），在 `finish_closing` 与移除运行时之前，关闭跟踪器并等待所有已登记的发布完成。所以 close 返回时，最后一份快照（通常为空队列）已经交给 sink、拿到事件序号；ACP 随后读取的最终序号覆盖它，关闭应答排在它之后（子项目 1 的关闭顺序保证）。

### 1.10 重启

从记录恢复的队列项没有等待者，停止时不撤回，由下一个 Run 消费（保持现状）。

**消费核对。** 打开会话重放队列记录后，逐项用 `entry_id` 查询 store：消息 entry 已存在，说明它在上次组合提交中已经写入历史（之后的 `Consumed` 记录没有落盘），视为已消费，不放回队列，记 `warn`。核对只读、不补写记录，每次打开结果相同。store 是单文件顺序追加，`Consumed` 记录不会先于其消息 entry 落盘，所以磁盘上只有「都没有」「只有消息」「都有」三种状态，核对覆盖了中间态。

## 2 ACP

### 2.1 `session/prompt`

- 读取 `_meta.clawcode.whenBusy`：`"followUp"` 或 `"steer"`；缺省时由 Kernel 取配置默认值；其他值返回 `-32602`。
- 读取 `_meta.clawcode.submissionId`：可选，非空字符串且不超过 128 个字符；否则返回 `-32602`。
- 处理器在分发循环中领取接纳序号（1.4），再在后台任务中调用 Kernel。应答路径不变：连接任务等待插入通知，插入或 Handled 时返回 `messageId`，被撤回或插入前被取消时映射为 `-32800`，持久化结果不确定时映射为带 `outcomeUnknown` 的 `-32603`。现有 `PromptRejection` 改名为 `KernelFailure`，扩展方法（移除、清空、转换）共用这一映射。

### 2.2 转为 steer

新增扩展方法 `_clawcode/session/pending_message/promote`，参数 `{ sessionId, queueId }`，返回 `{ "promoted": bool }`。`false` 表示该项已开始执行或已被撤回，不作为错误。

### 2.3 退役 `_clawcode/followUp`

删除 `AcpExtensionMethod::FollowUp` 及其处理与 initialize 方法列表中的条目。

### 2.4 队列同步

`QueueChanged` 经现有 `_clawcode/event` 透传，队列项含 `submissionId`。回放时快照按顺序到达，最后一份即当时的最新状态。

## 3 配置

- `crates/config/src/kernel.rs` 新增 `when_busy`，`claw.toml` 写作 `[kernel] when_busy = "follow_up"`（默认）或 `"steer"`。
- Kernel 构造时读入，作为 1.4 的缺省值。
- `UiBootstrapResponse` 新增 `whenBusy`，取值与 `claw.toml` 相同（`"follow_up"` / `"steer"`），供 WebUI 决定修饰键行为与提示文字；只有 ACP 线上的 `_meta.clawcode.whenBusy` 使用 `"followUp"` / `"steer"`。

## 4 WebUI

### 4.1 发送

- 运行中按 Enter：发送 `session/prompt`，不带 `whenBusy`。
- 运行中按 Alt+Enter（macOS 为 Option+Enter）：显式发送非默认的那一种。
- 每次发送都带新生成的 `submissionId`。
- 运行中输入框下方提示当前组合，例如「Enter 排队 follow-up · Alt+Enter 发送 steer」，默认 steer 时对调。
- 空闲时行为不变。
- 输入框不等待应答：请求发出后立即清空并恢复可用，可继续发送下一条；请求之后的全部结果由 controller 处理（4.2）。

### 4.2 请求生命周期

controller 为每个会话按提交顺序记录每次提交：提交序号、`submissionId`、原始输入（文字、图片、资源）与状态。

| 状态 | 进入条件 | 处理 |
|---|---|---|
| 已发送 | 请求发出 | 保留原始输入 |
| 已确认入队 | 在队列快照中看到自己的 `submissionId` | 保留原始输入 |
| 已插入 | 收到应答 | 删除记录 |
| 已退还 | 收到 `-32800` | 进入退还合并（4.4） |
| 被拒绝 | 收到其他确定的错误（如 `-32602`、`-32603`），不含 `data.outcomeUnknown` | 原始输入放回输入框（与 4.4 同一合并路径），并显示错误原因 |
| 结果未知 | 重连核对时既无终态应答、也不在队列中；或收到 `data.outcomeUnknown = true` 的错误（持久化结果不确定） | 显示结果未知提示，保留原始输入供确认后重发；后者另提示需关闭并重新打开会话 |

**重连核对。** 曾经确认入队，不能证明断线时仍在队列：撤回或插入的应答可能恰好在断线时丢失，已认领的项也不出现在快照中。所以断线时不直接下结论，重连后拉到第一份队列快照时，逐条核对仍未收到终态应答的提交（「已发送」与「已确认入队」）：

| 核对结果 | 处理 |
|---|---|
| 仍在当前队列中 | 转为「排队中（应答已失联）」，不提示；之后从快照中消失（被消费或被用户移除）时静默删除记录 |
| 已不在队列中 | 转为「结果未知」，保留原始输入供确认后重发 |

断线到重连之间不显示结果未知提示，避免对仍在队列中的项提示重发。子项目 1 中单条 prompt 插入前被取消、以及 Composer 原先在 catch 中的恢复，全部改由这张状态表处理。

### 4.3 队列面板

- 分 Steer 与 Follow-up 两组显示，内容来自 `QueueChanged`（附着会话时仍先拉取一次 `_clawcode/session/pending_messages`），各自操作后的主动刷新删除。
- 每个 follow-up 项提供「转为 steer」，与配置无关；移除、清空保留。
- 请求进行中禁用对应按钮；`promoted: false` 时提示「已开始执行，无法转换」。

### 4.4 退还合并

- 已退还与被拒绝的提交先进入待放回缓冲；短暂的合并窗口后统一处理：按提交序号排序，文字以空行分隔拼成一段放在当前草稿之前，图片与资源按同样顺序合并，写回会话草稿并刷新输入框。
- 提示：全部为退还时显示「已停止，N 条未执行的消息已放回输入框」；含被拒绝项时显示其错误原因。
- 已知边界：某条结果晚于合并窗口到达时会再合并一次，可能排在已放回的更晚内容之后，不额外处理。

## 错误码

| 情况 | 返回 |
|---|---|
| `whenBusy` 或 `submissionId` 取值非法 | `-32602` |
| 排队中、等待接纳或已认领未插入的 prompt 被停止、移除、清空、因关闭撤回，或所在 Run 失败结束 | `-32800`；提交方已断开的队列项不撤回，留在队列 |
| 转为 steer 时该项已不在队列 | 成功，`{ "promoted": false }` |
| 任一队列写入（入队、移除、清空、转换、撤回、消费）追加或同步失败，或会话已处于持久化结果不确定状态 | `-32603`，`data` 为 `{ "outcomeUnknown": true, "message": … }`；会话需关闭并重新打开 |
| 入队与 Run 结束竞态（队列已关闭） | 无错误，改为等待执行锁并运行 |
| 忙时提交直接执行的 Slash Command 或 `!bash` | 无错误，等待执行锁 |
| 忙时 input hook 返回 Handled | 成功，以实时 user message 的 id 应答 |

## 日志

遵循 CLAUDE.md 第 13、14 条，不记录消息正文与提交标识以外的输入内容：

- 接纳：session id、接纳序号、结果（入队 / Handled / 运行）。
- 入队：queue id、种类、来源（prompt / submit / 扩展）、submission id。
- 认领与插入：queue id、run id；认领后因取消未插入时记 info；队列写入失败记 error（操作、queue id、原始错误）；打开时核对为已消费记 warn（queue id、entry id）。
- 关闭屏障：等待中的快照发布条数（debug）。
- 停止或 Run 异常结束撤回：session id、撤回条数、原因。
- 转为 steer：queue id、结果；未在队列时记 info。
- 队列关闭后的入队回退：session id（debug）。

## 测试

按 TDD 实施；测试只放在 crate 的 `tests/` 或 `#[cfg(test)] mod tests`。

- Kernel（`crates/kernel/tests/protocol_alignment/queue.rs`）：
  - 忙时 prompt 按 `whenBusy` 或配置默认值入队，插入时才收到 `messageId`；steer 先于 follow-up。
  - 接纳序号决定顺序：后领序号的任务先被调度，插入顺序仍按序号。
  - 忙时 input hook：Transform 后的内容入队；Handled 直接应答且不入队；回退运行时 hook 只执行一次。
  - 选中后、插入前停止：不写入历史，提交方收到 `PromptCancelled`，`QueueCancelled(Withdrawn)` 已落盘；同一窗口内移除为空操作，该项照常插入一次。
  - 认领后未插入的各个出口：Turn 开始即超出上限（`max_turns = 1`）、preflight 失败、`MessageStart` 发送失败，认领项都回到队列或按存活规则撤回，不会既不在队列又无结算。
  - 认领后提交方断开、再停止：该项不被撤回，留在队列，之后由下一个 Run 插入一次。
  - input hook 阻塞时停止：hook 返回后不入队、不运行，提交方收到 `PromptCancelled`。
  - Transform 把普通输入改写成 `!echo hello` 或 `/compact`：忙时入队与空闲回退两条路径都作为普通输入交给模型，不执行 shell、不执行命令，也不被拒绝。
  - 停止撤回：有存活等待者的项被撤回并结算；提交方已断开的 prompt 不撤回，之后由下一个 Run 插入一次；重复停止只撤回一次。
  - Run 失败结束时剩余项同样撤回。
  - 移除、清空结算对应等待者。
  - 转为 steer：只追加一条记录；磁盘上只有原入队记录或两条入队记录时，恢复结果都是一个完整项（后者在 steering）；已认领或已插入返回 `false`；已是 steer 返回 `true` 且不改动。
  - Run 结束后队列关闭，之后的提交自行发起 Run，不滞留。
  - 在非 Agent 操作的 hook 中调用扩展 `send_user_message` 不死锁，消息在该操作结束后运行一次。
  - 每种队列变化发出 `QueueChanged`，快照含 `submissionId`。
  - 入队记录已追加、同步失败（注入存储故障）：入队返回 `SessionPersistenceUncertain`，忙时 prompt 的接收端得到的正是该变体；内存队列不含该项，之后的入队被拒绝；关闭并重新打开后，该项按记录恢复到队列，只出现一次。
  - 组合提交中消息已写入、`Consumed` 记录写入失败（注入存储故障）：该项不回到队列，提交方收到 `SessionPersistenceUncertain` 而非 `PromptCancelled`；同时排队且仍在等待的其他 prompt 也收到该错误；之后的入队被拒绝；关闭并重新打开后，该项不在队列中且历史中恰有一条该消息，其他排队项按记录恢复。
  - 阻塞 `QueueChanged` 的发送后关闭会话：close 在快照送出前不返回；放行后最后一份快照为空队列，且早于 close 返回。
- ACP（`crates/acp/tests/v2_alignment.rs`）：忙时 prompt 的应答 `messageId` 与插入的 `user_message` 一致；同一连接不等应答连续发送多条，插入顺序与发送顺序一致；非法 `whenBusy` / `submissionId` 返回 `-32602`；停止时多条排队 prompt 各自收到 `-32800`；排队 prompt 的请求方断开后该项仍被插入；转为 steer 的成功与 `false`；`_clawcode/followUp` 不再公布；`queue_changed` 透传；停止并关闭时最后一条 `queue_changed`（空队列）先于 close 应答到达；注入存储故障（认领项 `Consumed` 写入失败、入队后同步失败）时，忙时 prompt 经插入通知得到的应答是 `-32603` 且 `data.outcomeUnknown == true`。
- WebUI 端到端（生产 app、DeepSeek、headless Chrome）：运行中连续发送时输入框始终可用；排两条 follow-up，面板正确显示；一条转为 steer 后先于另一条插入；Alt+Enter 发送非默认种类；停止后所有未执行内容按序回到输入框，原草稿在后；非 `-32800` 的拒绝同样恢复输入并显示原因；刷新后停止，已确认入队的项留在面板，不出现结果未知；确认入队后断线、服务端在断线期间撤回，重连后显示结果未知并保留原始输入；以默认 steer 的配置副本复验提示与行为对调。

## 实现顺序

| 阶段 | 内容 | 依赖 |
|---|---|---|
| K | 队列开放状态、等待者、认领与插入边界、接纳序号与 input hook、停止撤回、移除结算、单记录转换、提交标识、`QueueChanged`、配置读入 | — |
| A | `whenBusy` / `submissionId` 解析、接纳序号领取、转换方法、退役 `_clawcode/followUp`、`QueueChanged` 透传、bootstrap 字段 | K |
| W | 发送与修饰键、请求生命周期、队列面板、退还合并、端到端验收 | A |

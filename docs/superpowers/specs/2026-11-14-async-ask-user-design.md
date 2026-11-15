# 异步 Ask User 设计

## 目标

把 Codex `request_user_input_async` 的语义引入 clawcode：模型在工作中随时向用户抛出一个或多个带候选项的问题，工具立即返回、不结束本轮也不等待；用户在 WebUI 中随时作答或跳过，答案作为一条用户侧消息回到会话。Run 运行中时答案按 steer 注入，空闲时启动新 Run。

成功标准：

- 模型调用 `request_user_input_async` 后立即拿回 `{"accepted":true}`，同一 Run 继续工作；问题出现在 WebUI 的问题面板中，不抢输入焦点。
- 用户在 Run 运行中作答，答案在下一次模型调用前进入上下文；空闲时作答启动新 Run，模型看到 Codex 格式的答复信封。
- 未答问题跨 Run 结束、刷新、重连、多标签页与恢复会话保持可答；被回答、被跳过、或用户发送普通消息后关闭，关闭后不会被重新弹出。
- 同一问题只能被结算一次：两个标签页同时作答时，后到者被拒绝。
- 空闲时跳过只写入记录、不启动 Run；模型在下一次请求中得知用户跳过了哪些问题。
- 停止 Run 时，排队中未执行的答案回到问题面板，问题重新开放。

约束：

- 沿用 [steer / follow-up 设计](./2026-10-30-steer-follow-up-design.md) 的「接受即插入」、admission 顺序与停止退还语义。
- WebUI 是唯一 ACP 客户端，与后端同步升级；不考虑任何历史数据兼容。
- 代码风格沿用 CLAUDE.md：超过 3 个字段的结构体使用 `typed-builder`，转换用 `From` / `TryFrom`，日志使用完全限定的 `tracing::` 宏和普通格式参数，不记录答案正文。

非目标：

- 阻塞式 `request_user_input`（Codex Plan 模式下的同步版本）。
- 子 agent 提问：子 agent 的疑问经最终回答交给父 agent。
- 配置开关、浏览器通知 / 标签页角标、批量作答、键盘快捷键。
- ACP 标准 elicitation 映射：elicitation 是连接级的模态请求，与「不打断、持久可答」相悖。

## 与 Codex 的对应关系

| Codex | clawcode | 说明 |
|---|---|---|
| `RequestUserInputAsyncHandler`（`core/src/tools/handlers/request_user_input_async.rs`） | kernel 工具 `request_user_input_async` | 名称、参数 schema、描述照搬，便于复用模型先验 |
| 发出 `AgentMessage { delivery: Async, questions }` | `ToolResult` 的 `ToolResultDetails::UserQuestions` | 工具卡片即问题卡片，问题随 ToolResult 持久化 |
| 模型目录 `experimental_supported_tools` 门控 | 根会话恒定注册 | 不设开关 |
| `!root_capabilities_restricted` | 子会话经 `ROOT_ONLY_TOOLS` 禁用工具策略移除 | 与 `spawn_agent` 同一机制，子 agent 看不到工具 |
| 问题身份 `["request_user_input_async", message_id, index]` | `["request_user_input_async", request_id, index]` | `request_id` 由 kernel 生成，工具调用 ID 跨 Turn 可能重复 |
| `AnsweredQuestion` 信封 `<send_user_message_question_reply>` | 同一信封，承载在 `question_reply` Extension 消息中 | 增加 `"skipped": true`（Codex 跳过时不告知模型） |
| TUI 本地 `seen_ids` / `answered_ids` | 由 transcript 推导，kernel 在作答时校验 | 多标签页、恢复会话一致 |
| 答案走普通用户输入（steer 或新 turn） | `session/prompt` + `_meta.clawcode.questionReply`，强制 steer | 复用 admission、队列与停止退还 |
| 本轮结束清空未答问题、草稿回到输入框 | 问题保留到被回答 / 跳过 / 被普通消息取代 | 偏离：WebUI 有持久 transcript，跨 Run 保留更自然 |

## 1 工具与数据模型

### 1.1 `request_user_input_async`

kernel 自有工具，位于 `crates/kernel/src/runtime/question/`，注册方式与 `exit_plan_mode`（`crates/kernel/src/runtime/plan/approval.rs`）相同，在每个会话注册。子会话沿用 `spawn_agent` 的根会话专属机制：`crates/kernel/src/runtime/session/ownership.rs` 的 `ROOT_ONLY_AGENT_TOOLS` 更名为 `ROOT_ONLY_TOOLS` 并加入本工具，`AgentSnapshot::without_tools` 把它写入子会话的禁用工具策略（`active_tools_change`）。作为 kernel 自有工具，它在 plan mode 下自动通过 `PlanModeGate`。

参数（照搬 Codex，`additionalProperties: false`）：

```json
{
  "questions": [
    { "title": "The complete question shown to the user …", "options": ["Recommended answer", "…"], "multi_select": false }
  ]
}
```

工具描述照搬 Codex `REQUEST_USER_INPUT_ASYNC_DESCRIPTION`：

> Ask the user one or more questions during ongoing work. Use this tool only to request missing information, preferences, constraints, clarification, or approval. The tool returns immediately without ending the turn or waiting for a reply; any reply arrives asynchronously as a new user message. Keep questions concise, self-contained, and easy to understand, using a level of detail appropriate to the user and task. The UI always allows a free-text answer, including when suggested options are provided. A preselected option is not submitted automatically.

`validate()` 的边界，越界时返回 `ToolError::InvalidArguments` 给模型：

| 字段 | 规则 |
|---|---|
| `questions` | 1–8 个 |
| `title` | 去空白后非空，≤ 2 KB |
| `options` | 可省略；给出时 1–8 个，每个去空白后非空、≤ 512 B，不得为 `Other`（不区分大小写），自由输入由 UI 提供 |
| `multi_select` | 可省略，默认 `false`；clawcode 在 Codex schema 上的扩展。为 `true` 时用户可多选，答案为每行一个 `- ` 项的 Markdown 列表 |

执行：

1. `context.ensure_active()`。
2. 生成 `request_id = kernel.id_generator.next(IdKind::Request)`。
3. 返回 `ToolResult`：文本块 `{"accepted":true}`，`details = ToolResultDetails::UserQuestions { request_id, questions }`。
4. `tracing::info!("asked {} questions {} in session {}", count, request_id, session_id)`。

问题随 ToolResult 写入 transcript，kernel 不另存问题状态。

### 1.2 protocol 类型

```rust
/// One model-authored question shown to the user without waiting.
pub struct AsyncQuestion {
    /// Complete question text, rendered as Markdown.
    pub title: String,
    /// Suggested answers in display order; the first is preselected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Whether the user may pick several options.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

pub enum ToolResultDetails {
    // …existing variants…
    /// Questions asked through `request_user_input_async`.
    UserQuestions {
        /// Kernel-generated identity shared by every question in this call.
        request_id: String,
        /// Questions in display order; a question is addressed by its index.
        questions: Vec<AsyncQuestion>,
    },
}

/// One settlement of one asynchronous question.
pub struct QuestionReplyItem {
    /// Identity of the asking `request_user_input_async` call.
    pub request_id: String,
    /// Position of the question within that call.
    pub index: usize,
    /// The user's answer; `None` means the user skipped the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// Settlements recorded together in one `question_reply` message.
pub struct QuestionReply {
    pub items: Vec<QuestionReplyItem>,
}
```

`RunInput` 增加变体：

```rust
pub enum RunInput {
    Text(String),
    Blocks(Vec<ContentBlock>),
    Mail(crate::AgentMail),
    /// Answers or skips for asynchronous questions; never dispatched as a command.
    QuestionReply(crate::QuestionReply),
}
```

`slash_command_text()` 对 `QuestionReply` 返回 `None`。

### 1.3 `question_reply` 消息

答案与跳过都持久化为：

```rust
MessageContent::Extension {
    extension: ExtensionMessage {
        extension_id: "clawcode",
        custom_type: "question_reply",      // protocol::QUESTION_REPLY_TYPE
        blocks: vec![ContentBlock::Text { text: <信封> }],
        display: true,
        include_in_context: true,
        details: Some(serde_json::to_value(QuestionReply)),
    },
}
```

与 `agent_message` 邮件同一模式，不新增 `MessageContent` 变体；provider 边界把它作为 User 角色消息发给模型（`crates/kernel/src/provider.rs`）。

信封由 `QuestionReply` 与被回答问题的标题生成，格式照搬 Codex `AnsweredQuestion`：

```
<send_user_message_question_reply>
[{"answer":"…","question":"…","questionItemId":"[\"request_user_input_async\",\"<request_id>\",0]"},
 {"question":"…","questionItemId":"[\"request_user_input_async\",\"<request_id>\",1]","skipped":true}]
</send_user_message_question_reply>
```

- `question` 取问题标题，截断到 512 B 的字符边界，换行替换为空格。
- 跳过项不带 `answer`，带 `"skipped": true`。
- `QuestionReplyItem` 带 `question` 字段：kernel 在结算时（`QuestionLedger::settle` / `skip`）把问题标题复制进记录，同一次分支扫描完成校验与取标题。`impl Display for QuestionReply` 输出信封文本，`TryFrom<&QuestionReply> for ExtensionMessage` 构造持久化消息；WebUI 直接从记录读取标题。
- 反向读取用标准 trait：`TryFrom<&ExtensionMessage>` / `TryFrom<&MessageContent> for QuestionReply`（错误 `QuestionReplyError`），`From<&QuestionReplyItem> for QuestionKey`。`QuestionKey` 同时是 ACP 线上形状 `{ requestId, index }`。

### 1.4 开放状态

一个问题 `(request_id, index)` 开放，当且仅当：

1. 当前分支（`SessionTranscript::active_branch()`）上存在 `details = UserQuestions { request_id, .. }` 的 ToolResult，且 `index < questions.len()`；并且
2. 当前分支上没有任何 `question_reply` 消息包含该 `(request_id, index)`；并且
3. 待处理队列（steering 与 follow-up）和跳过通道（2.2）中没有任何 `question_reply` 包含该 `(request_id, index)`；并且
4. 活动 Run 的 `reply_keys` 不包含该 `(request_id, index)`。

`reply_keys` 是 `SessionExecution` 队列状态中的一个集合，与队列内容共用一把锁，覆盖「已离开队列或跳过通道、尚未写入 transcript」的输入：

- Run 启动时（`install_run` 之后、释放 admission ticket 之前）登记首个 Turn 初始输入中的 `QuestionReply` 键（初始输入要到第一个 Turn 才落盘）；
- `claim_next_or_close()` 在 `pop_front` 取出含 `QuestionReply` 的项时，于同一临界区登记其键（取出后要到 `consume_claimed` 才落盘）；
- `take_skips()` 取出跳过通道时，于同一临界区登记其键（取出后才写入 transcript）；
- 只在 `ActiveRunLease::drop` 清空，此时 Run 的所有写入都已结束。不在 `close_queue()` 中清空：`cancel()` 会在 Run 中途调用 `close_queue()`，而首个 Turn 落盘初始输入前只做一次无锁的取消检查，清空可能落在检查与落盘之间。已落盘的键留在集合中直到 Run 结束，与 transcript 重复但无害；未提交而被放回队列的项仍在队列中，判定不变。

扫描顺序固定为**先在队列锁内读队列、跳过通道与 `reply_keys`，再读当前分支**：Run 先登记 `reply_keys`、再写 transcript，因此任一时刻答案至少出现在先读的一侧或后读的一侧。

判定由 `QuestionLedger`（`crates/kernel/src/runtime/question/open.rs`）按需扫描实现，不维护问题索引：`TryFrom<&Session> for QuestionLedger` 读取队列侧结算与当前分支，`is_open` / `open_keys` / `settle` / `skip` 回答具体问题。分支扫描读 `active_branch()` 的 Store entry，而不是 `history()`：后者是模型上下文，压缩后不再包含被摘要的 ToolResult。扫描沿用 `Kernel::reconcile_lost_exec`（`crates/kernel/src/runtime/exec/mod.rs`）的做法，先按 payload 中的 `details.type` / `extension.custom_type` 过滤，再反序列化消息。被普通消息取代的问题由 kernel 在接纳该 prompt 时显式跳过落盘（见 2.3）。切换分支后原问题不在当前分支上，自然关闭。

### 1.5 结算的串行化

`run.rs` 原本在取得 operation gate 后立即释放 admission ticket，此时初始输入既不在队列、也不在 transcript。规则：

- 所有结算请求（带 `questionReply` 的 prompt、`_clawcode/session/question/dismiss`）都持有同一会话的 admission ticket，按票号串行；开放校验在 `ticket.wait()` 之后立即执行。
- 每条路径只在结算对后来者可见之后才释放 ticket：
  - 入队路径：写入队列之后（现状如此）；
  - Run 路径：`run.rs` 把 `ticket.release()` 移到 `install_run` 与登记 `reply_keys` 之后；
  - dismiss 运行中路径：放入跳过通道之后；
  - dismiss 空闲路径：写入 transcript 之后。
- 由此任意两次结算的校验之间，前一次的结果已经出现在当前分支、队列或 `reply_keys` 三者之一，后一次必然看到并被拒绝。Run 自身只在三者之间搬运已结算的项，不改变开放性。

## 2 作答与跳过链路

### 2.1 作答：`session/prompt`

WebUI 发送：

```json
{
  "sessionId": "…",
  "prompt": [{ "type": "text", "text": "<答案>" }],
  "_meta": { "clawcode": { "submissionId": "…", "questionReply": { "requestId": "…", "index": 0 } } }
}
```

ACP 层（`crates/acp/src/server.rs` 的 `PromptOptions`）：

- 增加 `question_reply: Option<QuestionKey>` 字段，解析 `requestId`（非空，≤ 128 字符）与 `index`（非负整数）。
- 与 `whenBusy` 同时出现、或 prompt 不是单个去空白后非空的文本块时，返回 `invalid_params`，kernel 不会收到请求。块数检查在 `PromptInput::try_from` 之前进行，因为后者会把多个文本块拼接成一个。
- 构造 `RunInput::QuestionReply(QuestionReply { items: vec![QuestionReplyItem { request_id, index, answer: Some(text) }] })`。
- 派发 prompt 的后台任务把 `KernelError::QuestionClosed` 与 `PromptCancelled`、`AuthRequired` 同样视为插入前拒绝：不记 `error` 日志，不发布 `publish_terminal_fallback`。

kernel `admit_prompt`（`crates/kernel/src/runtime/run/admission.rs`）：

1. `request.ticket.wait()` 之后立即做开放校验（1.5）；不开放时返回 `KernelError::QuestionClosed { request_id, index }`，`InsertionNotice` 结算为拒绝。
2. 跳过 input hook 与直接 Slash Command 判定（二者只针对 `RunInput::Text`）。
3. placement 强制为 `WhenBusy::Steer`，忽略会话默认值。
4. 其余不变：运行中经 `enqueue` 进入 steering 队列并由 `InsertionNotice` 应答；`SessionNotRunning` 时经 `run_with_source` 启动新 Run。

`PromptInputExpansion::from_question_reply(&reply)` 把 admission 已填好标题的答案生成 1.3 的 Extension 消息（与 `from_mail` / `from_blocks` 并列），不再扫描分支；`PromptInputExpansion::Mail` 更名为 `Extension`，由邮件与答案共用。`KernelError::for_insertion` 原样保留 `QuestionClosed`，ACP 才能把它映射为带 `reason` 的 `invalid_params`。`crates/kernel/src/runtime/run.rs` 与 `crates/kernel/src/runtime/run/queue/operations.rs` 中两处 `match RunInput` 各补一个分支。pre-Agent 扩展事件看到的文本投影为信封文本。

停止 Run 时，队列中未执行的答案按现有规则退还给仍在监听的提交方（`withdraw_pending` 以 `PromptCancelled` 拒绝其等待者），问题因此重新开放；提交方已断开的答案保留在队列中，由下一个 Run 消费。

### 2.2 跳过：`_clawcode/session/question/dismiss`

新增 ACP 扩展请求：

```json
// request
{ "sessionId": "…", "items": [{ "requestId": "…", "index": 0 }, { "requestId": "…", "index": 1 }] }
// response
{}
```

`Kernel::dismiss_questions(session_id, items)`：

1. 分配 admission ticket 并 `ticket.wait()`，等待之前的结算完成（1.5）。
2. 过滤出仍开放的项；全部无效时什么都不写，直接返回成功（幂等）。
3. 按不唤醒邮件（`crates/kernel/src/runtime/subagent/mail.rs`）的语义写入，完成后才释放 ticket：
   - 有活动 Run：放入 `PendingQueue` 中与 mail 并列的内存跳过通道 `skips`。Run 在每次模型请求前（`model_request` 中 `take_mail` 之后）和 Run 结束时（`write_back_mail` 旁）用 `take_skips()` 取出并写入 transcript。跳过**不会让 Run 多跑一个 Turn**；若作为 steering 项入队，Run 的最后一个 Turn 之后会为一条跳过记录多调用一次模型。队列已关闭（Run 正在启动或收尾、或其他操作持有 gate）时，不轮询：同时等待「Run 打开队列」通知（`SessionExecution::queue_opened`，在检查队列前注册，避免错过）与 operation gate，先到者生效——队列打开则重新放入跳过通道，拿到 gate 则走空闲路径；
   - 空闲：`acquire_operation()` 后直接写入 `question_reply` 消息并发出 `MessageStart` / `MessageEnd`，**不启动 Run**，写法同 `Session::write_mail`。
4. 写入的 `QuestionReply` 中所有 `answer` 为 `None`。
5. `tracing::info!("dismissed {} questions in session {}", count, session_id)`。

停止 Run 时，跳过通道在 Run 结束时照常写回（与非唤醒邮件相同，取消也写），问题保持关闭。跳过通道只在内存中：进程在写回前崩溃时跳过丢失，问题重新开放，用户可再次跳过（实现处加 `ponytail:` 注释，与 mail 通道的同类注释一致）。跳过通道不出现在 `pending_messages` 中。

### 2.3 普通消息取代开放问题

kernel 在 prompt **写入 transcript 时**处理，所有 ACP 客户端一致：一条客户端 prompt（有等待其插入的提交者：Run 首个输入带 insertion notice，或队列项带 waiter；内容为 `User` / `ExpandedUser`，因此不含直接 Slash Command、`!` 用户 bash、邮件与答案）在 `insert_user_message` 中即将写入时，用 `QuestionLedger` 列出当前分支所有开放问题，紧挨着在它**之前**写入一条跳过记录，使 prompt 仍是模型看到的最后一条用户输入。prompt 在写入前被取消、撤回或入队失败时不产生任何跳过，问题保持开放；内部输入与扩展发送的消息不取代问题。它从不单独增加模型调用。WebUI 不再在发送前额外调用 dismiss。

此处不持有 admission ticket：同一时刻被接纳的答案可能再结算一个刚被跳过的问题，此时后到的答案在所有视图中生效（已知上限）。prompt 自身 message hook 期间发生的停止会留下跳过而退还 prompt（`ponytail:` 注释，需要时把两条消息放进同一次提交）。

### 2.4 ACP 投影与多端一致

- `UserQuestions` details 随现有 `tool_call_update` 的 `rawOutput` 投影（工具结束时为 `ToolResult`，消息落盘后与回放为整条 message），serde tag 为 `user_questions`。WebUI 只从整条 message 形式（`rawOutput.content.details`）读取问题：工具结束事件早于 ToolResult 落盘（同批其他工具结束、消息 hook 之后才落盘），在此之前 kernel 尚不认为问题存在，作答或跳过会被拒绝或无效。
- `question_reply` 消息沿用现有 Extension 消息投影，WebUI 的 `extension/upserted` 已能解码。
- 不新增事件类型。问题、答案与跳过都是 transcript 消息，经现有投影推送到所有连接；重放与恢复会话无需额外处理。

### 2.5 子 agent 继承

`ForkedHistory`（`crates/kernel/src/runtime/subagent/fork.rs`）丢弃 ToolResult 与 Extension 消息，子 agent 因而不继承问题；但 `question_reply` 携带的是用户提供的信息，应当保留：`ForkedHistory` 把 `question_reply` 转为一条 User 文本消息（内容为信封），与 Compaction / Branch 摘要的处理方式相同。跳过记录同样转换，保持与父上下文一致。

## 3 WebUI

### 3.1 状态推导

`web/src/workspace/selectors.ts` 增加开放问题选择器（类型与解码辅助放在 `domain/model.ts`）：遍历 transcript 中 `request_user_input_async` 工具结果的 `UserQuestions` details，减去 `question_reply` Extension 消息覆盖的 `(requestId, index)`，再减去待处理队列快照（`pending_messages`）中 `question_reply` 项覆盖的键，以及会话状态 `settlingQuestions` 中本标签页已提交的答案和跳过（成功后保留，失败或退还时移除，因为运行中的跳过在写入前不可见；会话转为空闲时清空，因为空闲的服务端不再持有跳过——它们要么已写入，要么随进程丢失，后者的问题由此重新显示）。前端不另存问题状态。减去队列快照使其他标签页也不会把已排队的答案显示为开放；运行中的跳过在写入前不在快照中，其他标签页短暂显示为开放，在那里作答会得到 `QuestionClosed` 提示。

### 3.2 `QuestionPanel`

新文件 `web/src/features/composer/QuestionPanel.tsx`，在 `Composer.tsx` 中与 `PlanPanel`、`PendingQueue`、`BackgroundTerminals` 一同停靠在输入框上方，有开放问题时显示：

- 「第 i 题 / 共 N 题」与前后切换。
- 标题按 Markdown 渲染。
- 候选项为单选列表，第一项默认选中但不自动提交；末尾固定「其他」，选中后展开自由输入框。无候选项时只有输入框。
- `multiSelect` 问题改用复选框（含「其他」）并提示「可多选」；发送时按显示顺序把选中项与非空自由输入拼成 `- ` Markdown 列表，答案仍是一个字符串，信封与校验不变。
- 「发送」调用 `controller.answerQuestion(sessionId, requestId, index, text)`，即 2.1 的 prompt；「跳过」调用 `controller.dismissQuestions(sessionId, [{requestId, index}])`。
- 问题到达时面板出现，Composer 焦点不变。
- 自由输入草稿按 `requestId:index` 存入 `sessionStorage`（同 `agent-draft`）。

### 3.3 Transcript 展示

- `ToolCallCard` 对 `request_user_input_async` 专门渲染：逐题显示标题与状态（待回答 / 已回答及答案 / 已跳过），只读。卡片不提供定位按钮：有开放问题时面板始终停靠在输入框上方，可前后切换。
- `question_reply` 消息渲染为用户侧气泡：每个答案为 `> 问题\n\n答案`；全为跳过时渲染为一行弱化的「已跳过 N 个问题」。

### 3.4 与 Composer 的配合

- `controller.submit` 照常发送；普通 prompt 取代开放问题由 kernel 完成（2.3）。
- `returnInput` 识别 `questionReply` 提交：退还的答案放回 `QuestionPanel` 对应问题的草稿，而不合并进 Composer 输入框。
- `PendingQueue` 的 `QueuedMessageView.text()` 识别 `question_reply` 答案项，显示为「回答：<答案>」（答案记录不含标题）；答案项已是 steering，不显示「本轮补充」按钮。移除答案项时 kernel 以 `PromptCancelled` 拒绝其等待者，WebUI 按现有的拒绝处理把答案交给 `returnInput`，问题重新开放。跳过不进入待处理队列，PendingQueue 无需处理。
- 作答被拒绝（`QuestionClosed`）时显示提示「该问题已在其他地方回答或跳过」；面板随 transcript 自动移除该题。

## 4 错误处理

| 场景 | 处理 |
|---|---|
| 参数越界、空标题、空选项或 `Other` 选项 | `validate()` → `ToolError::InvalidArguments`，模型可见 |
| 子 agent 会话 | 不注册工具 |
| 作答时问题已关闭（已答、已跳过、已在队列中、不在当前分支） | `KernelError::QuestionClosed` → ACP `invalid_params`，`data: { "reason": "questionClosed" }` |
| `questionReply` 与 `whenBusy` 并存、prompt 非单个文本块 | ACP `invalid_params` |
| dismiss 指向已关闭或不存在的问题 | 静默忽略，幂等 |
| 停止 Run | 排队答案退还给面板；跳过通道在 Run 结束时写回，问题保持关闭 |
| 进程在写回跳过通道前崩溃 | 跳过丢失，问题重新开放（已知上限，`ponytail:` 注释） |
| 模型在不同 Turn 复用同一 `tool_call_id` | kernel 按 `request_id` 区分，两组问题都可作答；WebUI 的工具卡片按 `toolCallId` 存储，较早的卡片被覆盖（现有限制，所有工具相同，不在本次范围） |
| 答案超长 | 沿用现有 prompt 长度上限；信封中的问题标题截断到 512 B |
| 日志 | 提问、作答接纳、跳过、拒绝时 `tracing::info!` / `tracing::warn!`，只记 `request_id`、`index` 与 session，不记答案正文 |

## 5 测试

按 CLAUDE.md 规则 10，测试只写在 `tests/` 目录或 `#[cfg(test)] mod tests` 中。

kernel（`crates/kernel/tests/protocol_alignment/ask_user.rs`）：

- 工具立即返回 `{"accepted":true}`，`UserQuestions` details 落盘，Run 继续下一个 Turn。
- 参数校验的每条边界。
- 空闲时作答启动 Run，模型上下文中出现信封且角色为 User。
- 运行中作答进入 steering，在下一次模型调用前插入。
- 重复作答被拒绝：答案已在 transcript 中、答案仍在队列中、答案是活动 Run 尚未落盘的初始输入、答案已被 claim 而尚未 commit 四种情形；空闲时两个并发作答只有一个启动 Run。
- 运行中跳过在下一次模型请求前写入；落在最后一个 Turn 期间的跳过不增加模型调用次数，在 Run 结束时写回；停止 Run 时同样写回。
- `ForkedHistory` 把 `question_reply` 转为 User 文本，子 agent 看到父会话中的答案。
- 作答与跳过并发指向同一问题时只有先到者生效。
- 空闲时跳过只写入、不启动 Run。
- dismiss 全部无效时不写入。
- 停止 Run 时排队答案退还，问题重新开放。
- 切换分支后问题关闭。
- 子会话的禁用工具策略包含此工具，子会话的模型请求不暴露它；plan mode 下可调用。

ACP（`crates/acp/tests/ask_user.rs`）：

- `questionReply` meta 解析与各拒绝情形。
- `_clawcode/session/question/dismiss` 往返。
- `UserQuestions` details 与 `question_reply` 消息的投影。

WebUI e2e（`crates/app/tests/web/ask_user.e2e.mjs`，按规则 9 运行于生产 `app` 后端与配置的 provider，参照 `plan_mode.e2e.mjs`）：

- 引导模型提问，面板出现且不抢焦点；在面板作答后答案进入会话（真实模型可能已结束本轮，因此不断言是否注入当前 Run）。
- 跳过后刷新，问题保持关闭。
- 存在开放问题时发送普通消息，问题被关闭并出现跳过记录。

前端组件按规则 7 不写单元测试。

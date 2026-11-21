# Desktop M2：交互与会话控制 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 desktop 的交互能力追平 WebUI，覆盖以下几项：
- 问题回答，以及计划审批、权限请求、MCP elicitation 这几类阻塞式交互
- 计划和待办面板
- slash 命令列表
- 模型、思考级别和计划模式的切换
- 上下文环和 Run 用量行
- 压缩和分支摘要卡片
- 通知、重试和错误提示
- 图片附件

另外先完成 M1b 代码审查推迟下来的结构重构。

**Architecture:**
- **后端连接：** `AcpClient` 在 initialize 时声明 elicitation 能力，并注册 agent→client 请求的 handler。每个请求的 `Responder` 留在后端线程，由一个 `PendingResponders` 表保管；UI 侧通过 `BackendEvent::Request` 和 `BackendEvent::RequestCancelled` 得知请求的出现和撤回。
- **投影：** 投影拆成两部分。transcript 行留在 `projection/mod.rs`；会话级控制状态放进新的 `SessionControls`（`projection/controls.rs`），包括配置项、命令、上下文用量、计划和待办、重试状态、问题，并按 4.4 节由 `SessionHandle` 持有。
- **UI：** 交互卡片都停靠在会话卡片内、composer 上方（spec 7.3），不弹全局模态框。只有 elicitation 的表单在用户点击「填写…」后，才用 `Dialog` 打开。

**Tech Stack:** 与 M1b 相同。新增使用 gpui-kit 的 `ProgressCircle`、`Popover`、`Attachment`、`DropdownMenu`（带勾选项）、`Checkbox`、`Radio`、`NumberInput`，以及 gpui 的 `ClipboardEntry::Image`、`ExternalPaths` 拖放；新增使用 workspace 已有的 `image` 依赖，只用 `guess_format`。

**Spec:** [Desktop 客户端设计](../specs/2026-11-19-desktop-gpui-kit-design.md)（7.2、7.3、7.4、9、12、15 节），协议细节见 [M2 协议参考](./2026-11-20-desktop-m2-protocol-reference.md)，前序计划见 [M1b](./2026-11-19-desktop-m1b-transcript-composer-settings.md)。

## Global Constraints

- M1a 和 M1b 的 Global Constraints 继续有效：AGENTS.md 规则 1–16、只用主题 token 和 rem、图标统一用 Lucide、clippy `-Dwarnings`、测试只放在 `tests/` 里。
- **不按任务提交**。整个 M2 完成、并且用户明确要求后再提交，提交时把本计划和协议参考一起带上。
- **交互卡片停靠的位置**：问题、计划审批、权限请求、MCP elicitation 一律停靠在会话卡片内部、composer 上方，顺序为附件条、待发送 chip、交互卡片、输入框。不弹全局模态框。后台会话的请求不能在别的会话上方弹窗。elicitation 的表单只有在用户点击「填写…」之后才用 `Dialog` 打开。
- **等待状态**：只要会话有未回答的权限请求、elicitation 或未结束的问题，Tab 和侧栏的状态就是 Waiting（`circle-help`），顶栏显示「等待回答」。
- **Esc 的层级**：先关最上层的弹层（命令列表、Popover、Dialog），焦点回到触发它的控件；没有弹层并且会话正在运行时，Esc 才停止运行。
- **Composer 底栏布局**（对照设计稿 `.comp .bar`）：
  - 左侧依次是附件按钮（`paperclip`，`Button::ghost().small()`）和计划模式 chip（`clipboard-list` 图标加「计划模式」，选中时用 `sidebar_accent` 底色）。
  - 右侧依次是上下文环 chip（`ProgressCircle` 加百分比）、模型和思考级别 chip（「{模型名} · {思考级别}」加 `chevron-down`）、发送/停止按钮。
  - chip 高度沿用 M1b chip 的 1.625rem。
- **Token 数字的格式**：`TokenCount(n).label()`。小于 1000 原样显示；小于 100 万显示 `{n/1000:.1}k`；其余显示 `{n/1e6:.1}M`，中英文相同。百分比保留一位小数。
- **Run 用量行**（spec 7.2）：`{模型名} · 输入 {in} · 输出 {out} · 缓存命中 {hit}% · {耗时}`。英文版为 `{model} · in {in} · out {out} · cache {hit}% · {elapsed}`。缺少的项直接省略。模型名取 `model` 配置项里与 `provider_id/model_id` 对应的选项名，找不到就用 `model_id`。
- **压缩 pill** 显示为「上下文已压缩 · {tokensBefore}」。
  - ponytail：spec 写的是「182k → 24k」，但后端不提供压缩之后的大小，所以只显示压缩前的大小。等后端在 `result` 里带上压缩后的大小再补。
- **图片限制**（以服务端为准，`crates/acp/src/input.rs`）：每条消息最多 5 张，单张不超过 10 MiB，总计不超过 20 MiB；格式只能是 png、jpeg、gif、webp，用 `image::guess_format` 按字节判断，不看扩展名。
- **需要登录的模型**：切换模型时服务端返回 -32000 并带 `data.methodId`，表示该模型需要先登录。M2 只推送通知「需要先登录该模型的 Provider」，选择回退到服务端的 `currentValue`。真正的登录流程属于 M4。只有 `requestId`、没有会话的 elicitation（登录用）在 M2 一律直接回复 `cancel`。
- 新增的文案都写进 `locales/app.yml`，en 和 zh-CN 都要有。中文术语以 spec 9.1 和协议参考里 WebUI 的用词为准。

## Review Focus

1. **请求被服务端撤回**：卡片显示期间，同一个请求在别处被回答，或者 Run 被取消，服务端会发 `$/cancel_request`。这时卡片立即消失；用户再点按钮不发送任何东西，也不能 panic。测试在 Task 2（`cancelled_request_emits_request_cancelled`）和 Task 8（`withdrawn_approval_card_disappears`）。
2. **后台会话收到问题或审批**：会话的 Tab 不在前台或已经关闭时，不弹任何窗口，侧栏和 Tab 显示 `circle-help`；切到该会话后，卡片原样停靠在 composer 上方。测试在 Task 7（`background_question_marks_the_session_waiting`）。
3. **无效的附件**：第 6 张图、11 MiB 的文件、BMP 或 SVG、读不出来的文件都要被拒绝，并给出本地化提示，不发送任何东西。粘贴同时含文字和图片的内容时，文字照常粘贴。测试在 Task 10。
4. **Esc 的层级**：命令列表开着时，Esc 只关列表；表单 Dialog 开着时，只关 Dialog；都没开并且正在运行时，才停止运行。测试在 Task 6 和 Task 8。
5. **配置切换失败**：传了不存在的值，或者模型需要登录（-32000）时，选项回退到服务端当前值，并推送通知；按钮在请求期间禁用，结束后恢复可用。测试在 Task 5。

---

### Task 1: 结构重构（M1b 审查推迟的三项）

**Files:**
- Create: `crates/desktop/src/ui/tabs.rs`、`crates/desktop/src/ui/format.rs`
- Modify: `crates/desktop/src/ui/mod.rs`、`card.rs`、`sidebar.rs`、`rows.rs`、`composer.rs`、`drafts.rs`、`lifecycle.rs`、`crates/desktop/src/transcript.rs`、`crates/desktop/src/catalog.rs`、`crates/desktop/src/projection.rs`
- Modify: `crates/desktop/tests/support/mod.rs`、`tests/support/shell.rs`，以及所有用到被移动 API 的测试文件
- Test: `crates/desktop/tests/tabs.rs`（新增）

**Interfaces:**
- Produces：
  - **`Tabs`**：`pub struct Tabs { entries: Vec<TabEntry>, active: Option<usize> }`，方法如下。规则从 M1b 的 `AppView` 原样搬过来，不改行为：
    - `pub fn entries(&self) -> &[TabEntry]`
    - `pub fn active(&self) -> Option<usize>`
    - `pub fn active_entry(&self) -> Option<&TabEntry>`
    - `pub fn open(&mut self, entry: TabEntry) -> usize`：已存在就复用
    - `pub fn close(&mut self, index: usize) -> Option<TabEntry>`：关闭后激活右侧的邻居
    - `pub fn activate(&mut self, index: usize) -> bool`
    - `pub fn move_tab(&mut self, from: usize, to: usize) -> bool`
    - `pub fn replace(&mut self, old: &TabEntry, new: TabEntry) -> bool`
    - `pub fn retain(&mut self, keep: impl FnMut(&TabEntry) -> bool)`
    - `pub fn persisted(&self) -> TabsState`

    `AppView` 改为持有 `tabs: Tabs` 和 `tab_scroll: ScrollHandle`。
  - **`SidebarState`**：`pub struct SidebarState { search_open: bool, sort: SessionSort, running_only: bool, show_all: HashSet<PathBuf> }`，提供对应的读写方法。`AppView` 改为持有 `sidebar: SidebarState`。
  - **`ui/format.rs`**：`Elapsed::label`、`RunSummary::label`、`ToolCategory::verb`、`RelativeAge::label`、`single_line` 搬到这里，以扩展 trait 或包装类型的形式实现。`transcript.rs`、`catalog.rs`、`projection.rs` 不再依赖 `rust_i18n` 和 `gpui_kit`。`impl ToolCategory` 合并回 `projection.rs`。
  - **测试 fixture 去重**：
    - `eventually` 只保留 `tests/support/mod.rs` 一份。
    - 新增 `fn finished(stop: StopReason) -> ModelStreamEvent`。
    - 删掉 `ScriptedBackend` 中没人读的字段（`kernel`；`server` 改名为 `_server`）。
    - 新增 `support::shell::live_in_fixture(cx) -> Live`：连上脚本后端，并把 fixture 工作区注册进来，替换掉 3 处重复。
    - `open()` 改为返回 `Shell { handle, view, _dir }`，并提供 `fn window(&self, cx, f)`，替换手写的 `update_window` 加 `render_frame` 块。

- [ ] **Step 1: 先写 `tests/tabs.rs`**，不依赖窗口，覆盖以下规则：

```rust
#[test] fn open_reuses_an_existing_entry()
#[test] fn close_activates_the_right_neighbor()          // [a,b,c] active b, close b → [a,c] active c
#[test] fn move_keeps_the_same_entry_active()
#[test] fn replace_rekeys_a_draft_in_place()
#[test] fn persisted_lists_only_session_tabs()           // Draft/Settings excluded, active index remapped
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test tabs`
Expected: 编译失败。

- [ ] **Step 3: 重构。** 只做搬迁，不改行为；中途和完成后都要让原有测试保持通过。

- [ ] **Step 4: 运行全部测试**

Run: `cargo test -p desktop && cargo clippy -p desktop --tests -- -Dwarnings`
Expected: 全部通过，测试数量只增不减。

---

### Task 2: 后端：agent 请求、配置项和扩展方法

**Files:**
- Modify: `crates/desktop/Cargo.toml`（`agent-client-protocol` 增加 `unstable_plan_operations` 和 `unstable_end_turn_token_usage` 两个 feature）
- Modify: `crates/desktop/src/backend.rs`
- Modify: `crates/desktop/tests/support/mod.rs`（新增脚本，见下）
- Test: `crates/desktop/tests/backend.rs`

**Interfaces:**
- Produces：
  - **请求标识和事件**：
    - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct RequestId(pub u64)`
    - `pub enum AgentRequest { Permission { id: RequestId, request: wire::RequestPermissionRequest }, Elicitation { id: RequestId, request: wire::CreateElicitationRequest } }`，带方法 `id()` 和 `session_id() -> Option<&SessionId>`
    - `BackendEvent` 新增 `Request(Box<AgentRequest>)` 和 `RequestCancelled(RequestId)` 两个变体
  - **请求处理流程**：
    - `connect` 声明 `elicitation { form, url }` 能力，并注册两个 `on_receive_request` handler。handler 立即把 `Responder` 存进 `PendingResponders`（`Arc<Mutex<HashMap<RequestId, Pending>>>`，`Pending` 是两种 responder 的枚举），然后发出 `BackendEvent::Request`。
    - handler 同时在后端 runtime 上 spawn 一个任务，`await responder.cancellation().cancelled()`；请求被撤回时，从表里移除，并发出 `RequestCancelled(id)`。
    - 没有 `sessionId` 的 elicitation（登录用）直接回复 `cancel`，打一条 `tracing::info!`，不发事件。
  - **回答请求**：
    - `AcpClient::answer_permission(&self, id: RequestId, option_id: Option<&str>) -> Result<(), BackendError>`：`None` 表示回复 cancelled。
    - `AcpClient::answer_elicitation(&self, id: RequestId, response: wire::CreateElicitationResponse) -> Result<(), BackendError>`
    - 请求已经被撤回时，两个方法都返回 `Ok(())`，什么都不做。
  - **会话的创建和恢复**：`new_session` 和 `resume_with_replay` 改为返回 `Result<SessionStart, BackendError>`，其中 `SessionStart { id: SessionId, config_options: Vec<wire::SessionConfigOption> }`。原有调用方同步修改。
  - **新增的请求方法**：
    - `AcpClient::set_config_option(&self, id, config_id: &str, value: &str) -> Result<Vec<wire::SessionConfigOption>, BackendError>`。-32000 并带 `data.methodId` 时，返回新增的 `BackendError::AuthRequired(String)`。
    - `AcpClient::compact(&self, id) -> Result<(), BackendError>`（`AcpExtensionMethod::Compact`）
    - `AcpClient::dismiss_questions(&self, id, keys: &[QuestionKey]) -> Result<(), BackendError>`（`QuestionDismiss`）
    - `AcpClient::answer_question(&self, id, key: &QuestionKey, text: &str) -> Result<(), BackendError>`：prompt 的 `_meta.clawcode.questionReply = {requestId, index}`。`data.reason == "questionClosed"` 时返回新增的 `BackendError::QuestionClosed`。
    - `#[derive(Clone, Debug, PartialEq, Eq, Hash)] pub struct QuestionKey { pub request_id: String, pub index: u32 }`，放在 `backend.rs`，Task 3 会复用。
- 新增的脚本模型，工具调用 id 用括号里的值：
  - `"plan"` → `exit_plan_mode {"plan":"1. step one"}`（`plan-1`）
  - `"ask"` → `request_user_input_async {"questions":[{"title":"Pick one","options":["a","b"]},{"title":"Anything else?"}]}`（`ask-1`）
  - `"todo"` → `todo_write`，带两项待办，一项 in_progress、一项 pending（`todo-1`；参数格式以 `crates/tools` 为准）
  - `/compact` 是内置命令，不需要脚本。

- [ ] **Step 1: 写失败的测试**

```rust
#[test] fn plan_approval_reaches_the_client_and_approve_executes()
    // set_config_option(mode=plan); prompt "plan" → events contain Request(Permission{ title "Ready to execute the plan?" });
    // answer_permission(id, Some("approve")) → wait_idle; tool plan-1 Completed
#[test] fn cancelled_request_emits_request_cancelled()
    // prompt "plan" in plan mode; on Request → client.cancel(session) → RequestCancelled(same id) arrives; answer_permission(id, ..) is Ok and a no-op
#[test] fn set_config_option_returns_the_new_list()       // thinking_level=high → returned option currentValue "high"
#[test] fn set_config_option_rejects_unknown_values()     // model "nope/none" → Err(BackendError::Request(_))
#[test] fn compact_runs_and_reports_completion()          // after "hello", compact() Ok; a CompactionUpdate completed arrives
#[test] fn question_answer_and_dismiss_round_trip()       // prompt "ask"; answer_question(key0,"a") Ok; dismiss_questions([key1]) Ok;
                                                          // answering key0 again → Err(BackendError::QuestionClosed)
#[test] fn new_session_carries_config_options()           // SessionStart.config_options ids == ["model","thinking_level","mode"]
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test backend`
Expected: 编译失败。

- [ ] **Step 3: 实现。** 先看 agent-client-protocol 5.4.0 的 `examples/v2_one_shot_client.rs` 和 `src/concepts/cancellation.rs`。不要在 handler 里 await UI，handler 必须立即返回。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test backend --test session`
Expected: 全部通过。

---

### Task 3: 投影：会话控制状态（`SessionControls`）

**Files:**
- Move: `crates/desktop/src/projection.rs` → `crates/desktop/src/projection/mod.rs`
- Create: `crates/desktop/src/projection/controls.rs`
- Modify: `crates/desktop/src/session.rs`
- Test: `crates/desktop/tests/controls.rs`（新增）

**Interfaces:**
- Consumes：Task 2 的 `QuestionKey`、`SessionStart.config_options`、`AgentRequest`。
- Produces（`projection/controls.rs`）：
  - **配置项**：`pub struct ConfigChoice { pub value: String, pub name: String, pub description: Option<String> }`，`pub struct ConfigOption { pub id: String, pub name: String, pub current: String, pub choices: Vec<ConfigChoice> }`。实现 `TryFrom<&wire::SessionConfigOption>`，只接受 select 类型，分组的选项展平。
  - **slash 命令**：`pub enum CommandSource { Builtin, Extension, Skill, Template }`，`pub struct SlashCommand { pub name: String, pub description: String, pub hint: Option<String>, pub source: Option<CommandSource>, pub qualified_name: Option<String> }`
  - **用量**：`pub struct ModelUsage { pub input: Option<u64>, pub output: Option<u64>, pub cache_read: Option<u64>, pub cache_write: Option<u64>, pub reasoning: Option<u64>, pub total: Option<u64> }`，实现 `TryFrom<&serde_json::Value>`，任何一个计数不是十进制字符串时整体返回 `Err`。另有方法 `fn context_tokens(&self) -> Option<u64>`（有 total 用 total，否则 input + output）、`fn cache_hit_permille(&self) -> Option<u64>`、`fn sum(&self, other: &ModelUsage) -> ModelUsage`。
    - `pub struct ContextUsage { pub used: Option<u64>, pub size: u64, pub exact: Option<ModelUsage> }`，方法 `fn percent_tenths(&self) -> Option<u64>`，计算方式为 `min(1000, used*1000/size)`。
  - **计划和待办**：`pub enum TodoStatus { Pending, InProgress, Completed }`，`pub struct Todo { pub content: String, pub status: TodoStatus }`
  - **重试**：`pub enum RetryState { Waiting { attempt: u32, max: u32, due_ms: u64, error: String }, Running { attempt: u32, max: u32 }, Failed { attempt: u32, error: Option<String> } }`
  - **问题**：`pub struct AskedQuestion { pub key: QuestionKey, pub tool_call_id: wire::ToolCallId, pub title: String, pub options: Vec<String>, pub multi_select: bool }`
  - **压缩状态**：`pub enum CompactionStatus { Running, Failed(String), Cancelled }`
  - **`SessionControls`**（字段私有，超过 3 个字段，按规则 5 使用 typed-builder）。读取方法：
    - `config(&self, id: &str) -> Option<&ConfigOption>`
    - `commands() -> &[SlashCommand]`
    - `context() -> Option<&ContextUsage>`
    - `todos() -> &[Todo]`
    - `approved_plan() -> Option<&str>`
    - `plan_open() -> bool`
    - `retry() -> Option<&RetryState>`
    - `compaction() -> Option<&CompactionStatus>`
    - `open_questions() -> Vec<&AskedQuestion>`
    - `requests() -> &[AgentRequest]`（`AgentRequest` 只读，`Clone` 由 Task 2 提供）
  - **写入方法**：
    - `apply(&mut self, update: &wire::SessionUpdate) -> bool`：处理 `ConfigOptionUpdate`、`AvailableCommandsUpdate`、`UsageUpdate`、`PlanUpdate`、`CompactionUpdate` 的状态部分，以及 `Other("_clawcode/event")` 里的 `usage_updated`、`retry_*`、`message_end`（`question_reply`）和 `run_end`。有变化时返回 true。
    - `set_config(&mut self, options: Vec<wire::SessionConfigOption>)`
    - `push_request` / `remove_request(id) -> bool`
    - `mark_settling(keys)` / `clear_settling()`
  - **问题从哪里来**：
    - 打开：`ToolCallUpdate` 的 `raw_output` 里，`details.type == "user_questions"`。确切的 JSON 路径以 `crates/acp/src/mapping.rs` 的 `tool_call_update` 为准，协议参考 §1 写的是 `rawOutput.content.details`。所以 `SessionProjection::apply` 处理 ToolCallUpdate 时，也要调用 `controls`。
    - 结束：`question_reply` 记录、队列快照里带回复的项（`projection.pending()`），或者本地正在提交中的 key。
  - **其他规则**：
    - 进入 Idle 时，清掉 `Running` 和 `Waiting` 两种重试状态（`Failed` 保留），清掉正在提交中的问题，`plan_open` 置为 false。
    - 压缩完成后，`context` 置为 `None`。
  - **在投影上的位置**：`SessionProjection` 新增 `pub fn controls(&self) -> &SessionControls` 和 `controls_mut`；`ProjectionChange` 新增 `pub controls: bool`，`is_empty` 和 `extend` 都要算进去。
  - **注册表**：
    - `SessionRegistry` 处理 `BackendEvent::Request` 和 `RequestCancelled` 时，转给对应会话的 handle，执行 `controls.push_request` / `remove_request`。找不到 handle 时（没有 attach 的会话），对权限请求回复 cancelled、对 elicitation 回复 cancel，然后丢弃。
    - attach 时用 `SessionStart.config_options` 调用 `set_config`。
    - `SessionHandle::status()`：有请求，或者有未结束的问题时，返回 `Waiting`。

- [ ] **Step 1: 写失败的测试**（真实 ACP 帧，加上少量手工构造的更新）

```rust
#[test] fn config_options_come_from_session_start_and_updates()   // ids model/thinking_level/mode; set thinking=high via client → update → current "high"
#[test] fn commands_snapshot_replaces_the_list()                 // hand-built AvailableCommandsUpdate twice; second replaces first; names without "/"
#[test] fn usage_update_sets_context_and_percent()               // after "hello": context().size > 0, percent_tenths() is Some
#[test] fn malformed_usage_counter_drops_the_whole_usage()       // usage {input_tokens:"x"} → exact None
#[test] fn todos_and_open_plan_follow_plan_updates()             // prompt "todo" → 2 todos, statuses InProgress/Pending, plan_open true; after Idle plan_open false
#[test] fn asked_questions_open_and_settle()                     // prompt "ask" → open_questions().len()==2; answer key0 → after events len()==1; dismiss key1 → 0
#[test] fn questions_reopen_after_replay()                       // new registry attach (replay) of a session with one unanswered question → 1 open
#[test] fn pending_request_marks_the_session_waiting()           // plan mode + "plan" → handle.status()==Waiting; RequestCancelled → status Running/Idle
#[test] fn compaction_clears_context_usage()                     // "/compact" after "hello" → context() None, compaction() None after completion
#[test] fn retry_countdown_state()                               // hand-built retry_scheduled{attempt 1,max 3,delay_ms 1200} meta ts 1000 → Waiting{due_ms 2200}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test controls`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test controls --test projection --test session`
Expected: 全部通过。

---

### Task 4: 投影的新行类型和 Run 用量

**Files:**
- Modify: `crates/desktop/src/projection/mod.rs`、`crates/desktop/src/transcript.rs`、`crates/desktop/src/session.rs`
- Test: `crates/desktop/tests/projection.rs`、`crates/desktop/tests/transcript.rs`

**Interfaces:**
- Consumes：Task 3 的 `ModelUsage`。
- Produces：
  - **新的行类型**：`TranscriptRow` 新增两个变体：
    - `Compaction(CompactionRow { entry_id: String, reason: CompactionReason, summary: String, tokens_before: Option<u64>, elapsed_ms: Option<u64> })`
    - `Error(ErrorRow { id: String, source: String, message: String })`，来自 `extension_handler_failed` 和持久化的 `handler_error` 扩展消息

    `RowKey` 相应新增 `Compaction(String)`（Display 为 `compaction:{entryId}`）和 `Error(String)`（Display 为 `error:{id}`）。`CompactionReason` 有 `Manual`、`Threshold`、`Overflow` 三个值。
  - **`MessageRow` 的新字段**：
    - `#[builder(default)] pub command: Option<CommandMeta>`，其中 `CommandMeta { name, source: CommandSource, kind: CommandKind { Invocation, Output, Expansion }, status: Option<CommandStatus { Succeeded, Failed, Cancelled }> }`，来自 `_meta.clawcode.slashCommand`；某个枚举值不合法时，整个字段为 `None`
    - `#[builder(default)] pub assistant: Option<AssistantMeta { provider_id, model_id, usage: Option<ModelUsage>, error: Option<String> }>`，来自 `_meta.clawcode.assistant`
  - **可见性**：带 `assistant.error` 的消息即使是空的，也算可见（`is_visible`）。
  - **transcript 布局**：
    - 布局新增 `LayoutRow::Usage { anchor: RowKey, usage: RunUsage }`。已结束的段，在最后一条可见的助手消息之后放一行 Usage。没有任何用量时不放；折叠的段同样放（放在摘要行之后的最后一条助手消息后面）。
    - `RunUsage { model: Option<(String, String)>, usage: ModelUsage, elapsed_ms: Option<u64> }`：`usage` 是段内所有 `assistant.usage` 之和；`model` 取最后一条带 meta 的消息；`elapsed_ms` 和 `RunSummary` 用同一个算法。
    - `element_id` 为 `usage:{anchor}`。
  - **notice**：`SessionUpdate::Notice` 不进投影。`SessionRegistry::apply_batch` 发出 `RegistryEvent::Notice { title: String, description: Option<String> }`，供 Task 5 弹 toast。

- [ ] **Step 1: 写失败的测试**

```rust
// projection.rs
#[test] fn compact_inserts_a_compaction_row()          // "hello" then "/compact" → row key "compaction:<entryId>", tokens_before Some
#[test] fn slash_invocation_and_output_carry_command_meta() // "/compact" → user row command.kind Invocation name "compact"; output row kind Output status Some(Succeeded)
#[test] fn assistant_meta_and_error_are_kept()         // "hello" → assistant.usage Some; hand-built agent_message with assistant.error → visible with empty text
#[test] fn handler_failure_becomes_an_error_row()      // hand-built _clawcode/event extension_handler_failed → RowKey::Error
// transcript.rs
#[test] fn finished_run_gets_one_usage_row()           // two assistant messages with usage in a finished segment → one LayoutRow::Usage after the last, summed
#[test] fn running_or_usage_less_runs_have_no_usage_row()
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test projection --test transcript`
Expected: 失败或编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test projection --test transcript --test session`
Expected: 全部通过。

---

### Task 5: 新行的渲染、通知 toast、重试和压缩状态

**Files:**
- Modify: `crates/desktop/src/ui/rows.rs`、`crates/desktop/src/ui/format.rs`、`crates/desktop/src/ui/session_view.rs`、`crates/desktop/src/ui/mod.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_transcript.rs`、`crates/desktop/tests/format.rs`（新增）

**Interfaces:**
- Consumes：Task 3 和 Task 4 的行、控制状态、`RegistryEvent::Notice`。
- Produces：
  - **格式化**：`ui/format.rs` 新增 `pub struct TokenCount(pub u64)`，实现 `label()`（格式见 Global Constraints），以及 `RunUsage` 的 label 方法。
  - **渲染**：

| 行或状态 | 渲染 |
| --- | --- |
| Compaction | 居中的 pill：`git-commit-horizontal` 图标，加「上下文已压缩 · {TokenCount(tokens_before)}」。点击展开，显示原因（手动 / 达到上下文阈值 / 上下文溢出恢复）、耗时，以及 Markdown 摘要 |
| 分支摘要（`MessageOrigin::BranchSummary`） | 同样的 pill 样式，文案「分支摘要」，点击展开显示 Markdown |
| 带 `command` 的消息 | 紧凑的命令卡：`square-slash` 图标、`/name`、来源徽标（内置 / 扩展 / Skill / 模板）、类型标签（调用 / 结果 / 展开调用）、状态标签（成功 / 失败 / 已取消）。正文用 Markdown，非 Output 类型去掉开头的 `/name` |
| Usage | 最后一条回复下面一行 muted 文字，格式见 Global Constraints |
| Error | danger 色的行内提示：`circle-alert` 图标、`{source}`、`{message}` |
| `assistant.error` | 消息内一块 danger 色区域：「模型调用失败」，加错误原文 |
| 重试（`controls.retry()`） | composer 上方一行 info 提示。Waiting 显示「重试 {attempt}/{max} · {剩余秒数} 秒」，每 250ms 刷新一次，只在 Waiting 状态下刷新；Running 显示「重试 {attempt}/{max}」；Failed 显示 danger 色的「重试失败」 |
| 压缩状态 | composer 上方一行：Running 显示「正在压缩上下文…」（带 `Spinner`）；Failed 显示 danger 色的「上下文压缩失败：…」；Cancelled 显示「已取消上下文压缩」 |
| notice | `window.push_notification`：标题为 title，正文为 description，6 秒后自动关闭 |

- **locale** 的 key（en / zh-CN）：
  - `Transcript.compacted`
  - `Transcript.branch_summary`
  - `Transcript.compaction_reason_manual` / `_threshold` / `_overflow`
  - `Transcript.usage_in` / `usage_out` / `usage_cache`
  - `Transcript.model_failed`
  - `Command.source_builtin` / `_extension` / `_skill` / `_template`
  - `Command.kind_invocation` / `_output` / `_expansion`
  - `Command.status_succeeded` / `_failed` / `_cancelled`
  - `Status.retry_waiting` / `retry_running` / `retry_failed`
  - `Status.compacting` / `compaction_failed` / `compaction_cancelled`

  中文用协议参考 §5、§8、§10 里 WebUI 的用词。

- [ ] **Step 1: 写失败的测试**

```rust
// format.rs
#[test] fn token_counts_are_compact() { assert_eq!(TokenCount(999).label(), "999"); assert_eq!(TokenCount(12_400).label(), "12.4k"); assert_eq!(TokenCount(1_250_000).label(), "1.2M"); }
#[test] fn run_usage_label_skips_missing_parts() // en: "Scripted · in 1 · out 1 · 3s"（no cache hit when cache_read missing）
// shell_transcript.rs
#[gpui_kit::test] fn compaction_pill_renders_and_expands()   // "hello" then "/compact" → find("compaction:..") visible, shows "Context compacted · …"; click → summary text shown
#[gpui_kit::test] fn slash_command_card_renders()            // "/compact" → shows "/compact" and "Builtin"
#[gpui_kit::test] fn finished_run_shows_the_usage_row()      // "hello" → find("usage:message:..") visible
#[gpui_kit::test] fn notice_becomes_a_toast()                // RegistryEvent::Notice → window notification count 1
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test format --test shell_transcript`
Expected: 失败或编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test format --test shell_transcript`
Expected: 全部通过。

---

### Task 6: Composer 底栏：模型和思考级别、计划模式、上下文环

**Files:**
- Create: `crates/desktop/src/ui/controls_bar.rs`
- Modify: `crates/desktop/src/ui/composer.rs`、`crates/desktop/src/ui/session_view.rs`、`crates/desktop/src/assets.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_controls.rs`（新增）

**Interfaces:**
- Consumes：Task 2 的 `set_config_option` 和 `BackendError::AuthRequired`；Task 3 的 `SessionControls::config` 和 `context`。
- Produces：
  - **`ControlsBar`**：`impl SessionView { fn render_controls_bar(&self, window, cx) -> AnyElement }`。按钮 id 如下：
    - 计划模式 chip `composer-plan-mode`：点击后把 `mode` 设为 `plan` 或 `default`
    - 上下文环 `composer-context`：点击打开 `Popover`，里面依次是 Context、`{used} / {size} tokens`、使用率、输入 / 输出、缓存读取 / 写入；没有的数据显示「未知」
    - 模型 chip `composer-model`：`DropdownMenu`，分两组：模型（所有 model 选项，当前项打勾）和思考级别（所有 thinking_level 选项，当前项打勾）。chip 文案为「{模型名} · {思考级别名}」
  - **切换流程**：`SessionView::set_config(&mut self, config_id: &'static str, value: String, cx)`。请求期间 `config_busy = true`，所有 chip 禁用。成功后用返回的列表调用 `controls.set_config`。失败时：
    - `AuthRequired`：推送通知 `Controls.needs_login`。
    - 其他错误：推送通知 `Controls.switch_failed`。

    两种情况下，选项都保持服务端当前值，不做乐观更新。
  - **草稿 Tab**：还没有配置项，chip 显示禁用状态。
  - **上下文环**：没有数据时显示「--」；达到 85% 以上时，环和文字用 `warning` 色。
  - **locale**：
    - `Controls.plan_mode` 计划模式 / Plan mode
    - `Controls.context` Context / 上下文
    - `Controls.usage` 使用率
    - `Controls.unknown` 未知
    - `Controls.models` 模型 / Models
    - `Controls.thinking` 思考级别 / Thinking
    - `Controls.needs_login` "需要先登录该模型的 Provider" / "Sign in to this model's provider first"
    - `Controls.switch_failed` "切换失败" / "Could not switch"

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test] fn plan_mode_chip_toggles_the_mode()       // click composer-plan-mode → eventually controls.config("mode").current == "plan" and chip selected; click again → "default"
#[gpui_kit::test] fn model_menu_switches_thinking_level()    // open composer-model, click "High" → current "high", chip shows "· High"
#[gpui_kit::test] fn context_ring_shows_percent_after_a_reply() // "hello" → composer-context shows "%"; click → popover shows "tokens"
#[gpui_kit::test] fn failed_switch_reverts_and_notifies()    // invoke set_config("model","nope/none") → 1 notification, current unchanged, chips enabled again
#[gpui_kit::test] fn draft_tab_controls_are_disabled()
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_controls`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_controls --test shell_composer`
Expected: 全部通过。

---

### Task 7: 停靠交互①：问题面板、计划和待办面板、等待状态

**Files:**
- Create: `crates/desktop/src/ui/question_panel.rs`、`crates/desktop/src/ui/plan_panel.rs`
- Modify: `crates/desktop/src/ui/session_view.rs`、`crates/desktop/src/ui/status.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_interactions.rs`（新增）

**Interfaces:**
- Consumes：Task 3 的 `open_questions()`、`todos()`、`approved_plan()`、`plan_open()`、`mark_settling`；Task 2 的 `answer_question` 和 `dismiss_questions`。
- Produces：
  - **`QuestionPanel`**（实体，归 `SessionView` 所有）：
    - 状态：当前题号；每道题的选择（多选时为一组）；「其他」文本，用 `InputState` 多行输入、2 行高；所有状态按 `QuestionKey` 存在面板里，切换会话 Tab 时保留。
    - 头部：「问题 · 第 i 题 / 共 n 题」，加上一题、下一题按钮（`Button::ghost().small()`）。标题用 Markdown 渲染；多选时显示「可多选」提示。
    - 选项：单选用 `Radio`，多选用 `Checkbox`。默认选中第一个选项，但要点「发送」才发出。最后有一个「其他」项。
    - 发送（`question-send`）：答案为空时禁用。多选的答案拼成 `"- a\n- b"`，自由文本放最后。先 `mark_settling`，然后调用 `answer_question`。返回 `QuestionClosed` 时推送通知 `Question.closed`，并清掉提交中标记；其他错误推送 `Question.send_failed`，并清掉提交中标记。
    - 跳过（`question-skip`）：调用 `dismiss_questions(当前题)`；失败时推送通知 `Question.skip_failed`。
    - 出现时不抢焦点。
  - **`PlanPanel`**：
    - 可折叠，标题「计划」，加「{完成数}/{总数}」。每条待办带图标：完成用 `check`，进行中用 `Spinner`，其余用 `circle`。下面嵌一个可折叠的「已批准的计划」，内容为 Markdown。
    - `plan_open()` 为 true 时默认展开；Run 结束后自动折叠，用户仍可手动展开。没有待办也没有计划时不渲染。
  - **等待状态**：`SessionStatus::Waiting` 已经由 Task 3 的 `status()` 提供。确认 Tab、侧栏和顶栏都显示 `circle-help` 和「等待回答」。
  - **locale**：
    - `Question.header`（含 `i`、`n`）
    - `Question.multi`：可多选
    - `Question.other`：其他
    - `Question.send`：发送
    - `Question.skip`：跳过
    - `Question.closed`：该问题已在其他地方回答或跳过
    - `Question.skip_failed`：未能跳过待回答的问题，请重试
    - `Question.send_failed`
    - `Plan.title`：计划
    - `Plan.approved`：已批准的计划

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test] fn question_panel_answers_with_the_chosen_option()
    // "ask" → panel shows "Pick one"; click option "b"; click question-send → eventually question 1 settled, panel shows "第 2 题"/"2 of 2" or next question
#[gpui_kit::test] fn multi_select_answer_is_a_markdown_list()   // hand-built multi_select question; pick a and b → prompt text "- a\n- b"（assert via projection user message text）
#[gpui_kit::test] fn skip_dismisses_the_question()               // question-skip → open_questions() shrinks
#[gpui_kit::test] fn background_question_marks_the_session_waiting()
    // two sessions; send "ask" in s1 then activate s2 → sidebar status for s1 == Waiting, no dialog in window; activate s1 → panel visible
#[gpui_kit::test] fn plan_panel_shows_todo_progress()            // "todo" → shows "Plan"/"计划" and "0/2"; after idle the panel is collapsed
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_interactions`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_interactions`
Expected: 全部通过。

---

### Task 8: 停靠交互②：计划审批、权限请求、MCP elicitation

**Files:**
- Create: `crates/desktop/src/ui/request_cards.rs`、`crates/desktop/src/ui/elicitation_form.rs`
- Modify: `crates/desktop/src/ui/session_view.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_interactions.rs`、`crates/desktop/tests/elicitation_form.rs`（新增）

**Interfaces:**
- Consumes：Task 3 的 `requests()`、`remove_request`；Task 2 的 `answer_permission` 和 `answer_elicitation`。
- Produces：
  - **卡片的排列**：`controls.requests()` 里的请求，按到达顺序在 composer 上方各停靠一张卡片，id 为 `request:{id}`。
  - **计划审批卡**：`subject` 是 `tool_call`、`kind == switch_mode` 的权限请求。
    - 标题「执行这个计划？」，下面是计划 Markdown（最大高度 `rems(20)`，内容可滚动）。
    - 按钮：「继续规划」（`request-reject`，次要按钮）和「批准」（`request-approve`，主按钮）。
    - 点击后调用 `answer_permission(id, Some(option_id))`，并在本地移除卡片。
  - **通用权限卡**：title、description，每个 option 一个按钮，reject 类排在前面。
  - **elicitation 卡**：
    - 文案「{serverId} 需要你填写信息」（没有 serverId 时用「MCP 需要你填写信息」），下面是 `message`。
    - form 模式有一个「填写…」按钮，打开 `Dialog`。
    - url 模式显示「将打开 {host}」，按钮为「打开并继续」：用 `cx.open_url` 打开链接，然后回复 accept，不带 content。
    - 两种模式都有「拒绝」（decline）和「取消」（cancel）按钮。
  - **elicitation 表单**：`pub struct ElicitationForm { fields: Vec<FormField> }`。
    - `TryFrom<&wire::ElicitationSchema>`（或者 `requestedSchema` 对应的类型，以 schema 1.19 为准）：必填字段无法渲染时返回 `Err(Unrenderable)`，非必填的无法渲染字段直接丢掉。
    - `FormField { name, label, description, required, kind: FieldKind }`，`FieldKind` 有 `Text { min, max, pattern, format }`、`Choice { options: Vec<(String, String)> }`、`Number { integer: bool, min, max }`、`Bool`、`MultiChoice { options, min_items, max_items }`。
    - `fn validate(&self, values: &FormValues) -> Result<BTreeMap<String, wire::ElicitationContentValue>, BTreeMap<String, FieldError>>`：必填为空时报「必填」；长度按 Unicode 码点算；数字以数字类型发送；为空的非必填字段不放进结果。
    - Dialog 里按字段类型分别用 `Input`、`Select`、`NumberInput`、`Checkbox`、多选 `Checkbox` 组渲染。按钮为「取消」（默认焦点，Esc 同效果）、「拒绝」、「提交」。
    - 校验失败时，在字段下方用 danger 色显示错误。
    - 无法渲染的表单：直接回复 cancel，并推送通知 `Request.form_unsupported`。
  - **撤回**：`RequestCancelled` 到达后，卡片立即消失；如果 Dialog 开着，同时关闭 Dialog。
  - **locale**：
    - `Request.plan_title`：执行这个计划？
    - `Request.approve`：批准
    - `Request.keep_planning`：继续规划
    - `Request.needs_input`（含 `server`）
    - `Request.fill`：填写…
    - `Request.open_continue`：打开并继续
    - `Request.will_open`（含 `host`）
    - `Request.decline`：拒绝
    - `Request.cancel`：取消
    - `Request.submit`：提交
    - `Request.required`：必填
    - `Request.form_unsupported`

- [ ] **Step 1: 写失败的测试**

```rust
// elicitation_form.rs（纯逻辑）
#[test] fn schema_fields_are_parsed()           // string(email), enum, integer(min 1), boolean, array of enum → five FieldKinds
#[test] fn unrenderable_required_field_fails()  // required field of type "object" → Err(Unrenderable); optional one → dropped
#[test] fn validation_reports_required_and_lengths() // required empty → "required"; maxLength 3 with "日本語x" → error (4 code points)
#[test] fn empty_optional_fields_are_omitted()
// shell_interactions.rs
#[gpui_kit::test] fn plan_approval_card_approves()        // mode plan + "plan" → find("request:..") visible with plan text; click request-approve → tool plan-1 Completed, card gone
#[gpui_kit::test] fn keep_planning_keeps_the_run_planning() // click request-reject → card gone, run continues (tool result keep-planning text)
#[gpui_kit::test] fn withdrawn_approval_card_disappears()   // press escape (stop) while card shown → RequestCancelled → card gone; no panic on late click
#[gpui_kit::test] fn elicitation_dialog_submits_values()    // push a synthetic AgentRequest::Elicitation into controls (form: name string required) → click Request.fill → dialog; submit empty → "Required" shown; input "x"; submit → card removed and controls.requests() empty（no test hooks in src: answer_elicitation on an unknown id is a no-op by design）
#[gpui_kit::test] fn escape_closes_the_dialog_before_stopping() // dialog open + running → escape closes dialog, run still Running
```

  测试里用的 elicitation 请求是直接构造后放进 `SessionControls` 的，因为 fixture 里没有 MCP 服务器能发起真实请求。真实的往返流程在 Task 11 用生产后端验收。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test elicitation_form --test shell_interactions`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test elicitation_form --test shell_interactions`
Expected: 全部通过。

---

### Task 9: Slash 命令列表

**Files:**
- Create: `crates/desktop/src/ui/command_palette.rs`
- Modify: `crates/desktop/src/ui/composer.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_palette.rs`（新增）

**Interfaces:**
- Consumes：Task 3 的 `commands()`。
- Produces：
  - **打开条件**：`CommandPalette` 挂在 `Composer` 上。输入内容以 `/` 开头、不含空白，并且没有被 Esc 关掉过时，在输入框上方显示命令列表（`popover` token），id 为 `command-palette`。
  - **过滤**：`pub fn filter<'a>(commands: &'a [SlashCommand], query: &str) -> Vec<&'a SlashCommand>`，按名字做大小写不敏感的前缀匹配，保持快照里的顺序。这是一个与上下文无关的纯算法，可以写成自由函数。
  - **键盘**：↑ / ↓ 循环移动选中项。Tab 或 Enter（不带 Shift，且不在输入法组字中）把输入框写成 `"/" + name + (有 hint ? " " : "")`，然后关闭列表，不发送。Esc 关闭列表，直到下一次编辑前不再打开。
  - **鼠标**：点击某一行和按 Enter 的效果相同，焦点留在输入框。
  - **行内容**：`/name`、等宽的 hint、来源徽标、muted 的描述；有 qualified name 时再加一行「完整名称 /{qualified}」。没有匹配项时显示「没有匹配的命令」。
  - **键位冲突**：列表开着时，composer 的 Enter 不发送，Esc 不停止运行。
  - **locale**：`Command.no_match` 没有匹配的命令；`Command.full_name`（含 `name`）。

- [ ] **Step 1: 写失败的测试**

```rust
#[test] fn filter_is_a_case_insensitive_prefix_match()      // ["compact","name","session"] query "SE" → ["session"]
#[gpui_kit::test] fn slash_opens_the_palette_and_tab_completes() // type "/co" → command-palette visible with "/compact"; press tab → value "/compact"; palette closed; nothing sent
#[gpui_kit::test] fn arrows_cycle_and_enter_picks()          // "/" → down twice → enter → value is the third command
#[gpui_kit::test] fn escape_closes_the_palette_only()        // running "slower" + "/" typed → escape → palette closed, run still Running
#[gpui_kit::test] fn whitespace_closes_the_palette()          // "/compact " → palette hidden
#[gpui_kit::test] fn no_matches_message()                     // "/zzz" → shows "No matching commands"
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_palette`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_palette --test shell_composer`
Expected: 全部通过。

---

### Task 10: 图片附件

**Files:**
- Create: `crates/desktop/src/ui/attachments.rs`
- Modify: `crates/desktop/src/ui/composer.rs`、`crates/desktop/src/ui/session_view.rs`、`crates/desktop/src/ui/drafts.rs`、`crates/desktop/Cargo.toml`（`image = { workspace = true }`）、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/attachments.rs`（新增）、`crates/desktop/tests/shell_composer.rs`

**Interfaces:**
- Produces：
  - **数据结构**：`pub struct ImageAttachment { pub name: String, pub mime: &'static str, pub data: Arc<[u8]> }`；`pub struct AttachmentSet { items: Vec<ImageAttachment> }`。
  - **`AttachmentSet::add(&mut self, name: String, bytes: Vec<u8>) -> Result<(), AttachmentError>`**：
    - 用 `image::guess_format` 判断格式，只接受 png、jpeg、gif、webp，否则返回 `UnsupportedFormat`。
    - 单张超过 10 MiB 返回 `TooLarge`。
    - 已有 5 张时返回 `TooMany`。
    - 加上这张后总计超过 20 MiB 返回 `TotalTooLarge`。
  - **其他方法**：`remove(index)`、`is_empty()`；`into_blocks(self) -> Vec<wire::ContentBlock>` 输出标准 base64 编码的 `Image` 块。
  - **三个入口**，都在后台 executor 上读文件，读完回到 UI 线程调用 `add`；读取期间发送按钮禁用：
    - 附件按钮 `composer-attach`：`cx.prompt_for_paths`，允许多选。
    - 粘贴：composer 处理粘贴动作时，读剪贴板里的 `ClipboardEntry::Image`（可能有多张）和 `ExternalPaths`；同时含文字的剪贴板，文字照常交给输入框处理。
    - 拖放：会话卡片内 `on_drop::<ExternalPaths>`，拖动经过时显示 `drag_over` 高亮。
  - **附件条**：在输入框上方，每张图用 gpui-kit `Attachment` 显示缩略图、名字和删除按钮（`attachment-remove-{i}`）。
  - **发送**：发送时，内容块为文字（非空时）加上所有图片。失败的处理和 M1b 一样，图片和文字都放回去；被取消时也一样。草稿 Tab 首次发送创建会话时，同样带上图片。
  - **出错提示**：推送通知，文案为 `Attachment.unsupported` / `too_large` / `too_many` / `total_too_large` / `read_failed`，中文分别是「不支持的图片格式」「图片超过 10 MB」「最多 5 张图片」「图片总大小超过 20 MB」「无法读取文件」。
  - **ponytail**：不提示模型不支持图片。客户端拿不到模型的输入模态，服务端会对纯文本模型自动降级成占位符。等配置项带上模态信息再加。

- [ ] **Step 1: 写失败的测试**

```rust
// attachments.rs（纯逻辑，用内存里的最小 PNG/BMP 字节）
#[test] fn png_is_accepted_and_bmp_rejected()
#[test] fn sixth_image_is_rejected()
#[test] fn oversize_and_total_limits()          // 10 MiB + 1 → TooLarge; 3 × 7 MiB → third TotalTooLarge
#[test] fn blocks_are_standard_base64_images()
// shell_composer.rs
#[gpui_kit::test] fn attached_image_is_sent_with_the_text()   // add via AttachmentSet hook on Composer (pub method add_attachment); enter → user row has 1 image
#[gpui_kit::test] fn pasted_image_becomes_an_attachment()     // write ClipboardItem with an Image entry; dispatch paste → attachment chip visible
#[gpui_kit::test] fn removing_an_attachment()                 // click attachment-remove-0 → no chip
#[gpui_kit::test] fn rejected_file_notifies_and_sends_nothing() // add BMP bytes → 1 notification, no chip
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test attachments --test shell_composer`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test attachments --test shell_composer`
Expected: 全部通过。

---

### Task 11: 验收和全量验证

**Files:** 只修复验收中发现的问题，不新增功能。

- [ ] **Step 1: 静态检查和测试**

Run: `cargo fmt --all && cargo clippy --workspace --tests --examples -- -Dwarnings && cargo test -p desktop -p acp -p app && pre-commit run --all-files`
Expected: 全部通过，没有 warning。

- [ ] **Step 2: 真机运行**

用生产 `app` 后端和已配置的 provider，按 M1b 验收的做法：先备份 `desktop.json`，结束后原样恢复，不打印密钥，用 `/tmp` 下的临时工作区。依次跑下面这些场景：
1. 切到计划模式，让 agent 给出计划，在卡片上点「继续规划」一次、「批准」一次。
2. 让 agent 调用 `request_user_input_async` 提问，单选回答一题、跳过一题。
3. 在后台会话里触发一个问题，确认侧栏显示 `circle-help`，切回去后卡片还在。
4. 输入 `/` 打开命令列表，选 `/compact` 并发送，确认压缩 pill 出现、上下文环归为「--」后又有了数值。
5. 切换模型和思考级别。
6. 粘贴一张截图和拖入一张 png 一起发送；再试一次拖入 bmp，确认被拒绝。
7. 如果本机配置了带 elicitation 的 MCP 服务器，跑一次表单；没有就记为「未验证，靠 Task 8 的测试覆盖」。

- [ ] **Step 3: 对照设计稿截图**

截图覆盖以下组合：
- Default Light 加中文
- Catppuccin Mocha 加英文
- 最小窗口
- 字号 18

和 mockup 并排对比以下区域：
- composer 底栏（计划模式、上下文环、模型 chip）
- 交互卡片
- 压缩 pill
- 用量行

逐项检查：
- 圆角档位
- hairline
- 间距
- 字号层级
- muted 色
- 图标尺寸

发现偏差当场修。

- [ ] **Step 4: 自查**

对照 spec 第 15 节 M2 的范围，逐条检查本计划的 Global Constraints 和 Review Focus。结论写进本计划对应的 SDD 账本。

- [ ] **Step 5: 等用户明确要求后再提交**

按 `.gitmessage` 的格式提交，代码、本计划和协议参考放在同一个提交里：

```bash
git add -A crates/desktop docs/superpowers/plans/2026-11-20-desktop-m2-interactions-and-controls.md docs/superpowers/plans/2026-11-20-desktop-m2-protocol-reference.md
git commit -m "feat(desktop): add interactions, session controls and image attachments"
```

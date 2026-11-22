# Desktop M4a：轨迹视图 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **修订（实现后）：** 轨迹搜索已移除（工具栏放不下），设计文档同步删除；下文中搜索框、`set_search`、搜索测试等要求不再适用，不要补回。「加载全部」按钮保留。

**Goal:** 在桌面端主卡片加「对话 / 轨迹」切换，逐轮、逐次请求地展示 agent 循环：每次请求实际发出的系统提示词、工具和选项，计时与用量，工具调用，以及时间总览和记录详情。

**Architecture:**
- **数据来源（JSONL）：** 每次 agent 循环的助手尝试在元数据里带一份请求快照（提示词和工具清单的内容哈希、请求选项、消息条数）；同一会话里新出现的提示词或工具清单写一条 `request_content` 记录。
- **查询层（SQLite 历史库）：** 镜像全部过程记录和请求内容；`state` 库把当前分支整理成带类型的轨迹记录并分页返回。
- **传输：** 内核经 `SessionCatalog` 转调，ACP 提供 `_clawcode/session/trajectory` 和 `_clawcode/session/trajectory/content` 两个只读方法。
- **桌面端：**
  - 纯数据模块 `trajectory/`：分页合并、可见行、折叠、区间、时间总览几何；
  - 界面模块 `ui/trajectory/`：视图、工具栏、记录表、时间总览画布；
  - 右侧面板新增 `ui/panel/record.rs`：记录详情。

**Tech Stack:** 与 M3b 相同。
- 新用到的 gpui-kit 组件：`Table` / `TableDelegate` / `TableState`（`gpui_kit::component::table`）、`TabBar` 的 `Segmented` 变体、`canvas` 自绘。
- 已有依赖：`sha2`（protocol 已依赖）、`similar`（工作区依赖，桌面端新加引用）、`sqlx`。

**Spec:** [Desktop 轨迹视图（M4a）设计](../specs/2026-11-21-desktop-trajectory-design.md)。前序计划见 [M3b](./2026-11-21-desktop-m3b-shell-redesign.md)。

## Global Constraints

- AGENTS.md 规则 1–16 全部适用。本计划特别要求：
  - **规则 5：** 超过 3 个字段的结构体用 `typed-builder` 构造，`Option` 字段加 `#[builder(default)]`。
  - **规则 6：** 克隆 `Arc` 写 `Arc::clone(&x)`。
  - **规则 8：** 单参数且参数是项目类型的帮助函数，写成该类型的方法或 trait 实现。
  - **规则 15：** 共享上下文的操作做成结构体的方法。本计划指定的上下文结构体有：`BranchIndex`、`TrajectoryAssembler`、`TrajectoryModel`、`TimelineLayout`、`Viewport`。不要把同一组参数在一串自由函数之间传递。
  - **规则 10：** 测试只在各库的 `tests/` 目录。不为测试增加生产代码；测试只用公开 API。
  - **规则 13、14：** 日志只记生命周期、重试、错误；用 `tracing::info!` 这类全限定宏和普通格式参数；两个新 ACP 方法是查询，INFO 不记参数。
- 界面用主题 token 和 rem。px 只用于 hairline 和画布内的像素计算，写注释说明原因。图标用 Lucide，缺的加进 `assets.rs` 的 `ExtraIcons`。
- 新文案写进 `crates/desktop/locales/app.yml`，en 和 zh-CN 都要有。
- clippy `-Dwarnings`；`cargo fmt`。
- **术语：**
  - clawcode 的 turn = 一次模型请求，界面显示「请求 #N」；
  - 运行（run）= 一个完整的 agent 循环，界面显示「第 N 轮」；
  - 运行之外的条目归在「运行之间」。
- **不考虑历史数据兼容（设计第 2 节第 4 条）：** 不为旧会话补快照，也不做降级展示。
- **不按任务提交。** 全部完成、用户明确要求后，一个提交，包含设计文档和本计划。
- **不做（设计第 10 节）：**
  - 父会话轨迹里嵌套子 agent；
  - 跨会话搜索；
  - WebUI；
  - 逐字显示生成中的输出；
  - 导出和回放。
- **常量：**

  | 名称 | 值 |
  | --- | --- |
  | 分页默认条目数 | 80 |
  | 分页上限 | 200 |
  | 补取合并窗口 | 150 ms |
  | 时间总览高度 | 6 rem |
  | 悬停浮层延迟 | 500 ms |
  | 工具泳道子行上限 | 3 |
  | 横条合并阈值 | 1 px |
  | 进行中横条刷新 | 1 s |
  | 快捷键 | `Ctrl+Shift+T` / `Cmd+Shift+T` |

## Review Focus

1. **带图片的消息：** 用户消息或工具结果里有 base64 图片，分页结果只能带数量，不能带图片数据，否则一页可能有几十 MB。测试在 Task 4（`page_counts_images_without_their_data`）。
2. **查看轨迹时切换了分支：** 已加载的是旧分支，补取时服务端返回新的 `leaf_id`。客户端要整体重新加载，不能把两条分支的记录拼在一起。测试在 Task 4（`after_query_reports_the_new_leaf`）和 Task 7（`a_new_leaf_replaces_every_record`）。
3. **失败后重试：** 第一次请求失败（留下带 `error` 的助手条目），重试成功。两次应显示为请求 #1 和 #2，#1 是错误状态，#2 和 #1 比较提示词时显示「未变化」。测试在 Task 5（`failed_attempt_and_retry_are_two_requests`）。
4. **fork 出来的会话：** fork 只复制分支上的条目。如果不同时复制 `request_content` 记录，新会话的快照就指向找不到的内容。测试在 Task 2（`fork_copies_request_contents`）。
5. **历史库还在回填或修复：** 打开轨迹时历史库不可读，界面要显示原因和「重试」，不能空白或卡在骨架。测试在 Task 6（`trajectory_reports_history_unavailable`）和 Task 9（`unavailable_history_offers_retry`）。

---

### Task 1：protocol——请求快照与轨迹类型

**Files:**
- Create: `crates/protocol/src/trajectory.rs`
- Modify: `crates/protocol/src/lib.rs`（`mod trajectory; pub use trajectory::*;`）、`crates/protocol/src/message.rs`（`AssistantMetadata`）、`crates/protocol/src/model.rs`（`ModelRequest::capture`）、`crates/protocol/src/identity.rs`（两个方法）
- Test: `crates/protocol/tests/trajectory.rs`

**Interfaces:**
- Produces（全部 `Serialize`/`Deserialize`，`#[serde(rename_all = "camelCase")]`；枚举用 `#[serde(tag = "type", rename_all = "snake_case")]`）：
  - `pub struct ContentHash(String)`：64 位小写十六进制。
    - `ContentHash::of(bytes: &[u8]) -> Self`（SHA-256）；
    - `as_str(&self) -> &str`；
    - `TryFrom<String>`，长度或字符不合法时报错；
    - `Display`。
  - `pub enum RequestContentKind { SystemPrompt, Tools }`
  - `pub struct RequestContent { pub hash: ContentHash, pub kind: RequestContentKind, pub content: String }`
  - `pub struct RequestOptions`（TypedBuilder）：`provider_id: String`、`model_id: String`、`thinking_level: ThinkingLevel`、`max_tokens: Option<u64>`、`temperature: Option<f64>`、`prompt_cache: Option<PromptCacheRetention>`。
  - `pub struct RequestSnapshot`（TypedBuilder）：`system_prompt: ContentHash`、`tools: ContentHash`、`options: RequestOptions`、`message_count: u32`。
  - `pub struct RequestCapture { pub snapshot: RequestSnapshot, pub contents: [RequestContent; 2] }`。
  - `impl ModelRequest { pub fn capture(&self, profile: &ModelProfile) -> Result<RequestCapture, serde_json::Error> }`：
    - **系统提示词：** 所有 `MessageContent::System` 消息的文本块按顺序用 `\n` 连接；
    - **工具：** `serde_json::to_string(&self.tools)`；
    - **消息条数：** 非系统消息的条数；
    - **选项：** 提供方和模型取自 `profile`，其余取自 `self.options`，缓存只取 `retention`。
  - `AssistantMetadata` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] #[builder(default)] pub request: Option<RequestSnapshot>`。
  - `pub enum TrajectoryCursor { Tail, Before(u64), After(u64) }`。
  - `pub struct TrajectoryParameters { session_id: SessionId, before: Option<u64>, after: Option<u64>, limit: Option<u32> }`：
    - `deny_unknown_fields`；
    - `TrajectoryParameters::cursor(&self) -> Result<TrajectoryCursor, TrajectoryCursorError>`，`before` 和 `after` 同时给出时报错。
  - `pub struct TrajectoryContentParameters { session_id: SessionId, hash: ContentHash }`。
  - `pub struct RunRef { pub id: RunId, pub index: u32 }`。
  - `pub struct TrajectoryRecord`（TypedBuilder）：`id: String`、`sequence: u64`、`run: Option<RunRef>`、`turn_id: Option<TurnId>`、`started_at_ms: Option<TimestampMs>`、`ended_at_ms: Option<TimestampMs>`、`kind: TrajectoryRecordKind`。
  - `pub enum TrajectoryRecordKind`：
    - `User(UserRecord)`、`Command(CommandRecord)`、`Request(RequestRecord)`、`Assistant(AssistantRecord)`、`Tool(ToolRecord)`；
    - `Compaction { summary: String, tokens_before: Option<u64> }`、`BranchSummary { summary: String }`；
    - `Setting(SettingChange)`、`Extension(ExtensionRecord)`。
  - 各记录结构体（超过 3 个字段的用 TypedBuilder）：
    - `UserRecord { text, image_count: u32, file_count: u32, origin: UserOrigin }`，其中 `UserOrigin { Input, Steering, FollowUp, Skill }`；
    - `CommandRecord { kind: CommandKind { Slash, Bash }, text }`；
    - `RequestRecord { index: u32, snapshot: RequestSnapshot, first: bool, prompt_changed: bool, tools_added: Vec<String>, tools_removed: Vec<String> }`；
    - `AssistantRecord { request_index: u32, provider_id, model_id, reasoning: String, text: String, tool_call_ids: Vec<ToolCallId>, usage: ModelUsage, requested_at_ms: TimestampMs, first_token_at_ms: TimestampMs, last_token_at_ms: TimestampMs, stop_reason: StopReason, error: Option<String>, cancelled: bool }`；
    - `ToolRecord { call_id: ToolCallId, name: String, arguments: serde_json::Value, result_text: Option<String>, image_count: u32, is_error: bool, cancelled: bool }`，`result_text = None` 表示还没有结果；
    - `SettingChange { Model { provider: String, model: String }, ThinkingLevel { level: ThinkingLevel }, ActiveTools { tools: Vec<String> } }`；
    - `ExtensionRecord { extension_id: String, custom_type: String, text: String }`。
  - `pub struct TrajectoryPage`（TypedBuilder）：`records: Vec<TrajectoryRecord>`、`leaf_id: Option<EntryId>`、`has_more_before: bool`、`has_more_after: bool`、`first_sequence: Option<u64>`、`last_sequence: Option<u64>`、`usage_before: ModelUsage`。
  - `AcpExtensionMethod::Trajectory`（`"_clawcode/session/trajectory"`）和 `AcpExtensionMethod::TrajectoryContent`（`"_clawcode/session/trajectory/content"`）：加入 `as_str`、`parse`，`is_query` 都返回 `true`。

- [ ] **Step 1：写失败的测试**（`crates/protocol/tests/trajectory.rs`）
  - `content_hash_is_sha256_hex`：`ContentHash::of(b"abc").as_str() == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"`；`ContentHash::try_from("xyz".to_string())` 是错误。
  - `capture_hashes_what_the_request_sends`：
    - 构造 `ModelRequest`：一条 System（两个文本块 `"a"`、`"b"`）、一条 User、一个工具 `read`，选项 `ThinkingLevel::High`、缓存保留 `Short`、缓存键 `"k"`；
    - 断言系统提示词哈希等于 `ContentHash::of(b"a\nb")`；
    - 断言 `snapshot.message_count == 1`、`options.model_id == profile.model_id`、`options.prompt_cache == Some(PromptCacheRetention::Short)`；
    - 断言 `contents[1].kind == RequestContentKind::Tools`，且 `contents[1].content` 能解析回同一个 `Vec<ToolDefinition>`。
  - `capture_changes_only_with_the_tools`：同一请求只改工具描述，`tools` 哈希变化，`system_prompt` 哈希不变。
  - `assistant_metadata_without_request_round_trips`：不带 `request` 的元数据序列化后不出现 `request` 键，反序列化得到 `None`。
  - `trajectory_parameters_reject_two_cursors`：`{"sessionId":"s","before":3,"after":5}` 的 `cursor()` 报错；`{"sessionId":"s"}` 得到 `Tail`；带未知字段时反序列化失败。
  - `trajectory_record_round_trips`：一个 `Tool` 记录和一个 `Request` 记录序列化、反序列化后相等；JSON 里类型标签为 `"type":"tool"`。
  - `trajectory_methods_are_queries`：两个新方法 `parse(as_str())` 往返一致，`is_query()` 为真。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p protocol --test trajectory`
  - Expected：编译失败，缺少 `ContentHash`、`ModelRequest::capture` 等。
- [ ] **Step 3：实现**
  - 类型按 Interfaces 写进 `trajectory.rs`；`capture` 写在 `model.rs` 的 `impl ModelRequest`。
  - 所有构造 `AssistantMetadata` 的地方（kernel、acp、mcp、state 的测试支持等，`grep -rn "AssistantMetadata::builder" crates`）都不用改，`#[builder(default)]` 给出 `None`。
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p protocol && cargo check --workspace --all-targets`
  - Expected：PASS，工作区编译通过。

### Task 2：store 与 kernel——写入请求快照和内容记录

**Files:**
- Modify: `crates/store/src/model.rs`（`RecordKind::RequestContent`，`as_str` 等匹配）、`crates/store/src/factory.rs`（`fork` 复制内容记录）
- Modify: `crates/kernel/src/runtime/session/transcript.rs`（哈希集合、追加内容记录）、`crates/kernel/src/runtime/run/turn.rs`（`stream_assistant`）、`crates/kernel/src/runtime/run/attempt.rs`（携带快照）、`crates/kernel/src/runtime/run/queue.rs`（新记录类型在恢复时忽略）
- Test: `crates/store/tests/fork.rs`（若无则新建）、`crates/kernel/tests/request_snapshot.rs`

**Interfaces:**
- Consumes：Task 1 的 `ModelRequest::capture`、`RequestCapture`、`RequestContent`、`AssistantMetadata.request`。
- Produces：
  - `RecordKind::RequestContent`，序列化为 `"request_content"`，载荷是 `RequestContent` 的 JSON。
  - `SessionTranscriptState` 增加 `request_hashes: HashSet<ContentHash>`，由 `SessionTranscript::restored` 根据已有的 `RequestContent` 记录初始化。
  - `SessionTranscript::append_request_contents(&self, run_id: &RunId, contents: &[RequestContent], next_id: impl FnMut() -> Result<RecordId, KernelError>) -> Result<(), KernelError>`：
    - 在一次加锁内，跳过已在集合中的哈希；
    - 其余逐个追加 `NewRecord { kind: RecordKind::RequestContent, run_id: Some(..), lane: 本分支线 }` 并加入集合。
  - `AssistantAttempt` 增加 `#[builder(default)] request: Option<RequestSnapshot>` 和 `with_request(self, snapshot: RequestSnapshot) -> Self`；`snapshot()` 和 `settle()` 构造元数据时带上它。
  - `store::JsonlStoreFactory::fork`：复制分支条目之后，把源会话中所有 `RecordKind::RequestContent` 记录按原顺序追加到新会话（`run_id: None`，分支线为目标分支线）。

- [ ] **Step 1：写失败的测试**
  - `crates/store/tests/fork.rs::fork_copies_request_contents`：
    - 源会话追加两条 `RequestContent` 记录和若干条目，fork 后；
    - 断言新会话的 `records()` 里正好有这两条内容记录，载荷相同。
  - `crates/kernel/tests/request_snapshot.rs`（用 `turn.rs` 里的 `ScriptedModel`、`SystemPromptCaptureModel` 写法，按需复制到本文件）：
    - `every_attempt_carries_the_request_it_sent`：
      - 一次带工具调用的运行，产生两次请求；
      - 两条助手消息的 `metadata.request` 都是 `Some`；
      - 每条的 `system_prompt` 和 `tools` 分别等于捕获模型收到的请求调用 `capture(profile)` 得到的哈希，以此证明快照取在扩展改写和 `adapt_input` 之后。
    - `request_contents_are_written_once_before_the_assistant`：
      - 读回会话日志：`request_content` 记录正好 2 条（提示词、工具各一条，第二次请求复用）；
      - 两条的顺序号都小于第一条助手条目。
    - `reopened_session_does_not_rewrite_contents`：关闭会话、重新打开、再运行一次，提示词和工具都没变，`request_content` 记录仍为 2 条。
    - `retry_attempts_each_carry_a_snapshot`：用 `retry.rs` 的可重试失败脚本，第一次失败、第二次成功；两条助手条目（失败的那条带 `error`）都有 `request`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p store --test fork && cargo test -p kernel --test request_snapshot`
  - Expected：编译失败或断言失败（缺少 `RequestContent`、`request` 为 `None`）。
- [ ] **Step 3：实现**
  - 在 `stream_assistant` 开头、创建 `AssistantAttempt` 之前：
    - 调用 `request.capture(turn.model.profile())?`（错误转 `KernelError`）；
    - 调用 `self.session.transcript.append_request_contents(self.run_id, &capture.contents, || kernel.record_id())`，记录 id 的生成与 `Kernel::record_operation` 相同，可以抽出 `Kernel::record_id(&self) -> Result<RecordId, KernelError>` 让两处共用；
    - 然后 `AssistantAttempt::new(..).with_request(capture.snapshot)`。
  - `queue.rs` 等匹配 `RecordKind` 的地方，把 `RequestContent` 归入「忽略」分支。
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p store && cargo test -p kernel`
  - Expected：全部 PASS。

### Task 3：state——镜像过程记录和请求内容

**Files:**
- Create: `crates/state/migrations/history/0003_trajectory.sql`（表结构见设计 3.2，原样照抄）
- Modify: `crates/state/src/projector/history_sink.rs`
- Modify: `crates/state/tests/support/mod.rs`（`SessionLog` 增加帮助方法）
- Test: `crates/state/tests/projection.rs`

**Interfaces:**
- Consumes：Task 2 的 `RecordKind::RequestContent`。
- Produces：
  - `HistoryLine::apply` 对每条 `LogMutation::Record` 先调用新的 `insert_record`，写入 `records` 表（`ON CONFLICT DO NOTHING`），再按类型分派：运行的开始和结束保持原有处理；`RequestContent` 另外调用 `insert_request_content`，写入 `request_contents`。
  - 测试支持 `SessionLog` 新增：
    - `request_content(&mut self, run_id: &str, content: &RequestContent)`；
    - `queue_enqueued(&mut self, run_id: &str, entry: &EntryId, target: &str)`；
    - `assistant_with(&mut self, ...)`：带 `request`、`usage`、计时、`error` 的助手条目，参数用一个本地 TypedBuilder 结构体 `AssistantFixture` 传入。

- [ ] **Step 1：写失败的测试**（`projection.rs`）
  - `records_are_mirrored_verbatim`：日志含运行开始、`step_attempt`、`queue_enqueued`、运行结束。投影后 `records` 表有 4 行，`kind` 依次为 `operation_started`、`step_attempt`、`queue_enqueued`、`operation_finished`，`record_json` 能反序列化回原记录。
  - `request_contents_are_addressed_by_hash`：两条 `request_content`（哈希不同），投影后 `request_contents` 有 2 行，`byte_len` 等于内容的字节数。
  - `rebuild_restores_trajectory_tables`：删除历史库后，通过现有的重建入口（与已有重建测试相同）重建，两张表的行数恢复。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p state --test projection`
  - Expected：FAIL，`no such table: records`。
- [ ] **Step 3：实现** 迁移和两个插入方法，SQL 写成模块常量，与 `OPEN_RUN_SQL` 的写法一致。
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p state`
  - Expected：全部 PASS。

### Task 4：state——当前分支、分页和记录整理

**Files:**
- Create:
  - `crates/state/src/history/trajectory.rs`：`StateRuntime::trajectory` 入口和常量；
  - `crates/state/src/history/trajectory/branch.rs`：`BranchIndex`；
  - `crates/state/src/history/trajectory/assemble.rs`：`TrajectoryAssembler`。
- Modify: `crates/state/src/history.rs`（声明 `mod trajectory;`）
- Test: `crates/state/tests/trajectory.rs`

**Interfaces:**
- Consumes：Task 1 的轨迹类型、Task 3 的表和测试支持。
- Produces：
  - `impl StateRuntime { pub async fn trajectory(&self, id: &SessionId, cursor: TrajectoryCursor, limit: Option<u32>) -> Result<Option<TrajectoryPage>, StateError> }`。
    - 与 `tree_snapshot` 相同：先调用 `caught_up(id)`，会话不存在时返回 `Ok(None)`。
    - `limit` 默认 80，上限 200。
  - `pub(super) struct BranchIndex { entries: Vec<BranchEntry> }`，`BranchEntry { id: EntryId, sequence: u64, message_type: Option<String>, run_id: Option<RunId> }`：
    - `BranchIndex::load(conn, session, leaf) -> Result<Self, StateError>`：递归 CTE，只查这四列；
    - `fn window(&self, cursor: TrajectoryCursor, limit: usize) -> PageWindow`，`PageWindow { range: Range<usize>, has_more_before: bool, has_more_after: bool }`；
    - `fn request_index(&self, position: usize) -> u32`：到该位置为止（含）的助手条目数；
    - `fn run_index(&self, run: &RunId) -> Option<u32>`：运行在分支上第一次出现的顺序，从 1 开始。
  - **分页边界算法（`window`）：**
    - `Tail`：`end = len`，`start = end.saturating_sub(limit)`；
    - `Before(seq)`：`end` 是第一个 `sequence >= seq` 的位置；
    - `After(seq)`：`start` 是第一个 `sequence > seq` 的位置，`end = min(len, start + limit)`；
    - 然后：`while start > 0 && entries[start] 是工具结果 { start -= 1 }`；`while end < len && entries[end] 是工具结果 { end += 1 }`。
  - `pub(super) struct TrajectoryAssembler<'a>`（TypedBuilder）：`branch: &'a BranchIndex`、`window_start: usize`、`origins: &'a HashMap<EntryId, UserOrigin>`。
    - `fn assemble(&self, entries: Vec<SessionEntry>) -> Result<Vec<TrajectoryRecord>, StateError>`；
    - 本任务先输出 `Request` 的 `first = request_index == 1`、`prompt_changed = false`、`tools_added` 和 `tools_removed` 为空，比较留给 Task 5。
  - **条目 → 记录：**
    - 用户 → `User`（`ExpandedUser` 的 `origin = Skill`，其余默认 `Input`，Task 5 覆盖排队标记）；
    - `SlashCommand` / `BashExecution` → `Command`；
    - 助手 → `Request` 加 `Assistant`，`id` 分别为 `request:{entry_id}` 和条目 id；`Assistant` 的 `started_at_ms` 是请求发出时刻，`ended_at_ms` 是末 token；
    - 助手里的每个工具调用块 → `Tool`，`id` 为 `tool:{call_id}`，`result_text = None`；随后遇到对应的工具结果条目时填入结果、`is_error`、`cancelled`，并以结果的开始和结束时间作为计时；
    - 压缩、分支摘要、模型 / 思考等级 / 工具启用的变化、扩展消息、`custom` 条目里有可显示文本的，按对应类型输出；
    - 其他条目跳过。
  - **图片：** 文本中只保留文本块；图片块只计数。

- [ ] **Step 1：写失败的测试**（`trajectory.rs`，用 `SessionLog` 构造日志）
  - `tail_page_follows_the_active_branch`：
    - 主分支上 5 条，从第 2 条分出旁支并把末端移过去；
    - 结果只含根到新末端的记录，`leaf_id` 是新末端，`has_more_before == false`。
  - `requests_are_numbered_across_the_branch`：两次运行共 3 次请求；`Request` 的 `index` 依次为 1、2、3；`run.index` 依次为 1、1、2。
  - `page_never_starts_with_a_tool_result`：
    - 构造「助手（2 个调用）、结果、结果、助手」，用 `limit = 2` 取末页；
    - 页的第一条记录是 `Request`，两个 `Tool` 都有 `result_text`。
  - `before_and_after_cursors_page_the_branch`：
    - 共 12 条，`limit = 5`；
    - `Tail`、`Before(first)`、`Before(first)` 依次取完；记录无重复，按顺序号拼起来等于全部；最后一页 `has_more_before == false`；
    - `After(last)` 在追加两条后正好返回它们，`has_more_after == false`。
  - `after_query_reports_the_new_leaf`：导航到另一分支后，用旧的 `last_sequence` 查 `After`，返回的 `leaf_id` 等于新末端。
  - `page_counts_images_without_their_data`：用户消息含一段 100 KiB 的 base64 图片；`image_count == 1`，整页 `serde_json::to_string` 不包含该 base64 字符串。
  - `assistant_record_carries_timing_and_usage`：
    - 助手条目的 `timestamp_ms`、`started_at_ms`、`ended_at_ms` 为 1000、1800、2600；
    - `requested_at_ms == 1000`、`first_token_at_ms == 1800`、`last_token_at_ms == 2600`；
    - 输出 token 和缓存读取 token 等于条目里的值。
  - `entries_outside_runs_have_no_run`：运行开始前写入的扩展消息 `run == None`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p state --test trajectory`
  - Expected：编译失败，缺少 `StateRuntime::trajectory`。
- [ ] **Step 3：实现**：按 Interfaces 实现。`trajectory()` 依次做：取末端、`BranchIndex::load`、`window`、按区间取 `entry_json`（`WHERE entry_id IN (SELECT value FROM json_each(?))` 再按 `sequence` 排序）、`TrajectoryAssembler::assemble`。
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p state --test trajectory`
  - Expected：PASS。

### Task 5：state——请求比较、排队标记、累计用量、内容读取

**Files:**
- Modify: `crates/state/src/history/trajectory.rs`、`trajectory/assemble.rs`
- Test: `crates/state/tests/trajectory.rs`

**Interfaces:**
- Consumes：Task 4。
- Produces：
  - `TrajectoryAssembler` 增加字段：
    - `previous: Option<RequestSnapshot>`：本页之前分支上最后一次请求的快照，用 `BranchIndex` 找到位置后单独读这一条；
    - `tool_names: &'a HashMap<ContentHash, Vec<String>>`：本页所有快照和 `previous` 涉及的工具哈希，一次查询取出并解析出名称。
  - 比较规则：
    - `prompt_changed` 等于本次与上次 `system_prompt` 哈希不同；
    - 工具哈希不同时，按名称算出增减并排序；
    - 第一次请求 `first = true`，不比较。
  - **排队标记：** 从 `records` 表取 `kind = 'queue_enqueued'` 的记录，读载荷的 `entryId` 和 `target`：`steering` → `Steering`；`follow_up` 和 `next_run` → `FollowUp`。结果作为 `origins` 传入。
  - **`usage_before`：** 窗口之前分支上所有助手条目用量的和，用 `ModelUsage` 的 `+=` 累加；SQL 只取 `json_extract(entry_json, '$.content.metadata.usage')`，再反序列化为 `ModelUsage`。
  - `impl StateRuntime { pub async fn request_content(&self, id: &SessionId, hash: &ContentHash) -> Result<Option<RequestContent>, StateError> }`：会话或哈希不存在都返回 `Ok(None)`。

- [ ] **Step 1：写失败的测试**
  - `prompt_and_tool_changes_are_reported`：
    - 三次请求：第二次换了提示词，第三次工具从 `[read, write]` 变为 `[read, grep]`；
    - #1 的 `first == true`；#2 的 `prompt_changed == true` 且工具无增减；#3 的 `tools_added == ["grep"]`、`tools_removed == ["write"]`。
  - `comparison_crosses_the_page_boundary`：用 `limit` 让 #3 成为页的第一个请求，结果与上一条测试一致。
  - `queued_messages_are_marked`：两条用户消息分别被 `queue_enqueued` 标为 `steering` 和 `follow_up`，`origin` 分别为 `Steering` 和 `FollowUp`；普通输入为 `Input`。
  - `failed_attempt_and_retry_are_two_requests`：
    - 第一条助手条目带 `error`，第二条正常，快照相同；
    - `Request` #1、#2；`Assistant` #1 的 `error` 有值；#2 的 `prompt_changed == false` 且工具无增减。
  - `usage_before_sums_earlier_requests`：5 次请求，每次输入 100、输出 10；`limit` 让页从第 4 次开始；`usage_before` 的输入为 300、输出为 30。
  - `request_content_reads_by_hash`：存在的哈希返回内容和类型；不存在的返回 `None`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p state --test trajectory`
  - Expected：新测试 FAIL。
- [ ] **Step 3：实现**（按 Interfaces）
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p state`
  - Expected：PASS。

### Task 6：kernel、ACP 与桌面客户端

**Files:**
- Modify: `crates/kernel/src/runtime.rs`（`KernelError::HistoryUnavailable`、`Kernel` 的两个方法）、`crates/kernel/src/runtime/session/catalog.rs`
- Modify: `crates/acp/src/extension.rs`、`crates/acp/src/server.rs`（注册两个方法，同 `WorkspaceGitDiff`）
- Modify: `crates/desktop/src/backend/client.rs`、`crates/desktop/src/backend/error.rs`
- Test: `crates/acp/tests/extension.rs`、`crates/desktop/tests/backend.rs`

**Interfaces:**
- Consumes：Task 4、5 的 `StateRuntime::trajectory`、`request_content`。
- Produces：
  - `KernelError::HistoryUnavailable(SessionId)`，消息为 `"session history is unavailable for {0}"`。
  - `SessionCatalog::trajectory(&self, id: &SessionId, cursor: TrajectoryCursor, limit: Option<u32>) -> Result<TrajectoryPage, KernelError>` 和 `SessionCatalog::request_content(&self, id: &SessionId, hash: &ContentHash) -> Result<Option<RequestContent>, KernelError>`：
    - `state_for_read()` 为 `None` 时返回 `HistoryUnavailable`；
    - `trajectory` 得到 `None`（会话未知）时返回现有的 `KernelError::SessionNotFound`。
  - `Kernel::session_trajectory(...)`、`Kernel::session_request_content(...)`，参数同上，直接转调 `self.catalog`。
  - ACP：
    - `Trajectory` 解析 `TrajectoryParameters`；`cursor()` 出错时返回 `invalid_parameters`；
    - `TrajectoryContent` 解析 `TrajectoryContentParameters`，返回 `{"kind": .., "content": ..}`，不存在时返回 `null`；
    - `HistoryUnavailable` 映射为 `INTERNAL_ERROR` 加 `data: {"reason": "historyUnavailable"}`。
  - 桌面端：
    - `BackendError::HistoryUnavailable`：`From<agent_client_protocol::Error>` 识别 `reason == "historyUnavailable"`；
    - `AcpClient::trajectory(&self, id: &SessionId, cursor: TrajectoryCursor) -> Result<TrajectoryPage, BackendError>`；
    - `AcpClient::request_content(&self, id: &SessionId, hash: &ContentHash) -> Result<Option<RequestContent>, BackendError>`。

- [ ] **Step 1：写失败的测试**
  - 测试装置：在 acp 的测试装置里加一个接上 SQLite 历史库的构造方式，做法照 `kernel/tests/subagent/fixture.rs::with_state`：`StateRuntime::init(&SqliteConfig::new(home))`，再对 `sessions` 目录做一次空 `backfill`，然后 `KernelFactory::builder().state(Some(..))`。
  - `acp/tests/extension.rs`：
    - `trajectory_method_pages_a_session`：用现有的 ACP 测试装置跑一次脚本化运行，调用 `_clawcode/session/trajectory` 得到包含 `Request`、`Assistant` 的页；再用 `Request` 快照里的提示词哈希调用 `/content`，得到 `kind == "system_prompt"`。
    - `trajectory_rejects_two_cursors`：同时带 `before` 和 `after`，返回参数错误。
    - `trajectory_reports_history_unavailable`：不启用 SQLite 状态（装置里 `state = None`）调用，返回的错误 `data.reason == "historyUnavailable"`。
  - `desktop/tests/backend.rs::history_unavailable_error_is_recognised`：由带该 `data` 的 ACP 错误转换，得到 `BackendError::HistoryUnavailable`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p acp --test extension && cargo test -p desktop --test backend`
  - Expected：FAIL 或编译失败。
- [ ] **Step 3：实现**（按 Interfaces）
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2，再加 `cargo test -p kernel`
  - Expected：PASS。

### Task 7：桌面端数据模型 `TrajectoryModel`

**Files:**
- Create:
  - `crates/desktop/src/trajectory/mod.rs`：`TrajectoryModel` 和分页合并、选择、折叠、区间；
  - `crates/desktop/src/trajectory/rows.rs`：`LedgerRow`、`RecordText` trait、进行中计算。
- Modify: `crates/desktop/src/lib.rs`（`pub mod trajectory;`）、`crates/desktop/src/projection/mod.rs`（`running_tools`）
- Test: `crates/desktop/tests/trajectory_model.rs`

**Interfaces:**
- Consumes：Task 1 的类型。
- Produces：
  - `pub struct TrajectoryModel`（TypedBuilder，全部字段默认）：
    - `records: Vec<TrajectoryRecord>`、`leaf_id: Option<EntryId>`、`has_more_before: bool`、`last_sequence: Option<u64>`、`usage_before: ModelUsage`；
    - `selected: Option<String>`、`collapsed_runs: HashSet<RunKey>`、`calls_folded: bool`、`expanded_calls: HashSet<u32>`；
    - `interval: Option<TimeInterval>`。
  - `pub enum RunKey { Run(RunId), Between(u64) }`：`Between` 用该段第一条记录的顺序号。
  - `pub enum PageApplied { Replaced, Prepended, Appended }`。
  - `TrajectoryModel` 的方法：
    - `apply(&mut self, cursor: TrajectoryCursor, page: TrajectoryPage) -> PageApplied`：
      - `Tail` 或 `leaf_id` 与已保存的不同 → 整体替换；
      - `Before` → 前插；`After` → 追加；
      - 按 `id` 去重：同 id 的记录以新的为准，以便补齐工具结果。
    - `rows(&self, in_flight: &[InFlight]) -> Vec<LedgerRow>`；
    - 选择：`select(&mut self, id: &str)`、`select_adjacent(&mut self, delta: isize) -> Option<&TrajectoryRecord>`（在可见的记录行之间移动）、`selected_record(&self) -> Option<&TrajectoryRecord>`；
    - 折叠：`toggle_run(&mut self, key: RunKey)`、`set_all_runs_collapsed(&mut self, collapsed: bool)`、`set_calls_folded(&mut self, folded: bool)`、`toggle_call_group(&mut self, request_index: u32)`；
    - 筛选：`set_interval(&mut self, interval: Option<TimeInterval>)`；
    - 汇总：`summary(&self) -> TrajectorySummary { runs: usize, requests: usize, input: u64, output: u64, cache_hit_percent: Option<u8> }`；
    - `previous_request(&self, index: u32) -> Option<&RequestRecord>`：记录详情做差异比较时用。
  - `pub enum LedgerRow`：
    - `LoadEarlier`；
    - `RunHeader { key: RunKey, index: Option<u32>, duration_ms: Option<u64>, tokens: u64, collapsed: bool }`（`index = None` 表示「运行之间」）；
    - `Record { id: String }`（请求分隔行就是 `Request` 记录，渲染时区分）；
    - `CallGroup { request_index: u32, count: usize, duration_ms: u64, failures: usize }`；
    - `InFlight(InFlight)`。
  - `pub enum InFlight { Request { started_at_ms: u64 }, Tool { name: String, started_at_ms: u64 } }`；`InFlight::from_projection(projection: &SessionProjection, last_ended_ms: Option<u64>) -> Vec<InFlight>`：
    - 运行中且有执行中的工具 → 每个工具一项；
    - 运行中但没有执行中的工具 → 一项 `Request`，开始时间取 `last_ended_ms` 和 `run_started_ms()` 中较晚的一个；
    - 空闲 → 空。
  - `SessionProjection::running_tools(&self) -> impl Iterator<Item = &ToolRow>`。
  - `pub trait RecordText { fn summary(&self) -> String; fn matches(&self, needle: &str) -> bool; }`，为 `TrajectoryRecord` 实现：
    - 摘要取单行，最多 200 个字符；
    - 工具为 `名称 参数摘要 → 结果摘要`；
    - `matches` 不区分大小写，查完整的文本、参数和结果。
  - `pub struct TimeInterval { pub start_ms: u64, pub end_ms: u64 }`；`TimeInterval::overlaps(&self, record: &TrajectoryRecord) -> bool`：起止区间有交集即算命中，闭区间；只有开始时间的记录按一个点处理。
  - **可见行规则：**
    - 有更早的记录时，第一行是 `LoadEarlier`；
    - 每个运行一个 `RunHeader`，被折叠时只显示标题；
    - `calls_folded` 时，一次请求的工具行合成一行 `CallGroup`，除非这次请求在 `expanded_calls` 里；
    - 区间有效时只保留命中的记录行和它们所属的 `RunHeader`，不出现 `CallGroup`；
    - `InFlight` 行放在最后。

- [ ] **Step 1：写失败的测试**（`trajectory_model.rs`，用本文件里的小构造函数生成记录和页）
  - `tail_then_before_then_after_merges_in_order`：先 `Tail`，再 `Before`、`After`；`records` 的顺序号严格递增，没有重复。
  - `a_new_leaf_replaces_every_record`：`After` 页的 `leaf_id` 不同 → 返回 `Replaced`，旧记录全部消失；原先选中的 id 如果在新页里，仍保持选中，否则清除。
  - `a_later_tool_record_replaces_the_pending_one`：先拿到 `result_text = None` 的工具记录，再补取到同 id 的完成记录，只剩一条，并且有结果。
  - `rows_group_runs_and_fold_calls`：两次运行，第一次 2 个请求、3 个工具；`set_calls_folded(true)` 后，第一次运行下出现 `CallGroup { count: 3, .. }`；`toggle_run` 折叠后只剩标题。
  - `interval_keeps_overlapping_records`：记录 [0,10]、[20,30]、[40,50]，区间 [25,45] → 只剩后两条（加标题）。
  - `selection_survives_prepending`：选中一条后前插一页，`selected_record()` 仍是它；`select_adjacent(1)` 跳过标题行，选到下一条记录。
  - `in_flight_follows_the_projection`：运行中且无执行中工具时，`InFlight::Request` 的开始时间等于 `max(last_ended, run_started)`。
  - `summary_counts_cache_hits`：两次请求，输入各 1000，缓存读取 850 和 650 → `cache_hit_percent == Some(75)`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p desktop --test trajectory_model`
  - Expected：编译失败。
- [ ] **Step 3：实现**（按 Interfaces）
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2
  - Expected：PASS。

### Task 8：桌面端时间总览几何（纯计算）

**Files:**
- Create: `crates/desktop/src/trajectory/timeline.rs`
- Test: `crates/desktop/tests/trajectory_timeline.rs`

**Interfaces:**
- Consumes：Task 7 的 `TrajectoryModel` 记录、`TimeInterval`。
- Produces：
  - `pub enum DurationMode { Actual, Equal }`。
  - `pub enum Lane { Request, Tool(u8), Event }`，其中 `Tool` 的子行编号为 0–2。
  - `pub struct Span { pub record_id: String, pub lane: Lane, pub start: f64, pub first_token: Option<f64>, pub end: Option<f64>, pub failed: bool, pub running: bool }`。时间单位：实际模式是毫秒，等宽模式是序号单位。
  - `pub struct TimelineLayout { spans: Vec<Span>, run_starts: Vec<(u32, f64)>, domain: Range<f64> }`：
    - `TimelineLayout::new(records: &[TrajectoryRecord], in_flight: &[InFlight], now_ms: u64, mode: DurationMode) -> Self`；
    - 工具按开始时间贪心分配到最早空闲的子行；3 个子行都占用时，并入第 3 行，并在该 `Span` 上记录合并数量（`merged: u32`）；
    - 进行中的项 `end = now`、`running = true`。
  - `pub struct Viewport { pub visible: Range<f64> }`：
    - `Viewport::full(domain: &Range<f64>) -> Self`；
    - `zoom(&mut self, factor: f64, anchor: f64)`：`anchor` 是 0–1 的位置，缩放后锚点下的时间不变，不超出 `domain`；
    - `pan(&mut self, fraction: f64, domain: &Range<f64>)`；
    - `x_of(&self, t: f64, width: f32) -> f32`、`t_of(&self, x: f32, width: f32) -> f64`。
  - `TimelineLayout::bars(&self, viewport: &Viewport, width: f32) -> Vec<Bar>`：`Bar { record_id: String, lane: Lane, x: f32, first_token_x: Option<f32>, w: f32, failed: bool, running: bool, merged: u32 }`。只输出与视口相交的横条；同一泳道上相邻、宽度都不足 1 px 的横条合并为一个，`merged` 累加。
  - `TimelineLayout::hit(&self, viewport: &Viewport, width: f32, x: f32, lane: Lane) -> Option<&str>`。
  - `TimelineLayout::interval(&self, viewport: &Viewport, width: f32, from_x: f32, to_x: f32) -> TimeInterval`：等宽模式下，换算回所覆盖记录的真实起止时间。

- [ ] **Step 1：写失败的测试**
  - `overlapping_tools_pack_into_three_lanes`：5 个完全重叠的工具 → 子行 0、1、2，第 3 行的 `merged == 3`。
  - `request_span_splits_at_the_first_token`：请求 1000→1800→2600，`first_token == Some(1800.0)`。
  - `zoom_keeps_the_anchor_time`：`domain = 0..1000`，在 `anchor = 0.25` 处放大 2 倍；放大前后锚点时间都是 250，可见宽度为 500，且不越出 `0..1000`。
  - `pan_stops_at_the_domain_edges`：平移到左边界后再左移，`visible.start` 保持 0。
  - `thin_neighbours_merge_below_a_pixel`：1000 个 1 ms 的请求放进 100 px 宽的视图，得到的横条数不超过 100，`merged` 之和为 1000。
  - `equal_mode_gives_each_record_one_unit`：3 条记录在 `Equal` 下的跨度分别为 0..1、1..2、2..3。
  - `interval_maps_back_to_real_time_in_equal_mode`：拖过第 2 个单位 → 区间是第 2 条记录的真实起止时间。
  - `running_span_extends_to_now`：进行中的工具，`end == now`，`running == true`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p desktop --test trajectory_timeline`
  - Expected：编译失败。
- [ ] **Step 3：实现**（按 Interfaces）
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2
  - Expected：PASS。

### Task 9：「对话 / 轨迹」切换、工具栏、记录表、加载与补取

**Files:**
- Create:
  - `crates/desktop/src/ui/trajectory/mod.rs`：`TrajectoryView`；
  - `crates/desktop/src/ui/trajectory/toolbar.rs`；
  - `crates/desktop/src/ui/trajectory/table.rs`：`LedgerTable: TableDelegate`。
- Modify:
  - `crates/desktop/src/ui/mod.rs`（声明模块）；
  - `crates/desktop/src/ui/session/mod.rs`（`ViewMode`、懒创建、渲染分支、补取触发）；
  - `crates/desktop/src/ui/shell/card.rs`（切换控件）；
  - `crates/desktop/src/actions.rs`（`ToggleTrajectory`、键位）；
  - `crates/desktop/src/ui/shell/title_bar.rs`（「视图」菜单项）；
  - `crates/desktop/src/projection/mod.rs`、`projection/events.rs`（`ProjectionChange::history`）；
  - `crates/desktop/locales/app.yml`。
- Modify: `crates/desktop/tests/support/mod.rs`（`ScriptedBackend::start_with_history()`：做法同 Task 6 的测试装置，给 `scripted_kernel` 接上 SQLite 历史库；`start()` 保持不接）
- Test: `crates/desktop/tests/shell_trajectory.rs`（沿用 `tests/support/shell.rs` 的 `live` 装置，另加使用 `start_with_history()` 的版本）

**Interfaces:**
- Consumes：Task 6 的客户端方法、Task 7 的 `TrajectoryModel` / `LedgerRow` / `InFlight`。
- Produces：
  - `ProjectionChange` 增加 `#[builder(default)] pub history: bool`。`_clawcode/event` 更新的 `payload.event` 为 `message_end`、`tool_execution_end`、`turn_end`、`run_end` 之一时置位，只读事件名字符串，不解码整个消息；`extend` 时取或。
  - `pub enum ViewMode { Chat, Trajectory }`。`SessionView` 增加字段 `view_mode: ViewMode` 和 `trajectory: Option<Entity<TrajectoryView>>`，以及：
    - `set_view_mode(&mut self, mode: ViewMode, window: &mut Window, cx: &mut Context<'_, Self>)`；
    - `view_mode(&self) -> ViewMode`；
    - `trajectory(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Option<Entity<TrajectoryView>>`：草稿返回 `None`，否则不存在时创建。
  - `TrajectoryView`：
    - 字段：`session: Entity<SessionHandle>`、`client: Option<AcpClient>`、`model: TrajectoryModel`、`table: Entity<TableState<LedgerTable>>`、`load: LoadState`、`_load: Option<Task<()>>`、`_refresh: Option<Task<()>>`；字段超过 3 个，用 TypedBuilder 构造。
    - `pub enum LoadState { Idle, Loading, Failed(String), Unavailable }`。
    - `pub enum TrajectoryEvent { Open(String) }`：回车或双击记录时发出。
    - 方法：
      - `new(session, client, window, cx) -> Self`：立即取末尾一页；
      - `refresh(&mut self, cx)`：150 ms 合并窗口后查 `After(last_sequence)`；没有已加载内容时改查 `Tail`；
      - `load_earlier(&mut self, cx)`；
      - `load_all(&mut self, cx)`：连续调用 `load_earlier`，直到 `has_more_before == false`；供工具栏的「加载全部」按钮使用；
      - `apply_page(&mut self, cursor: TrajectoryCursor, page: TrajectoryPage, cx)`：生产加载路径和测试都调用它；
      - `select(&mut self, id: &str, cx)`、`select_adjacent(&mut self, delta: isize, cx)`；
      - `model(&self) -> &TrajectoryModel`。
  - **补取触发：** `SessionView` 收到会话更新时，若 `change.history` 且 `view_mode == Trajectory`，调用 `trajectory.refresh(cx)`；切换到「轨迹」时也调用一次 `refresh`。
  - **跟随：** 记录表滚动到底部时为跟随状态，补取后滚到底；不在底部时显示「回到最新」按钮。
  - `actions::ToggleTrajectory`：键位 `primary!("shift-t")`，绑定在 `WORKSPACE_CONTEXT`。`AppView::on_toggle_trajectory` 切换当前会话视图的 `ViewMode`。
  - 切换控件：`TabBar` 的 `Segmented` 变体，两个标签「对话」「轨迹」，放在 `render_card_header` 右侧工具组最前面；当前是草稿标签时不渲染。

- [ ] **Step 1：写失败的测试**（`shell_trajectory.rs`；在会话视图上直接 `apply_page`，不依赖后端）
  - `switch_shows_the_trajectory_and_back`：点击 `view-switch:trajectory` 后能找到 `trajectory-table`；点击 `view-switch:chat` 后回到 `transcript`；底部输入框一直存在。
  - `each_tab_remembers_its_view`：两个会话标签，A 切到轨迹，再切到 B，B 是对话；回到 A 仍是轨迹。
  - `toggle_trajectory_action_switches_the_active_tab`：派发 `ToggleTrajectory`，视图变化。
  - `rows_render_every_kind`：`apply_page` 一个含所有记录类型的页，能找到 `ledger-row:{id}` 的各行，以及 `run-header:1`、`request:{entry_id}`。
  - `enter_opens_the_selected_record`：选中一行后按回车，订阅到 `TrajectoryEvent::Open(id)`。
  - `a_live_run_appears_in_the_open_trajectory`：用 `start_with_history()` 的后端打开会话并切到轨迹，发送一条会触发一次工具调用的脚本化提示；运行结束后，最终能看到 `request:` 开头的两行和一个工具行，并且请求 #1 的工具行有结果。这条测试覆盖补取的完整路径：事件 → `history` 标记 → 合并 → `After` 查询 → 合并到模型。
  - `unavailable_history_offers_retry`：用 `ScriptedBackend::start()`（没有历史库）打开轨迹，界面出现 `trajectory-retry`，记录表区域不显示骨架。
  - `following_stops_after_scrolling_up`：滚到中间后 `apply_page(After)`，滚动位置不变，`trajectory-latest` 按钮出现。
  - `draft_tab_has_no_switch`：草稿标签找不到 `view-switch:trajectory`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p desktop --test shell_trajectory`
  - Expected：编译失败。
- [ ] **Step 3：实现**
  - **记录表：**
    - 列为类型、内容、输入、输出、思考、耗时；行高统一 `rems(2.)`；
    - 请求分隔行（`Request` 记录）显示「请求 #N · 模型 · 首 token 0.8s」，首 token 延迟取同一请求的 `Assistant` 记录；`prompt_changed` 或工具有增减时加变化标记；
    - 用 `cell_text` 返回可读文本，`render_td` 画类型标记和带颜色的文字；
    - 数字列右对齐，用等宽数字（主题等宽字体）；
    - 错误行用 `theme.danger`；
    - `LoadEarlier` 行点击或回车调用 `load_earlier`。
  - **工具栏：**
    - 时长切换、「轮次」「调用」开关、总览开关（Task 10 接入）、右侧汇总；
    - 窄窗口（工具栏宽度小于 `rems(36.)`）时，三个开关收进 `DropdownMenu`。
  - **状态：**
    - `Loading` 且没有记录时显示表格形状的骨架；
    - 空会话显示设计 5.4 的文案；
    - `Unavailable` 和 `Failed` 显示原因和「重试」；
    - 补取失败时在表格底部插一行错误和「重试」，已加载的内容保留。
  - **文案：** 新增 `Trajectory.*`（en / zh-CN），包括视图名、工具栏、列名、各类型名、状态文案、「运行之间」「仅工具调用」「回到最新」「加载更早的记录」「加载全部」。
- [ ] **Step 4：运行测试，确认通过**
  - Run：`cargo test -p desktop --test shell_trajectory && cargo test -p desktop --test projection`
  - Expected：PASS。

### Task 10：时间总览画布与交互

**Files:**
- Create: `crates/desktop/src/ui/trajectory/timeline.rs`（画布元素和事件处理）
- Modify: `crates/desktop/src/ui/trajectory/mod.rs`（布局插入、`Viewport`、`DurationMode`、悬停计时器）、`crates/desktop/src/state.rs`（`LayoutState::timeline_open: bool`，默认 `true`）
- Test: `crates/desktop/tests/shell_trajectory.rs`、`crates/desktop/tests/state.rs`

**Interfaces:**
- Consumes：Task 8 的 `TimelineLayout`、`Viewport`、`Bar`；Task 9 的 `TrajectoryView`。
- Produces：
  - `TrajectoryView` 增加 `viewport: Viewport`、`duration_mode: DurationMode`、`hover: Option<(String, Task<()>)>`。
  - 方法：
    - `set_duration_mode(&mut self, mode: DurationMode, cx)`；
    - `zoom_at(&mut self, factor: f64, anchor: f64, cx)`、`pan_by(&mut self, fraction: f64, cx)`、`reset_zoom(&mut self, cx)`；
    - `select_interval(&mut self, interval: Option<TimeInterval>, cx)`：转给 `model.set_interval`，工具栏同步显示区间和清除按钮。
  - 画布：
    - 用 `canvas(prepaint, paint)` 画三条泳道、轮次分隔线和标签、时间刻度；
    - 请求横条的等待段用 `theme.chart_1.opacity(0.35)`，生成段用 `theme.chart_1`；工具用 `theme.chart_2`；事件用 `theme.muted_foreground`；失败用 `theme.danger`；进行中的横条用虚线收尾；
    - 合并的横条上标出数量（宽度足够时）。
  - 交互映射：

    | 输入 | 调用 |
    | --- | --- |
    | 左键单击横条 | `select(id)` 并滚动记录表 |
    | 左键拖动 | `select_interval(Some(..))` |
    | 右键单击 | `select_interval(None)` |
    | 滚轮 | `zoom_at` |
    | Shift+滚轮，或右键拖动 | `pan_by` |
    | 双击 | `reset_zoom` |
    | 获得焦点时左右键 | `select_adjacent(±1)` |
    | Esc | `select_interval(None)` |

  - 进行中有项目时，视图每秒 `cx.notify()` 一次，复用外壳已有的 ticker。

- [ ] **Step 1：写失败的测试**
  - `state.rs::timeline_visibility_is_saved`：`timeline_open` 默认为 `true`，设为 `false` 后序列化再读回仍为 `false`。
  - `shell_trajectory.rs`：
    - `clicking_a_bar_selects_its_record`：用 `apply_page` 放入 3 条请求，在 `trajectory-timeline` 元素内按 `TimelineLayout::bars` 算出的第 2 根横条中点单击，`model().selected_record()` 是第 2 条。
    - `dragging_filters_the_table`：拖过第 2 根横条的范围后，记录表只剩它和它的运行标题；工具栏出现 `trajectory-interval-clear`，点击后恢复。
    - `timeline_toggle_hides_the_overview`：点击总览开关后找不到 `trajectory-timeline`。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p desktop --test shell_trajectory --test state`
  - Expected：FAIL。
- [ ] **Step 3：实现**（按 Interfaces；画布里所有坐标换算都调用 `Viewport` 和 `TimelineLayout` 的方法，不在画布代码里重复数学计算）
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2
  - Expected：PASS。

### Task 11：记录详情面板标签

**Files:**
- Create:
  - `crates/desktop/src/ui/panel/record.rs`：`RecordView`、顶部、分页切换；
  - `crates/desktop/src/ui/panel/record_sections.rs`：各分页内容的渲染。
- Modify:
  - `crates/desktop/src/state.rs`（`PanelTabKey::Record`）；
  - `crates/desktop/src/ui/panel/mod.rs`、`lifecycle.rs`（`PanelView::Record`、构建）、`render.rs`（`PanelTabKey::label` 和 `id` 的分支）；
  - `crates/desktop/src/ui/mod.rs` 或 `ui/shell/lifecycle.rs`（订阅 `TrajectoryEvent::Open`，打开标签）；
  - `crates/desktop/src/ui/trajectory/mod.rs`（内容缓存）；
  - `crates/desktop/Cargo.toml`（引用 `similar = { workspace = true }`，按规则 11 分组）；
  - `crates/desktop/locales/app.yml`。
- Test: `crates/desktop/tests/shell_record.rs`、`crates/desktop/tests/state.rs`

**Interfaces:**
- Consumes：Task 9 的 `TrajectoryView`、`TrajectoryEvent`；Task 7 的 `previous_request`；Task 6 的 `request_content`。
- Produces：
  - `PanelTabKey::Record`：单元变体，序列化为 `{"kind":"record"}`；`id()` 返回 `"record"`；标题「记录详情」；图标 Lucide `ScrollText`，缺的加进 `ExtraIcons`。
  - `TrajectoryView` 增加 `contents: HashMap<ContentHash, ContentState>`，其中 `ContentState { Loading, Ready(Arc<RequestContent>), Missing, Failed(String) }`；方法 `request_content(&mut self, hash: &ContentHash, cx) -> ContentState`，第一次调用时发起加载。
  - `RecordView`（TypedBuilder）：字段 `trajectory: Entity<TrajectoryView>`、`section: RecordSection`、`prompt_mode: PromptMode`、`_observe: Subscription`。
    - `RecordView::new(trajectory: Entity<TrajectoryView>, cx) -> Self`：观察 `trajectory`，选中变化时 `notify`；选中的记录类型变了，就把 `section` 重置为 `Summary`。
    - `pub enum RecordSection { Summary, Timing, Usage, SystemPrompt, Tools, Options, Arguments, Result, Raw }`；`RecordSection::for_record(record: &TrajectoryRecord) -> &'static [RecordSection]` 按设计第 7 节的表返回各类型可用的分页。
    - `pub enum PromptMode { Full, Diff }`。
  - 打开：`AppView` 订阅活动会话的 `TrajectoryEvent::Open`，调用 `open_panel_tab(PanelTabKey::Record, ..)`。`build_panel_view` 对 `Record` 通过 `self.views[TabEntry::Session(id)]` 的 `trajectory(window, cx)` 取得实体，再构建 `RecordView`；会话没有轨迹实体时创建一个。
  - 分页内容：
    - **计时：** 吞吐量 = 输出 token ÷（末 token − 首 token）秒；分母为 0 时显示「时长过短」；
    - **用量：** 「会话累计」= `usage_before` 加上已加载记录里到这一条为止的用量；
    - **系统提示词：** `PromptMode::Diff` 用 `similar::TextDiff::from_lines` 生成统一差异文本，交给 gpui-kit 的 `DiffFile::parse_with` 和 `Diff` 显示；只有 `prompt_changed` 时才显示「完整 / 差异」切换；
    - **工具：** 解析工具清单 JSON；每个工具用 `Collapsible` 展开显示 Schema；顶部列出增减；
    - **选项、参数、原始内容：** `serde_json::to_string_pretty`，用代码高亮只读显示，带复制按钮。
  - 顶部：类型标记、标题、状态，「上一条」「下一条」按钮（`record-prev`、`record-next`）调用 `trajectory.select_adjacent(∓1)`。

- [ ] **Step 1：写失败的测试**
  - `state.rs::record_panel_tab_round_trips`：`PanelTabKey::Record` 序列化为 `{"kind":"record"}` 并能读回。
  - `shell_record.rs`：
    - `opening_a_record_shows_its_sections`：轨迹里选中一条 `Request` 记录并回车，面板出现 `panel-tab:record`；分页按钮依次为概述、计时、用量、系统提示词、工具、选项、原始内容。
    - `detail_follows_the_selection`：点 `record-next`，`trajectory.model().selected_record()` 前进一条，标题随之改变。
    - `tool_record_sections`：工具记录的分页依次为概述、参数、结果、计时、原始内容。
    - `prompt_diff_toggle_only_when_changed`：`prompt_changed == false` 时找不到 `prompt-mode:diff`；为 `true` 时能找到。
    - `record_tab_without_selection_shows_hint`：面板里有 `Record` 标签但轨迹没有选中项（例如重启后），标签内容显示「在轨迹中选择一条记录」。
    - `missing_content_is_reported`：内容状态为 `Missing` 时，系统提示词页显示「本会话没有这份内容」。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p desktop --test shell_record --test state`
  - Expected：FAIL。
- [ ] **Step 3：实现**（按 Interfaces）
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2，再加 `cargo test -p desktop --test shell_panel`（确认原有面板行为不受影响）
  - Expected：PASS。

### Task 12：文档、全量检查与实机验收

**Files:**
- Modify: `docs/superpowers/specs/2026-11-21-desktop-trajectory-design.md`（只在执行中出现 Ruling 时补充说明）
- Ledger：`.superpowers/sdd/2026-11-21-desktop-m4a-trajectory/progress.md`

- [ ] **Step 1：全量检查**
  - Run：`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test --workspace`
  - 先备份未跟踪文件，再运行 `pre-commit run --all-files`。
  - Expected：全部通过。
- [ ] **Step 2：实机验收**（生产后端、真实 DeepSeek、嵌套 KWin、隔离的 XDG，沿用 M3b 的 `live/` 脚本）
  - 跑一个会话，覆盖：并行和出错的工具调用、子 agent、运行中插话、`/compact`、导航到另一个分支。
  - 逐项截图（设计 9.2 的 7 项），保存为 `m4a-01` 之后的编号。
  - 对照 ACP 推送的计时和用量，确认至少一次请求的首 token 延迟和输出 token 一致。
- [ ] **Step 3：** 在 ledger 记录结果、Rulings 和推迟的小问题，用中文向用户汇报。不提交，等用户明确要求。

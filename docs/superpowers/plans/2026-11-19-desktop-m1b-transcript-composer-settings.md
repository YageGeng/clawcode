# Desktop M1b：Transcript、Composer 与设置页 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 M1a 外壳上补齐 M1 的剩余部分。会话 Tab 能回放和流式渲染 transcript：用户气泡和图片、Markdown 正文、thinking 折叠、工具分组框、Run 折叠。composer 支持发送、停止和 follow-up 队列，新会话在发出第一条消息时创建。新增设置页，包含通用和外观两个分页。同时收掉 M1a 遗留的几项：agent 标题回写目录、侧栏筛选、顶栏运行耗时、Ctrl+L 和 Ctrl+,。界面要和设计稿对得上。

**Architecture:** 新增 UI 无关的 `transcript` 模块。它把 `SessionProjection` 折算成 `TranscriptLayout`（显示行：分组位置、Run 折叠摘要），并算出新旧布局之间的最小 splice。每个 Tab 对应一个 `SessionView` 实体，持有 `MessageScrollerState`、按 messageId 缓存的 `TextViewState`、折叠状态和 `Composer`。投影每变化一次，它只 splice 或 remeasure 受影响的行。设置页是单例 Tab，使用 gpui-kit `Settings`，修改后立即应用、写回 `desktop.json`，并让所有 transcript 重新测量。

**Tech Stack:** 与 M1a 相同：gpui-kit 0.10（`MessageScroller`、`TextView`、`Textarea`、`Diff`、`Settings`、`ShimmerText`）、agent-client-protocol 5 v2 schema、rust-i18n 4。新增使用 workspace 已有的 `base64` 依赖。

**Spec:** [Desktop 客户端设计](../specs/2026-11-19-desktop-gpui-kit-design.md)，视觉稿见 [mockup](../specs/2026-11-19-desktop-gpui-kit-mockup.html)。前序计划见 [M1a](./2026-11-19-desktop-m1a-shell-and-data.md)。

## Global Constraints

- M1a 的 Global Constraints 全部继续有效，包括 AGENTS.md 规则、clippy `-Dwarnings`（非测试代码不能 `unwrap`、`panic!`，也不能直接下标）、只用 rem 和主题 token、图标统一用 Lucide（缺的补进 `ExtraIcons`）。
- **不提交**。M1a 到 M4 全部完成，且用户明确要求后，才作为一个提交进入 `main`（见 memory `desktop-single-commit`）。
- 行的 `ElementId` 用 `RowKey` 的 Display（`message:{id}`、`tool:{id}`）。Run 摘要行用 `run:{anchor key}`。
- 尺寸按默认字号 16px 换算成 rem，取自设计稿：
  - transcript 列：`max_w(rems(42.5))`，水平居中；外边距 `pt_5 px_7`；行间距 `gap_3`。
  - 用户气泡：`muted` 底，最大宽 78%，`rounded(radius_lg)`，`px_3p5 py_2`。
  - 工具分组框：1px `border`，`rounded(radius)`；每行高 `rems(2.125)`，`px_3`，行与行之间画 hairline。
  - composer：最大宽 `rems(41.25)`，左右 `mx_7`，`mt_3 mb_4`，`background` 底，1px `border`，`rounded(radius_lg)`，浅色主题加 `shadow_sm`。
  - 发送按钮：`Button::primary().small()`，全圆角；运行中换成停止图标。
- thinking 默认折叠。结束的 Run 默认折叠。正在运行的 Run 一直展开。
- 工具 kind 和图标、动词的对应关系见设计稿 7.2：
  - read → `file-text`，读取
  - edit → `pencil`，编辑
  - delete → `trash-2`，删除
  - move → `arrow-right-left`，移动
  - search → `search`，搜索
  - execute → `square-terminal`，执行
  - fetch → `globe`，获取
  - think → `brain`，思考
  - 其他 → `wrench`，工具
- execute 工具展开后默认只显示尾部 20 行，另有「完整输出」开关。
- 以下属于 M2，M1b 不做：
  - slash 命令
  - 模型和思考级别选择
  - 上下文环
  - 计划模式开关
  - 附件和粘贴图片
  - 用量行
  - 压缩和分支摘要卡片
  - 问题、计划审批、权限、elicitation

  composer 底栏左侧在 M1b 为空，右侧只有发送按钮。
- 已送达的用户消息上的「本轮补充 / 稍后发送」标签不做。后端没有在消息 meta 里标出它来自哪条队列。
  - ponytail：等后端在 `MessageEnd` meta 里带上 queue kind 后再加。
- 「打开配置文件」之外的后端配置不放进设置页。设置页只放 desktop 自己的偏好。

## Review Focus

1. **很长的流式回复或工具输出**（几千个片段、几百 KB）：每个片段只 splice 尾部行；正文用 `push_str` 追加，不重新解析整段；execute 输出默认只渲染尾部 20 行。测试在 Task 2（`streaming_append_only_touches_the_tail`）和 Task 4（`markdown_cache_appends_and_resets`、`exec_output_shows_the_tail_until_expanded`）。
2. **输入法组字、Shift+Enter、只有空白的内容**：组字时按 Enter 只确认候选，不发送；Shift+Enter 插入换行；只有空白时不发送。测试在 Task 5。
3. **运行中按 Enter，以及 Esc 的层级**：运行中按 Enter 进入「稍后发送」队列并显示 chip；空闲时按 Esc 什么都不做；运行中按 Esc 才停止。测试在 Task 5。
4. **新会话的第一条消息发送失败**（后端已断开）：输入框里的文字保留，Tab 仍是草稿，弹出错误通知。测试在 Task 5（`draft_send_failure_keeps_the_text`）。
5. **改字号或语言时 transcript 正开着**：行要整体重新测量，相邻行不能重叠。测试在 Task 7（`font_size_change_remeasures_rows`）。

---

### Task 1: 投影补充：工具细节、终端输出、消息耗时、队列、Run 开始时间

**Files:**
- Modify: `crates/desktop/src/projection.rs`
- Modify: `crates/desktop/tests/support/mod.rs`（新增脚本：`read`、`edit`、`exec`；fixture 工作区写入 `notes.txt`，内容为 `"alpha\n"`）
- Modify: `crates/desktop/Cargo.toml`（`base64 = { workspace = true }`）
- Test: `crates/desktop/tests/projection.rs`

**Interfaces:**
- Consumes：M1a 的 `SessionProjection`、`ToolRow`、`MessageRow`、`ProductMeta`、`ProjectionChange`。
- Produces：
  - `ProductMeta` 新增 `#[builder(default)] pub message_timing: Option<(u64, u64)>`。值取自 `clawcode.messageTiming.startedAtMs` 和 `endedAtMs`，两者都是十进制字符串，也接受数字。
  - `MessageRow` 新增 `#[builder(default)] pub duration_ms: Option<u64>`，等于 `ended - started`。thinking 的「8 秒」用的就是它。
    - ponytail：后端只给出整条消息的耗时，没有单独的 reasoning 耗时，所以这里用整条消息耗时近似。
  - `ToolRow` 新增 `#[builder(default)] pub patch: Option<String>`，来自 `ToolCallContent::Diff` 的 `patch.text`。
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum ToolCategory { Read, Edit, Delete, Move, Search, Execute, Fetch, Think, Other }`，实现 `From<Option<&wire::ToolKind>>`。
  - `impl ToolRow`：
    - `pub fn category(&self) -> ToolCategory`
    - `pub fn target(&self) -> Option<&str>`：在 `raw_input` 里找第一个字符串值。Search 类按 `pattern`、`path` 的顺序查找；其他类按 `path`、`file_path`、`cmd`、`command`、`url`、`query`、`pattern` 的顺序查找。都没有时返回 `None`，UI 退回显示 `title`。
    - `pub fn diff_stats(&self) -> Option<(usize, usize)>`：统计 `patch` 里以 `+` 开头（不含 `+++`）和以 `-` 开头（不含 `---`）的行数。
    - `pub fn line_count(&self) -> usize`：`output` 的行数。
  - `SessionUpdate::TerminalUpdate`：如果 `terminal_id` 等于某个已有工具行的 id，并且 `output` 有值，就把 base64 解码，按 lossy UTF-8 转成文本，**整体替换**该行的 `output`（服务端每次发的是完整快照）。解码失败就忽略，不改动。没有对应工具行的终端更新直接忽略。
    - ponytail：不解析 ANSI，转义序列按原样保留。需要时再加 strip。
  - `SessionUpdate::Other`：`session_update == "_clawcode/event"`，并且 `fields["payload"]["event"] == "queue_changed"` 时，把 `payload["pending"]` 反序列化成 `protocol::PendingMessages`，存进投影。反序列化失败时打一条 `tracing::warn!`，忽略这次更新。
  - `SessionProjection` 新增：
    - `pub fn pending(&self) -> &protocol::PendingMessages`
    - `pub fn set_pending(&mut self, pending: protocol::PendingMessages) -> bool`：返回是否有变化
    - `pub fn run_started_ms(&self) -> Option<u64>`
  - `ProjectionChange` 新增 `pub queue: bool`。`is_empty` 和 `extend` 都要把它算进去。
  - Run 开始时间的规则：`StateUpdate::Running` 让状态从非 Running 变成 Running 时，记下该更新 meta 里的 `timestampMs`，没有就记当前时间。变成 Idle 时清空。`run_started_ms()` 在状态为 Running 但没有记录时（例如中途 attach 一个正在运行的会话），退回使用最后一条 `Role::User`、`origin == Prompt` 消息的 `timestamp_ms`。
- 脚本模型。工具调用的 id 固定为 `read-1`、`edit-1`、`exec-1`（`exec30` 例外，见下），工具执行完后的下一轮请求统一回复 `"ok"`：
  - `"read"` → `ToolCall read {"path":"notes.txt"}`
  - `"edit"` → `ToolCall edit {"path":"notes.txt","edits":[{"oldText":"alpha","newText":"beta"}]}`
  - `"exec"` → `ToolCall exec_command {"cmd":"printf 'one\\ntwo\\n'","login":false}`
  - `"exec30"` → `ToolCall exec_command {"cmd":"seq 1 30","login":false}`，id 为 `exec-30`

  fixture 的 Kernel 必须让内置工具直接执行，不发审批请求。如果默认会发，就在 `scripted_kernel` 里设置 yolo。

- [ ] **Step 1: 写失败的测试**（真实 ACP 帧，写法和 M1a 的 `tests/projection.rs` 一致）

```rust
#[test] fn read_tool_reports_target_and_line_count()
    // prompt "read" → wait_idle → tool:read-1: category Read, target Some("notes.txt"), status Completed, line_count() >= 1
#[test] fn edit_tool_carries_patch_and_stats()
    // prompt "edit" → tool:edit-1: category Edit, diff_stats() == Some((1, 1)), patch contains "+beta"
#[test] fn exec_output_follows_terminal_snapshots()
    // prompt "exec" → tool:exec-1: category Execute, target Some("printf 'one\\ntwo\\n'"), output contains "one\ntwo"
#[test] fn terminal_update_for_unknown_tool_is_ignored()
    // hand-built TerminalUpdate("nope", output base64 "aGk=") → change.is_empty()
#[test] fn assistant_message_records_its_duration()
    // prompt "think" → assistant row duration_ms.is_some()
#[test] fn queue_snapshot_replaces_pending()
    // prompt "slow" then prompt "hello" while running → some update sets change.queue and pending().follow_up.len() == 1;
    // after wait_idle → pending().follow_up.is_empty()
#[test] fn run_start_is_recorded_and_cleared()
    // prompt "slow": once Running → run_started_ms().is_some(); after Idle → None
#[test] fn attaching_mid_run_falls_back_to_the_last_prompt_time()
    // fresh projection: set_run_state(Running), apply replayed UserMessage(meta timestampMs "1000") → run_started_ms() == Some(1000)
#[test] fn malformed_queue_payload_is_ignored()
    // Other("_clawcode/event", {"payload":{"event":"queue_changed","pending":42}}) → !change.queue
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test projection`
Expected: 编译失败（新方法和新字段还不存在）。

- [ ] **Step 3: 按上面的 Interfaces 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test projection`
Expected: 全部通过（包括 M1a 原有的用例）。

---

### Task 2: `TranscriptLayout`：分组、Run 折叠、最小 splice、耗时文案

**Files:**
- Create: `crates/desktop/src/transcript.rs`（在 `lib.rs` 里登记 `pub mod transcript`）
- Modify: `crates/desktop/locales/app.yml`（`Elapsed.*`、`Transcript.worked`、`Tool.*` 动词）
- Test: `crates/desktop/tests/transcript.rs`

**Interfaces:**
- Consumes：Task 1 的 `SessionProjection`、`ToolCategory`、`RowKey`。
- Produces：
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum GroupPosition { Only, First, Middle, Last }`
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct RunSummary { pub elapsed_ms: Option<u64>, pub counts: Vec<(ToolCategory, usize)> }`。`counts` 按 `ToolCategory` 的声明顺序排列，只包含非零项。
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub enum LayoutRow { Row { key: RowKey, index: usize, group: Option<GroupPosition> }, RunSummary { anchor: RowKey, summary: RunSummary } }`。另有 `pub fn element_id(&self) -> SharedString`，返回 key 的 Display，摘要行返回 `run:{anchor}`。
  - `#[derive(Default)] pub struct TranscriptLayout { rows: Vec<LayoutRow> }`，方法：
    - `pub fn build(projection: &SessionProjection, expanded_runs: &HashSet<RowKey>) -> Self`
    - `pub fn len(&self) -> usize`、`pub fn row(&self, ix: usize) -> Option<&LayoutRow>`、`pub fn position_of(&self, key: &RowKey) -> Option<usize>`
    - `pub fn diff(&self, next: &TranscriptLayout, updated: &[RowKey]) -> LayoutDiff`
  - `#[derive(Debug, PartialEq, Eq)] pub struct LayoutDiff { pub splice: Option<(Range<usize>, usize)>, pub remeasure: Vec<usize> }`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Elapsed(pub u64)`（毫秒），方法 `pub fn label(self) -> SharedString`。规则：小于 60 秒显示 `t!("Elapsed.seconds")`；小于 1 小时显示 `Elapsed.minutes`（分和秒）；其余显示 `Elapsed.hours`（小时和分）。
  - `impl ToolCategory { pub fn verb(self) -> SharedString }`，取 `t!("Tool.<category>")`。
  - `impl RunSummary { pub fn label(&self) -> SharedString }`：`Transcript.worked`（带 elapsed；没有耗时就不带）加上 ` · {verb} {n}`，逐项拼接。

**build 算法**：
1. **跳过空行**：文本为空、reasoning 为空、没有图片、而且不在流式输出中的消息不进布局。
2. **切分 Run 段**：每遇到一条 `Role::User`、`origin == Prompt` 的消息，就开始一个新段，这条消息的 key 就是该段的 anchor。第一条这样的消息之前的行自成一段，anchor 是该段第一行的 key。
3. **判断是否结束**：不是最后一段的都算已结束；最后一段在 `run_state() == Idle` 时也算已结束。
4. **折叠**：已结束、至少有一行工具、并且 anchor 不在 `expanded_runs` 里的段，把所有 Tool 行去掉，在第一行 Tool 的位置放一行 `RunSummary`。
   - `elapsed_ms` = 段内所有行的时间戳的最大值（消息的结束时间 = `timestamp_ms + duration_ms`）减去 anchor 的时间戳。任一端缺失就是 `None`。
5. **工具分组**：没有折叠的段里，连续的 Tool 行标上 `GroupPosition`：只有一行时是 `Only`，否则依次是 `First`、`Middle`…、`Last`。中间夹着消息行或摘要行就断开。

**diff 算法**：
- `prefix` = 从头开始逐个比较、`old[i] == next[i]` 的行数。
- `suffix` = 从尾部开始逐个比较、相等的行数，上限是 `min(old.len, next.len) - prefix`。
- 只要中间有不同的行，`splice = Some((prefix..old.len - suffix, next.len - prefix - suffix))`。
- `remeasure` = `next` 中落在 prefix 或 suffix 区间内、key 属于 `updated` 的行的下标，升序。

- [ ] **Step 1: 写失败的测试**。用手工构造的 `wire::SessionUpdate` 喂给投影，带 sequence meta。

```rust
#[test] fn consecutive_tools_form_one_group()        // user, tool a, tool b, tool c, assistant → groups [First, Middle, Last]; a lone tool → Only
#[test] fn finished_run_folds_its_tools_into_one_summary()
    // user(ts 0), tool read, tool edit, tool edit, assistant(ts 88_000), Idle →
    // rows: [user, RunSummary{elapsed_ms: Some(88_000), counts: [(Read,1),(Edit,2)]}, assistant]
#[test] fn running_run_stays_expanded()             // same rows, state Running → no RunSummary
#[test] fn expanded_anchor_unfolds_a_finished_run() // expanded_runs = {anchor} → tools present, grouped
#[test] fn only_the_last_segment_can_be_live()      // two prompts, Running → first segment folded, second expanded
#[test] fn empty_messages_are_skipped()             // assistant with no text/reasoning/images, not streaming → absent
#[test] fn streaming_append_only_touches_the_tail() {
    // layout A (5 rows); append a chunk to the last assistant message → layout B equal rows
    // assert_eq!(a.diff(&b, &[last_key]), LayoutDiff { splice: None, remeasure: vec![4] });
    // a new tool row appended → splice == Some((5..5, 1))
}
#[test] fn folding_replaces_the_tool_range()        // Running→Idle: diff.splice covers exactly the tool rows, count 1
#[test] fn summary_and_elapsed_labels() {
    rust_i18n::set_locale("en");
    assert_eq!(Elapsed(14_000).label(), "14s");
    assert_eq!(Elapsed(88_000).label(), "1m 28s");
    assert_eq!(Elapsed(3_720_000).label(), "1h 2m");
    // RunSummary{Some(88_000), [(Read,3),(Edit,2),(Execute,1)]}.label() == "Worked 1m 28s · Read 3 · Edit 2 · Run 1"
    rust_i18n::set_locale("zh-CN");
    // == "已工作 1 分 28 秒 · 读取 3 · 编辑 2 · 执行 1"
}
```

  文案测试会切换全局 locale，放进单独的测试函数，并在函数结束前恢复成 `"en"`。这个测试二进制里只有它一个用例会改 locale。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test transcript`
Expected: 编译失败。

- [ ] **Step 3: 实现**。locale 文案（en / zh-CN）：
  - `Elapsed.seconds` "%{s}s" / "%{s} 秒"
  - `Elapsed.minutes` "%{m}m %{s}s" / "%{m} 分 %{s} 秒"
  - `Elapsed.hours` "%{h}h %{m}m" / "%{h} 小时 %{m} 分"
  - `Transcript.worked` "Worked %{elapsed}" / "已工作 %{elapsed}"
  - `Transcript.worked_plain` "Worked" / "已工作"
  - `Tool.read` Read / 读取
  - `Tool.edit` Edit / 编辑
  - `Tool.delete` Delete / 删除
  - `Tool.move` Move / 移动
  - `Tool.search` Search / 搜索
  - `Tool.execute` Run / 执行
  - `Tool.fetch` Fetch / 获取
  - `Tool.think` Think / 思考
  - `Tool.other` Tool / 工具

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test transcript`
Expected: 全部通过。

---

### Task 3: 后端和注册表：prompt 内容块、队列操作、新会话直连、目录回写

**Files:**
- Modify: `crates/desktop/src/backend.rs`、`crates/desktop/src/session.rs`、`crates/desktop/src/ui/mod.rs`
- Test: `crates/desktop/tests/backend.rs`、`crates/desktop/tests/session.rs`

**Interfaces:**
- Consumes：Task 1 的 `set_pending` 和 `ProjectionChange.queue`。
- Produces：
  - `AcpClient::prompt(&self, id: &SessionId, blocks: Vec<wire::ContentBlock>) -> Result<(), BackendError>`：签名从 `text: &str` 改成内容块。M1a 的调用方同步改掉。M2 发图片时也走这个接口。
  - `AcpClient::pending_messages(&self, id) -> Result<protocol::PendingMessages, BackendError>`（`AcpExtensionMethod::PendingMessages`，参数 `{"sessionId"}`）
  - `AcpClient::promote_pending(&self, id, queue_id: &protocol::QueueId) -> Result<bool, BackendError>`（`PendingMessagePromote`，参数 `{"sessionId","queueId"}`，读取响应里的 `promoted`）
  - `AcpClient::remove_pending(&self, id, queue_id: &protocol::QueueId) -> Result<(), BackendError>`（`PendingMessageRemove`）
  - `SessionRegistry::adopt(&mut self, id: &SessionId, cwd: &Path, cx) -> Entity<SessionHandle>`：给刚用 `session/new` 建好的会话直接建 handle，`phase = Attached`，不做回放。新会话在服务端已经被这条连接 watch 着，后续事件会正常流进来。
  - `attach` 在 `session_running` 之后、`resume_with_replay` 之前，调用 `pending_messages` 并 `set_pending`。这一步失败只打 `tracing::warn!`，不影响 attach。
  - `RegistryEvent::SessionChanged(SessionId)`：`apply_batch` 里某个会话的 change 带 `title`，或者 `run_state` 变成 Idle 时发出，每批每个会话最多一次。
  - `AppView` 订阅这个事件后调用 `refresh_catalog`。agent 设置的标题因此会出现在侧栏和 Tab 上，修改时间排序也会更新。

- [ ] **Step 1: 写失败的测试**

```rust
// tests/backend.rs
#[test] fn pending_queue_round_trip()
    // prompt "slow"; prompt "hello" while running → pending_messages().follow_up.len()==1;
    // promote_pending(q) == true → steering.len()==1; remove_pending(q) → both empty
// tests/session.rs
#[test] fn adopted_session_streams_without_replay()
    // new_session + registry.adopt → phase Attached; prompt "hello" → projection contains "Hi there"
#[test] fn attach_loads_the_pending_queue()
    // session with a queued follow-up (prompt "slow" then "hello" via client, before attaching) → after attach, handle.projection().pending().follow_up.len()==1
#[test] fn title_and_settle_emit_session_changed()
    // subscribe; prompt "hello" → exactly one SessionChanged(id) once idle within a batch
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test backend --test session`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test backend --test session`
Expected: 全部通过。

---

### Task 4: `SessionView`：transcript 渲染

**Files:**
- Create: `crates/desktop/src/ui/session_view.rs`（实体、scroller、缓存、同步）
- Create: `crates/desktop/src/ui/rows.rs`（各类行的渲染方法，`impl SessionView`）
- Modify: `crates/desktop/src/ui/mod.rs`、`crates/desktop/src/ui/card.rs`、`crates/desktop/src/assets.rs`、`crates/desktop/locales/app.yml`
- Modify: `crates/desktop/tests/support/shell.rs`（新增 `live(cx, state) -> Live`：启动 `ScriptedBackend`，把它的 `events` 交给 `view.set_backend`，返回 `Live { handle, view, backend, _dir }`）
- Test: `crates/desktop/tests/shell_transcript.rs`

**Interfaces:**
- Consumes：Task 2 的 `TranscriptLayout` 和 `LayoutDiff`；Task 3 的 `SessionRegistry`；M1a 的 `TabEntry`。
- Produces：
  - `pub enum SessionTarget { Draft { workspace: Option<PathBuf> }, Session(Entity<SessionHandle>) }`
  - `#[derive(TypedBuilder)] pub struct SessionView`，字段：
    - `target`
    - `layout: TranscriptLayout`
    - `scroller: Entity<MessageScrollerState>`
    - `markdown: MarkdownCache`
    - `diffs: HashMap<wire::ToolCallId, Entity<DiffState>>`
    - `images: HashMap<(wire::MessageId, usize), Arc<gpui::Image>>`
    - `expanded_runs: HashSet<RowKey>`
    - `open_thinking: HashSet<wire::MessageId>`
    - `open_tools: HashSet<wire::ToolCallId>`
    - `full_output: HashSet<wire::ToolCallId>`
    - `composer: Entity<Composer>`（Task 5 前先放一个空占位）
    - `_subscriptions`
  - 方法：
    - `pub fn new(target: SessionTarget, window, cx) -> Self`
    - `pub fn set_target(&mut self, target: SessionTarget, cx)`
    - `pub fn sync(&mut self, updated: &[RowKey], cx)`：重建布局 → `diff` → `scroller.splice` 和 `remeasure_items` → 同步缓存
    - `pub fn remeasure(&mut self, cx)`
    - `pub fn toggle_run(&mut self, anchor: RowKey, cx)`、`toggle_thinking`、`toggle_tool`、`toggle_full_output`：改完集合后调用 `sync`
  - `#[derive(Default)] pub struct MarkdownCache { entries: HashMap<(wire::MessageId, MarkdownPart), (Entity<TextViewState>, usize)> }`，其中 `MarkdownPart { Body, Thinking }`。方法 `pub fn sync(&mut self, id, part, text: &str, cx: &mut App) -> Entity<TextViewState>`：第一次见到时创建 `TextViewState::markdown`；新文本以已推送的前缀开头时 `push_str` 追加增量；否则 `set_text`。
  - `AppView` 新增 `views: HashMap<TabEntry, Entity<SessionView>>`（`TabEntry` 加 `Hash`）。`render_card_body` 改成 `&mut self`，按需为活动 Tab 创建视图，因为 `Textarea` 需要 `Window`。关闭 Tab 时移除对应的视图。
  - `SessionView` 订阅 handle 的 `ProjectionChange`，用 `inserted ∪ updated` 调 `sync`。MessageScroller 的 renderer 写成 `move |ix, window, cx| view.update(cx, |view, cx| view.render_row(ix, window, cx))`。所有缓存都在 `sync` 里建好，`render_row` 只读取。如果 renderer 运行时遇到实体被重入借用，就改成 `view.read(cx)` 读取，点击回调用捕获的 `WeakEntity`。

**各类行的渲染**（具体数值见 Global Constraints）：

| 行 | 渲染 |
| --- | --- |
| 用户 Prompt | 右对齐气泡，Markdown 用 `TextView`。图片按 `ImageBlock` 解码（`base64` → `Image::from_bytes(ImageFormat::from_mime_type(..))`），缩略图最大 `rems(6)`，id 为 `image:{messageId}:{n}`；点击后用 `open_dialog` 显示大图。解码失败就不显示该图 |
| 用户执行的命令（`UserBash`） | 单行工具框样式：`square-terminal` 图标、`Transcript.you_ran` 标签，等宽的第一行文本；点击展开完整文本 |
| 助手 | 有 reasoning 时，先显示 thinking 头：`chevron-right` 和 `brain` 图标，文案 `Transcript.thinking_for`（带 `Elapsed(duration_ms)`，没有耗时就用 `Transcript.thinking`）。流式输出中、还没有正文时，改用 `ShimmerText(Transcript.thinking_now)`。展开后用 muted 文字的 `TextView` 显示 reasoning。正文用 `TextView`，不加气泡；流式输出中在末尾追加 muted 的 `▍` |
| 工具 | 图标、动词（`category.verb()`）、等宽的 `target()`（没有就用 `title`）、右侧状态：<br>• Running：`Spinner` 加 `Elapsed(now − timestamp_ms)`<br>• Completed：success 色的 `check`。edit 显示 `+a`（success）和 `−d`（danger）；read 显示 `Tool.lines`；其余只显示 ✓<br>• Failed：danger 色的 `circle-x` 加 `Tool.failed`<br>• Cancelled：`Tool.cancelled`<br>• Blocked：`ban` 加 `Tag` 形式的 `Tool.blocked`<br>• Pending：`Tool.pending`<br>按 `GroupPosition` 决定圆角和顶部 hairline：First 画上圆角，Last 画下圆角，Only 画全部圆角，Middle 不画，First 以外的行都画顶部 hairline。点击整行展开 |
| 工具展开 | edit 用 `Diff::new(&state)`，限高 `max_h(rems(20))`，`DiffState` 由 `DiffFile::parse(patch)` 生成并按 id 缓存；解析失败时退回等宽显示 patch 文本。execute 显示等宽输出，默认只显示尾部 20 行，加一个 `Tool.full_output` / `Tool.show_less` 开关。其余类型显示 pretty 格式的 `raw_input` JSON，加上输出的前 20 行 |
| Run 摘要 | 一行 muted 文字：`chevron-right` 图标加 `RunSummary::label()`。点击后 `toggle_run(anchor)` |

- MessageScroller 打开 `jump_button(true)`，按钮文案用 `with_jump_button_label(t!("Transcript.jump_latest"))`，并设置 `with_bottom_fade(cx.theme().background)`。
- Attaching 状态继续显示 M1a 的 Skeleton。
- locale（en / zh-CN）：
  - `Transcript.thinking` Thinking / 思考
  - `Transcript.thinking_for` "Thinking · %{elapsed}" / "思考 · %{elapsed}"
  - `Transcript.thinking_now` "Thinking…" / "思考中…"
  - `Transcript.you_ran` "You ran" / "你执行的"
  - `Transcript.jump_latest` "Jump to latest" / "回到最新"
  - `Tool.lines` "%{count} lines" / "%{count} 行"
  - `Tool.failed` Failed / 失败
  - `Tool.cancelled` Cancelled / 已取消
  - `Tool.blocked` Blocked / 已拦截
  - `Tool.pending` Pending / 等待中
  - `Tool.full_output` "Full output" / "完整输出"
  - `Tool.show_less` "Show less" / "收起"

- [ ] **Step 1: 写失败的测试**。用 `Live` 加脚本模型，由测试直接调用 `backend.client.prompt` 发 prompt，再用 `eventually` 等待渲染。

```rust
#[gpui_kit::test] fn replayed_and_streamed_messages_render()
    // session with prior "hello" turn; open tab → shows "hello" and "Hi there"; send "think" → shows "done" and "Thinking · …" header
#[gpui_kit::test] fn thinking_is_collapsed_until_clicked()
    // after "think": !shows("pondering"); click "thinking:{id}" → shows("pondering")
#[gpui_kit::test] fn finished_run_folds_and_expands()
    // send "read" → idle → window.find("run:message:{prompt id}") visible, "tool:read-1" absent;
    // click run row → "tool:read-1" visible and shows "notes.txt"
#[gpui_kit::test] fn edit_tool_shows_line_stats()
    // send "edit", expand run → shows "+1" and "−1"
#[gpui_kit::test] fn exec_output_shows_the_tail_until_expanded()
    // send "exec30", expand run and tool:exec-30 → shows "30" and "11", !shows a row for "10"; click full output → shows "1"
#[gpui_kit::test] fn markdown_cache_appends_and_resets()
    // MarkdownCache::sync("a") then ("ab") → same entity, text "ab"; then ("x") → text "x"
#[gpui_kit::test] fn user_images_render_as_thumbnails()
    // prompt blocks [Text "img", Image(1×1 PNG base64)] → window.find("image:{id}:0") visible
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_transcript`
Expected: 编译失败。

- [ ] **Step 3: 实现**。缺少的图标（`Brain`、`FileText`、`Pencil`、`Trash2`、`ArrowRightLeft`、`SquareTerminal`、`Globe`、`Wrench`、`Ban`、`CircleX`、`ChevronRight` 中默认图标集没有的那些）补进 `ExtraIcons`。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_transcript`
Expected: 全部通过。

---

### Task 5: Composer：发送、停止、队列、新会话和键位

**Files:**
- Create: `crates/desktop/src/ui/composer.rs`
- Modify: `crates/desktop/src/ui/session_view.rs`、`crates/desktop/src/ui/mod.rs`、`crates/desktop/src/actions.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_composer.rs`

**Interfaces:**
- Consumes：Task 3 的 `prompt`、`pending_messages`、`promote_pending`、`remove_pending`、`adopt`；Task 4 的 `SessionView` 和 `SessionTarget`。
- Produces：
  - `pub struct Composer { input: Entity<TextareaState>, locale: u64, _subscriptions }`，方法：
    - `new(window, cx)`：`TextareaState::new(..).auto_grow(1, 8).submit_on_enter(true)`
    - `pub fn take_text(&mut self, window, cx) -> Option<String>`：去掉首尾空白后为空就返回 `None`，否则取出文本并清空输入框
    - `pub fn restore(&mut self, text: &str, window, cx)`
    - `pub fn focus(&self, window)`
  - `pub enum ComposerEvent { Submit, Stop }`：`Composer` 实现 `EventEmitter<ComposerEvent>`。`InputEvent::PressEnter { secondary: false, shift: false }` 发出 `Submit`（Shift+Enter 由 gpui-kit 插入换行）。key context `"Composer"` 里按 `escape` 发出 `Stop`。
  - 占位符：草稿 Tab 用 `Composer.placeholder_new`，会话 Tab 用 `Composer.placeholder`。渲染时如果 `LocaleGeneration` 变了，就重新设置。
  - `SessionView` 处理 `Submit`：
    - 会话 Tab：`take_text` 后调用 `client.prompt(id, vec![Text])`。运行中也照样发，后端按 `when_busy`（默认 follow-up）放进队列。失败时把文字 `restore` 回去，并推送 `Composer.send_failed` 错误通知。
    - 草稿 Tab：发出 `SessionViewEvent::CreateSession { text }`，交给 `AppView`。
  - `SessionView` 处理 `Stop`：运行中才 `client.cancel(id)`，空闲时什么都不做。输入框里的 Esc 由 gpui-kit 先关闭弹层；如果 Textarea 自己吃掉了 escape，就把 `Stop` 绑在 `SessionView` 的 key context 上，由测试来确定最终方案。
  - `AppView::create_session(&mut self, view: &Entity<SessionView>, text: String, window, cx)`：
    1. 取草稿的工作区。没有工作区时，发送按钮处于禁用状态，并显示 `Composer.need_workspace` 提示，所以这里不会走到。
    2. 调用 `client.new_session(cwd)`。
    3. 成功：`registry.adopt` → `catalog.upsert`（标题为空，创建和修改时间取当前时间）→ 把 `tabs[i]` 从 Draft 换成 `Session(id)` 并给 `views` 换 key → `view.set_target(Session(handle))` → `prompt` → `persist`。
    4. 失败：`restore(text)`，Tab 保持草稿，推送 `Composer.create_failed` 通知。
  - 队列 chip 在输入框上方，`follow_up` 在前，`steering` 在后。每个 chip 显示种类标签（`Composer.follow_up` / `Composer.steering`）和消息文本的第一行（截断显示）。follow-up chip 带一个 `Composer.promote` 按钮（调用 `promote_pending`）；所有 chip 都带一个 `x` 按钮（`Composer.remove`，调用 `remove_pending`）。操作失败时推送通知。
  - 草稿 Tab 的 composer 上方有工作区选择条：`folder` 图标、工作区名，后面是 muted 的路径。点击后用 `DropdownMenu` 列出所有已注册的工作区，选中后更新 `TabEntry::Draft { workspace }`，同时给 `views` 换 key。分支显示留到 M3。
  - 底栏只有发送按钮：`id("composer-send")`；运行中换成 `id("composer-stop")`，显示 `square` 图标。tooltip 分别是 `Composer.send`（Enter）和 `Composer.stop`（Esc）。
  - `actions!`：新增 `FocusComposer`，绑定 `primary!("l")`，作用是聚焦活动视图的 composer。
- locale（en / zh-CN）：
  - `Composer.placeholder` "Continue the task" / "继续描述任务"
  - `Composer.placeholder_new` "Describe the task" / "描述任务"
  - `Composer.send` Send / 发送
  - `Composer.stop` Stop / 停止
  - `Composer.follow_up` Follow-up / 稍后发送
  - `Composer.steering` Steering / 本轮补充
  - `Composer.promote` "Move to this turn" / "改为本轮补充"
  - `Composer.remove` Remove / 移除
  - `Composer.need_workspace` "Add a workspace to start" / "先添加一个工作区"
  - `Composer.create_failed` "Could not create the session" / "无法创建会话"
  - `Composer.send_failed` "Could not send the message" / "无法发送消息"

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test] fn enter_sends_and_clears()              // focus composer, input "hello", press enter → eventually shows "Hi there"; composer value == ""
#[gpui_kit::test] fn shift_enter_inserts_a_newline()       // input "a", press "shift-enter", input "b" → value "a\nb", no prompt sent (projection empty)
#[gpui_kit::test] fn blank_text_is_not_sent()              // input "   ", enter → projection stays empty, value unchanged
#[gpui_kit::test] fn enter_while_composing_only_commits()  // TestInput::compose_text(None, "nihao", None) then press enter → marked range None, nothing sent
#[gpui_kit::test] fn enter_while_running_queues_a_follow_up()
    // send "slow"; while running input "hello" + enter → chip "Follow-up" visible; after idle chip gone and "Hi there" shown
#[gpui_kit::test] fn chip_actions_promote_and_remove()     // queued chip → click promote → label "Steering"; click remove → chip gone
#[gpui_kit::test] fn escape_stops_only_a_running_session()
    // idle: press escape → no cancel (state stays Idle, no error); running "slow": escape → eventually Idle and the reply "late" never shows
#[gpui_kit::test] fn draft_first_message_creates_the_session()
    // ctrl-n in workspace = fixture dir, input "hello", enter → tabs() == [Session(new id)], catalog has it, shows "Hi there"
#[gpui_kit::test] fn draft_send_failure_keeps_the_text()
    // close the fixture connection, then send from a draft → tabs still [Draft], composer value "hello"
#[gpui_kit::test] fn draft_without_workspace_disables_send() // no workspaces → window.find("composer-send").disabled() == Some(true), shows "Add a workspace to start"
#[gpui_kit::test] fn ctrl_l_focuses_the_composer()         // focus sidebar search, press ctrl-l → composer input focused
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_composer`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_composer`
Expected: 全部通过。

---

### Task 6: 顶栏运行耗时、侧栏筛选、失败反馈、相对时间刷新

**Files:**
- Modify: `crates/desktop/src/ui/card.rs`、`crates/desktop/src/ui/sidebar.rs`、`crates/desktop/src/ui/dialogs.rs`、`crates/desktop/src/ui/mod.rs`、`crates/desktop/src/catalog.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell.rs`、`crates/desktop/tests/catalog.rs`、`crates/desktop/tests/shell_actions.rs`

**Interfaces:**
- Consumes：Task 1 的 `run_started_ms`；Task 2 的 `Elapsed`。
- Produces：
  - 顶栏状态：Running 时显示 `Status.running_for`，带 `Elapsed(now − run_started_ms)`；没有开始时间就只显示 `Status.running`。Waiting 和 Idle 的显示不变。
  - `AppView` 新增 `_ticker: Task<()>`，在 `new` 里启动：每秒检查一次，只有活动 Tab 正在运行、或者可见 transcript 里有 Running 的工具时才 `cx.notify()`；另外每 60 秒无条件 `notify` 一次，让侧栏的相对时间刷新。
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub enum SessionSort { #[default] Modified, Created }`（放在 `catalog.rs`）。`SessionCatalog::for_workspace(&self, ws, sort)` 和 `recent(&self, limit, sort)` 都加上 `sort` 参数，M1a 的调用方同步改掉。按 Created 排序时，缺少创建时间的会话排在最后。
  - `AppView` 新增 `sort: SessionSort` 和 `running_only: bool`，都不持久化。侧栏标题行在搜索按钮前面加一个筛选按钮（`ListFilter`，`Button::ghost().small()`，tooltip 为 `Sidebar.filter`），点开是 `DropdownMenu`，里面有三个可勾选项：`Sidebar.sort_modified`、`Sidebar.sort_created`、`Sidebar.running_only`。勾选 `running_only` 时，`sidebar_model` 只保留 `session_status` 为 Running 或 Waiting 的会话行，工作区行仍然保留。
  - 重命名和删除失败时调用 `window.push_notification`，推送 error 通知 `Session.rename_failed` / `Session.delete_failed`。删除失败时，会话重新出现在目录里，由 `refresh_catalog` 恢复。
- locale：
  - `Status.running_for` "Running · %{elapsed}" / "运行中 · %{elapsed}"
  - `Sidebar.filter` Filter / 筛选
  - `Sidebar.sort_modified` "Sort by modified" / "按修改时间"
  - `Sidebar.sort_created` "Sort by created" / "按创建时间"
  - `Sidebar.running_only` "Running only" / "只看运行中"
  - `Session.rename_failed` "Could not rename the session" / "无法重命名会话"
  - `Session.delete_failed` "Could not delete the session" / "无法删除会话"

- [ ] **Step 1: 写失败的测试**

```rust
// catalog.rs
#[test] fn sort_by_created_orders_and_puts_unknown_last()  // c(created 5), d(created 9), e(None) → [d, c, e]
// shell.rs
#[gpui_kit::test] fn running_only_hides_idle_sessions()   // two sessions, one handle Running (Live backend "slow") → model rows only the running one
#[gpui_kit::test] fn header_shows_running_elapsed()        // Live "slow" → shows text starting with "Running · "
// shell_actions.rs
#[gpui_kit::test] fn failed_rename_pushes_a_notification() // closed connection → commit rename → shows "Could not rename the session"
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test catalog --test shell --test shell_actions`
Expected: 编译失败，或断言失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test catalog --test shell --test shell_actions`
Expected: 全部通过。

---

### Task 7: 设置页（通用、外观）、Ctrl+, 和轨道入口

**Files:**
- Create: `crates/desktop/src/ui/settings.rs`
- Modify: `crates/desktop/src/ui/mod.rs`、`crates/desktop/src/ui/card.rs`、`crates/desktop/src/ui/rail.rs`、`crates/desktop/src/ui/title_bar.rs`、`crates/desktop/src/actions.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_settings.rs`、`crates/desktop/tests/shell_locale.rs`

**Interfaces:**
- Consumes：M1a 的 `LanguagePreference::apply`、`AppearancePreference::apply`、`ThemeRegistry`；Task 4 的 `SessionView::remeasure`。
- Produces：
  - `TabEntry::Settings`：单例 Tab。不写进 `desktop.json` 的 tabs。顶栏不显示运行状态，也不显示会话菜单。Tab 标题为 `Settings.title`。
  - `AppView::open_settings(&mut self, cx)`：已经打开就激活，否则在末尾追加。`actions!` 新增 `OpenSettings`，绑定 `primary!(",")`。File 菜单新增「设置」项，轨道底部（`flex_1` 空白之后）新增 `Settings2` 按钮（`Button::ghost()`，tooltip 为 `Settings.title`，并写出快捷键）。
  - `AppView::set_language(&mut self, language: LanguagePreference, cx)`、`AppView::set_appearance(&mut self, appearance: AppearancePreference, cx)`：写进 `state`，然后 `apply`（外观用 `cx.window_appearance()`），然后 `persist`，最后对 `views` 里每个视图调用 `remeasure`。
  - 设置页内容是 `Settings::new("settings").pages([..])`，分两页：
    - **通用**（`Settings.general`）：语言，`SettingField::dropdown`，选项为 `system` → `Settings.follow_system`、`en` → `English`、`zh-CN` → `简体中文`。
    - **外观**（`Settings.appearance`）：
      - 模式：dropdown，选项 `system` / `light` / `dark`，文案 `Settings.follow_system` / `Settings.light` / `Settings.dark`。
      - 浅色主题：`scrollable_dropdown`，列出 `ThemeRegistry::sorted_themes()` 里 `mode == Light` 的主题名。
      - 深色主题：同上，`mode == Dark`。
      - 界面字号：`SettingField::number_input`，参数 `NumberFieldOptions { min: 12.0, max: 20.0, step: 1.0 }`。
    - 字段的读写闭包捕获 `WeakEntity<AppView>`，取值时 `read`，设值时 `update` 并调用上面的两个方法。
- locale：
  - `Settings.title` Settings / 设置
  - `Settings.general` General / 通用
  - `Settings.appearance` Appearance / 外观
  - `Settings.language` Language / 语言
  - `Settings.follow_system` "Follow system" / "跟随系统"
  - `Settings.mode` Mode / 模式
  - `Settings.light` Light / 浅色
  - `Settings.dark` Dark / 深色
  - `Settings.light_theme` "Light theme" / "浅色主题"
  - `Settings.dark_theme` "Dark theme" / "深色主题"
  - `Settings.font_size` "Font size" / "界面字号"
  - `Menu.settings` Settings / 设置

- [ ] **Step 1: 写失败的测试**

```rust
// shell_settings.rs
#[gpui_kit::test] fn ctrl_comma_opens_one_settings_tab()  // press ctrl-, twice → tabs().iter().filter(Settings).count()==1, shows "Appearance"
#[gpui_kit::test] fn appearance_changes_apply_and_persist()
    // set_appearance(dark theme "Catppuccin Mocha", mode Dark) → cx.theme().theme_name()=="Catppuccin Mocha"; after save, desktop.json has it
#[gpui_kit::test] fn font_size_change_remeasures_rows()
    // Live: two messages rendered; record find("message:a").bounds(); set font 20 → message:a height grows and message:b top >= message:a bottom
#[gpui_kit::test] fn settings_tab_is_not_persisted()      // open settings → state().tabs.open excludes it
// shell_locale.rs
#[gpui_kit::test] fn language_setting_switches_the_ui()   // set_language(ZhCn) → shows "设置"; set_language(En) → shows "Settings"
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell_settings --test shell_locale`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell_settings --test shell_locale`
Expected: 全部通过。

---

### Task 8: 视觉还原验收和全量验证

**Files:** 只修复验收中发现的问题，不新增功能。

- [ ] **Step 1: 静态检查和测试**

Run: `cargo fmt --all && cargo clippy --workspace --tests --examples -- -Dwarnings && cargo test -p desktop -p acp -p app`
Expected: 没有 warning；全部测试通过。

- [ ] **Step 2: pre-commit**

Run: `pre-commit run --all-files`
Expected: 全部 Passed。

- [ ] **Step 3: 真机运行**

用生产 `app` 后端加已配置的 provider（规则 9，不使用 fixture）跑一遍，用 `run` skill 启动 `clawcode-desktop`。依次完成：
1. 在一个工作区里新建会话。
2. 让 agent 读取、编辑一个文件，并执行一条命令。
3. 运行中再发一条消息，确认出现「稍后发送」chip。
4. 按 Esc 停止一次运行。
5. 重启后打开旧会话，确认回放和 Run 折叠正确。

- [ ] **Step 4: 和设计稿并排截图对照**

截图覆盖下面这些组合：
- Default Light 加简体中文
- Catppuccin Mocha 加 English
- 最小窗口 960×640
- 字号 16 和 18 各一张

每张都和 mockup 的同一区域并排比较：
- 标题栏
- 侧栏
- Tab 条
- 用户气泡
- thinking 折叠行
- 工具分组框
- composer

逐项检查以下几点，发现偏差就当场修：
- 圆角档位
- hairline
- 间距
- 字号层级（只有四档）
- muted 色
- 图标尺寸（只有两档）
- 选中底色

mockup 里属于 M2 的元素（计划模式、上下文环、模型选择、附件）不参与对比，在结论里注明。

- [ ] **Step 5: 自查**

对照设计稿第 15 节 M1 的范围，逐条检查本计划的 Global Constraints 和 Review Focus。结论和截图路径写进 `.superpowers/sdd/2026-11-19-desktop-m1b-transcript-composer-settings/progress.md`。

- [ ] **Step 6: 不提交**

保持工作区未提交状态，进入 M2 计划。

# 内置 Plan 模式实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal：** 会话可切换到只读的 plan 模式；模型调用 `exit_plan_mode` 提交计划，用户在 WebUI 批准后自动切回 Default 并执行；批准的计划与 `todo_write` 待办以 ACP 计划更新显示在计划面板中。

**Architecture：** Kernel 持有会话模式（`custom` 条目持久化）、在工具批次中按模式拦截调用、以历史中的说明消息告知模型；`exit_plan_mode` 仿照 MCP elicitation 的待处理表发起审批；ACP 把模式暴露为 `mode` 配置项，把审批转成 `session/request_permission`，并从工具结果详情推导 `plan_update`；WebUI 增加权限弹窗与计划面板。

**Tech Stack：** Rust（agent-client-protocol 5 + `unstable_plan_operations`、shlex、tokio）、React 19、zustand、CDP 端到端脚本。

**Spec：** [内置 Plan 模式设计规格](../specs/2026-11-05-plan-mode-design.md)

## Global Constraints

- 未经用户明确允许不执行 `git commit`，执行期间也不运行 `git add`、`git reset`、`git stash`；本计划没有逐任务提交步骤，全部完成后由用户决定是否作为一个提交放在 main 上。
- 项目规则：注释用英文，每个新函数有函数级注释；超过 3 个字段的结构体用 `typed-builder`；克隆 `Arc` 写 `Arc::clone(&x)`；tracing 宏用全路径且不用字段语法；测试只放 crate 的 `tests/` 或 `#[cfg(test)] mod tests`；依赖经 root `[workspace.dependencies]` 声明并按语义分组；共享上下文的操作做成结构体方法。
- 只有 protocol、provider、extension 可以直接依赖 rig（`scripts/check-rig-boundary.py`）。
- 固定取值（逐字使用）：
  - 持久化条目：`type = custom`，`extensionId = "clawcode"`，`customType = "session_mode"`，`data = {"mode": "default" | "plan", "turnId": "<turn id>"}`
  - ACP 配置项：`configId = "mode"`，名称 `Mode`，类别 `Mode`，选项 `default`/`Default`、`plan`/`Plan`，排在 `model`、`thinking_level` 之后
  - 保留工具名：`exit_plan_mode`、`todo_write`
  - planId：`plan-mode`（`markdown`）、`todos`（`items`）
  - 权限请求：`title = "Ready to execute the plan?"`；选项 `approve`（`allow_once`，`Approve and execute`）、`keep_planning`（`reject_once`，`Keep planning`）
  - 工具结果文本：批准 `The user approved the plan. Plan mode has ended; you can now edit files. Carry out the plan.`；继续规划 `The user chose to keep planning. Stop and wait for their feedback.`
  - 错误文本：`Plan mode is not active.`；拦截 `Plan mode is active: {tool} is not allowed. Finish the plan and call exit_plan_mode.`
  - 退出说明：`Plan mode has ended; you may edit files now.`
- 日志不得包含计划内容、待办文本、完整 bash 命令。
- WebUI 不写单元测试；端到端测试使用生产 `app` 后端与 `claw.toml` 中配置的 provider，不引入 fixture 后端。

## Review Focus

1. **流式输出中切换模式**：助手消息还在流式生成时切到 Plan，说明消息应排在这条助手消息之前，重新打开会话后位置不变。测试见 Task 3。
2. **工具调用后没有结果**：运行中断导致工具调用没有结果，之后用户发新消息；推迟的说明消息必须在新用户消息之前出现，不能丢失。测试见 Task 3。
3. **同一批里两次 `exit_plan_mode`**：第二次返回“已有待审批”错误，第一次正常审批。测试见 Task 5。
4. **等待审批时用户手动切回 Default 再点“继续规划”**：模式保持用户最后的选择（Default），不被强制改回 Plan。测试见 Task 5。
5. **`git --no-pager log`**：按 spec 拒绝全局选项，错误原因写明不接受 git 全局选项，模型可据此改写命令。测试见 Task 1。

---

### Task 1: 共享命令解析与只读白名单

**Files:**
- Create: `crates/tools/src/command_line.rs`
- Modify: `crates/tools/src/lib.rs`、`extensions/src/available/command_guard.rs`、`extensions/Cargo.toml`
- Test: `crates/tools/tests/command_line.rs`

**Interfaces:**
- Produces（`tools`）：
  - `pub struct ShellCommandLine`，`impl From<&str> for ShellCommandLine`（宽松解析，无法解析的段被丢弃，供 `command-guard` 沿用原行为）
  - `ShellCommandLine::invocations(&self) -> &[CommandInvocation]`
  - `ShellCommandLine::read_only(command: &str) -> Result<(), ReadOnlyViolation>`（严格：任一段解析失败即违规）
  - `pub struct CommandInvocation { pub executable: String, pub arguments: Vec<String> }`，`CommandInvocation::program(&self) -> &str` 返回 basename
  - `pub enum ReadOnlyViolation { Unparsable, Substitution, Redirect, Program(String), Argument { program: String, argument: String }, GitGlobalOption(String) }`，`Display` 只写原因类别与程序名/参数名，不复述整条命令；`argument` 只保存参数名（`--output=/x` 记为 `--output`，`-oFILE` 记为 `-o`），不带值。例如 `output redirection is not allowed in plan mode`、`git global options are not allowed in plan mode: --no-pager`

- [ ] **Step 1：写失败测试**

`crates/tools/tests/command_line.rs`，表驱动两个测试：

```rust
#[test]
fn read_only_rejects_writes_and_execution() {
    for command in [
        "ls; rm x", "cat a > b", "echo \"$(rm x)\"", "echo `rm x`", "diff <(ls) a",
        "find . -delete", "find . -exec rm {} ;", "env rm x", "xargs rm",
        "git branch -D x", "git diff --output=x", "git -c core.pager=x log",
        "git --no-pager log", "git diff --ext-diff", "git grep -O x", "git push",
        "rg --pre=sh x", "rg --pre sh x", "fd -x rm", "fd --exec-batch rm",
        "sort -o f a", "sort -of a", "sort --output=f a", "tree -o f", "uniq a b",
        "file -C", "sed -i s/a/b/ f", "tee f", "cargo test", "printenv", "ls 'unclosed",
    ] {
        assert!(ShellCommandLine::read_only(command).is_err(), "{command}");
    }
}

#[test]
fn read_only_allows_inspection() {
    for command in [
        "cd src && ls", "git log | head", "git status", "git branch --show-current",
        "git remote -v", "grep \">\" f", "ls 2>&1", "ls >/dev/null 2>&1 || ls",
        "uniq a", "rg TODO src", "find . -name '*.rs'", "jq . a.json", "pwd",
    ] {
        assert_eq!(ShellCommandLine::read_only(command), Ok(()), "{command}");
    }
}
```

另加 `violation_names_the_reason_not_the_command`：`ShellCommandLine::read_only("cat secret > out")` 的 `to_string()` 不含 `secret`；`sort --output=/tmp/secret a` 的 `to_string()` 含 `--output`、不含 `/tmp/secret`；`git --no-pager log` 的 `to_string()` 含 `--no-pager`（Review Focus 5）。

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p tools --test command_line`
Expected: 编译失败，`ShellCommandLine` 不存在。

- [ ] **Step 3：实现 `crates/tools/src/command_line.rs`**

- 从 `command_guard.rs` 移入引号感知的切分（`;`、`|`、`&`、换行）与 `CommandInvocation` 的 `shlex` 解析，保留 `From<&str>` 的宽松语义。
- `read_only` 在同一次扫描中记录引号外的 `>`、`>>`、`&>`（放行 `2>&1`、`1>&2` 和目标为 `/dev/null` 的重定向）以及任意位置的 `$(`、反引号、`<(`、`>(`；然后逐段按 spec §4.3 的表格检查程序与参数。参数检查同时识别 `-o FILE`、`-oFILE`、`--output=FILE` 三种写法。
- `extensions/Cargo.toml` 把 `tools` 从 dev-dependencies 移到 `[dependencies]`（`{ workspace = true }`，放在相对路径 workspace crate 分组中按字母序）；`command_guard.rs` 改用 `tools::ShellCommandLine`、`tools::CommandInvocation`，删除本地副本，`rm -rf` 判定逻辑不变。

- [ ] **Step 4：确认通过**

Run: `cargo test -p tools --test command_line && cargo test -p extensions`
Expected: 全部通过，`command-guard` 现有测试不变。

---

### Task 2: 会话模式状态与持久化

**Files:**
- Create: `crates/protocol/src/plan.rs`、`crates/kernel/src/runtime/plan/mod.rs`
- Modify: `crates/protocol/src/lib.rs`、`crates/protocol/src/capability.rs`、`crates/kernel/src/runtime.rs`、`crates/kernel/src/runtime/session/settings.rs`、`crates/kernel/src/runtime/session/assembly.rs`
- Test: `crates/kernel/tests/protocol_alignment/plan_mode.rs`（在 `main.rs` 中加 `mod plan_mode;`）

**Interfaces:**
- Produces（`protocol::plan`，经 `lib.rs` 导出）：
  - `#[serde(rename_all = "snake_case")] pub enum SessionMode { #[default] Default, Plan }`（`Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize`）
  - `pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode"; pub const TODO_WRITE_TOOL: &str = "todo_write";`
- Produces（`protocol::SessionSettings`）：新字段 `pub mode: SessionMode`
- Produces（kernel）：
  - `pub(in crate::runtime) struct SessionPlanState`（`runtime/plan/mod.rs`），`Session` 新字段 `plan: SessionPlanState`；方法 `mode(&self) -> Result<SessionMode, KernelError>`、`replace_mode(&self, mode: SessionMode) -> Result<(), KernelError>`
  - `SettingChange::Mode { mode: SessionMode, turn_id: TurnId }`，编码为 Global Constraints 中的 `custom` 条目；解码时 `mode` 不认识或缺失、`turnId` 缺失或非法都返回“跳过”，而不是错误。`turnId` 是在 spec §3.2 示例之外新增的字段，用于让重建出的说明消息与运行中的身份一致（Task 3）
  - `SessionSettingsUpdate::mode(&self, mode: SessionMode) -> Result<SessionMode, KernelError>`
  - `pub async fn Kernel::set_session_mode(&self, session_id: &SessionId, mode: SessionMode) -> Result<SessionMode, KernelError>`

- [ ] **Step 1：写失败测试**（`plan_mode.rs`，使用 `fixture::TestKernel`）

| 测试名 | 断言 |
|---|---|
| `mode_change_is_persisted_and_restored` | `set_session_mode(.., Plan)` 后 `session_settings().mode == Plan`；会话文件中有一条 `type == "custom"`、`extensionId == "clawcode"`、`customType == "session_mode"`、`data.mode == "plan"` 的条目；再次设置 `Plan` 不新增条目；重新打开会话后仍为 `Plan`；记录到 `SettingsChanged` 事件 |
| `mode_follows_the_branch` | 在条目 A 处为 Default，之后切到 Plan；`navigate` 回 A 后模式为 Default |
| `foreign_or_unknown_mode_entries_are_skipped` | 手工追加 `extensionId = "other"` 的同名条目、`data.mode = "review"` 的条目、缺少 `turnId` 的条目（写入会话文件后重新打开）：会话正常打开，模式保持前一个有效值 |

- [ ] **Step 2：确认失败**

Run: `cargo test -p kernel --test protocol_alignment plan_mode`
Expected: 编译失败，`set_session_mode` 不存在。

- [ ] **Step 3：实现**

- `SessionPlanState` 用 `RwLock<SessionMode>` 保存模式；`Session::settings()` 填入 `mode`。
- `SessionSettingsUpdate::mode` 与 `thinking_level` 同构（加锁、相同值直接返回、先 `persist` 再改内存、`publish`、`tracing::info!("changed mode for Session {} to {:?}", ..)`），说明消息的写入留给 Task 3。
- `assembly.rs` 的设置重放额外识别 `EntryKind::Custom`：只有 `extensionId == "clawcode"` 且 `customType == "session_mode"` 的条目参与，其余 `custom` 条目照旧跳过；不认识的 `mode` 记 `tracing::warn!` 后跳过。

- [ ] **Step 4：确认通过**

Run: `cargo test -p kernel --test protocol_alignment`
Expected: 全部通过。

---

### Task 3: 模式说明消息

**Files:**
- Create: `crates/kernel/src/runtime/plan/note.rs`
- Modify: `crates/kernel/src/runtime/plan/mod.rs`、`crates/kernel/src/runtime/session/settings.rs`、`crates/kernel/src/runtime/session/transcript.rs`、`crates/kernel/src/runtime/session/replay.rs`、`crates/kernel/src/runtime/compaction.rs`、`crates/kernel/src/runtime/session/ownership.rs`
- Test: `crates/kernel/tests/protocol_alignment/plan_mode.rs`

**Interfaces:**
- Consumes：Task 2 的 `SessionMode`、`SettingChange::Mode`、`SessionPlanState`。
- Produces：
  - `pub(in crate::runtime) struct ModeNote { mode: SessionMode, delegated: bool }`；`ModeNote::message(&self, message_id: MessageId, turn_id: TurnId, at: TimestampMs) -> AgentMessage` 生成 `MessageContent::Extension`（`extension_id = "clawcode"`，`custom_type = "session_mode"`，`display = false`，`include_in_context = true`）；`ModeNote::is_note(message: &AgentMessage) -> bool`
  - 说明文本三个常量：进入 plan（普通）、进入 plan（子代理）、退出（Global Constraints 中的固定文本）。进入文本按 spec §4.4 撰写。
  - 放置规则（实时与重建共用一个实现）：历史尾部存在“有工具调用但结果未齐”的助手消息时，说明消息推迟；当这组结果齐全，或下一条非 `ToolResult` 消息即将追加时，先放出推迟的说明消息。
  - `SessionTranscript::append_mode_note(&self, entry: NewEntry, note: AgentMessage) -> Result<(), KernelError>`：在同一把锁内写入 `custom` 条目并按放置规则推入或推迟说明消息；`persist_message` 路径按规则放出推迟的消息
  - 压缩：`ModeNote` 提供 `after_summary(mode_at_boundary, delegated, retained_tail: &[AgentMessage]) -> Option<ModeNote>`，仅当边界处为 plan 且保留尾部没有说明消息时返回进入说明；`compaction.rs` 与 `history_from_store` 都在摘要后插入它
  - 说明消息 ID：条目处为 `session-mode-{entry_id}`，压缩后为 `session-mode-{compaction_entry_id}`

- [ ] **Step 1：写失败测试**（`plan_mode.rs`，通过 `model.requests` 检查实际发给模型的历史）

| 测试名 | 断言 |
|---|---|
| `entering_plan_appends_a_note_without_touching_the_prefix` | Default 下一轮后切到 Plan 再发消息：第二次请求的系统提示词与工具定义与第一次逐项相等；第一次请求的全部消息是第二次请求消息的前缀；新增消息中依次是说明消息、用户消息 |
| `note_waits_for_the_tool_results` | `Route::Wait` 让模型发出 `read` 调用后阻塞在工具执行中切到 Plan：下一次请求里说明消息位于该工具结果之后；重新打开会话后重建的历史与运行中相同 |
| `note_precedes_a_streaming_reply` | Review Focus 1：助手回复流式生成中切换，说明消息位于该助手消息之前；重新打开后相同 |
| `dangling_tool_calls_do_not_swallow_the_note` | Review Focus 2：正常取消会补上取消结果，这种历史只在进程崩溃时出现，所以直接向会话文件追加一条带工具调用、没有对应结果的助手消息条目后重新打开；切到 Plan 后发消息：说明消息在新用户消息之前，重新打开后相同 |
| `compaction_keeps_the_plan_note` | 在 plan 模式下压缩且进入说明位于压缩点之前：压缩后的历史为摘要、进入说明、保留尾部；Default 下压缩不插入；重新打开后相同 |
| `subagent_inherits_plan_with_its_own_note` | 父会话为 Plan 时创建子代理：子会话 `mode == Plan`，其第一次请求中的说明消息是子代理版本（不含 `exit_plan_mode`） |

- [ ] **Step 2：确认失败**

Run: `cargo test -p kernel --test protocol_alignment plan_mode`
Expected: 新测试失败。

- [ ] **Step 3：实现**

- `SessionSettingsUpdate::mode` 改用 `append_mode_note` 写入；`data.turnId` 取事件使用的 Turn ID。
- `history_from_store` 遇到 `session_mode` 条目时用同一放置规则生成说明消息；遇到压缩条目时按 `after_summary` 插入。
- `ownership.rs` 中 `OwnedSessionSpawn` 读取 owner 的模式，为 Plan 时在子会话创建后立即写入 Plan（`delegated = true`）。

- [ ] **Step 4：确认通过**

Run: `cargo test -p kernel`
Expected: 全部通过，包括现有压缩与重放测试。

---

### Task 4: plan 模式下的工具拦截

**Files:**
- Create: `crates/kernel/src/runtime/plan/gate.rs`
- Modify: `crates/kernel/src/runtime/tools/state.rs`、`crates/kernel/src/runtime/tools/batch.rs`、`crates/kernel/src/runtime/run/turn.rs`、`crates/kernel/src/runtime/session/assembly.rs`、`crates/kernel/src/runtime/subagent/mod.rs`、`crates/kernel/src/runtime/extension/host.rs`、`crates/tools/src/builtin/read.rs`、`crates/tools/src/builtin/grep.rs`
- Test: `crates/kernel/tests/protocol_alignment/plan_mode.rs`

**Interfaces:**
- Consumes：Task 1 的 `ShellCommandLine::read_only`；Task 2 的 `SessionPlanState::mode`。
- Produces：
  - `SessionToolSnapshot` 新字段 `kernel_owned: BTreeSet<String>`；`SessionToolState::register_kernel(&self, tool: Arc<dyn AgentTool>) -> Result<(), ToolError>`（写入 extensions 分区并记入 `kernel_owned`）。`tool_search`、`subagent`、`background_subagent`（以及后续的 `exit_plan_mode`、`todo_write`）改用它注册。
  - 保留名：新增 `ToolError::ReservedName(String)`。运行中由扩展调用的 `register` 遇到 `exit_plan_mode`、`todo_write` 时返回该错误（`host.rs` 的 `register_tool` 把它转成 `ExtensionHostError` 交给扩展，会话不受影响）；会话组装时 `restored_tools` 收集静态扩展工具遇到这两个名字则跳过并 `tracing::warn!("rejected extension tool {} because the name is reserved", name)`，以免会话无法创建
  - `pub(in crate::runtime) struct PlanModeGate { mode: SessionMode, kernel_owned: BTreeSet<String> }`，在 Turn 开始时与工具快照一起捕获；`PlanModeGate::check(&self, call: &ToolCall, metadata: &ToolMetadata) -> Result<(), String>` 返回拒绝原因（spec §4.2 的三步判定；bash 的原因使用 `ReadOnlyViolation` 的文本）
  - `ToolBatch` 新字段 `gate: &'a PlanModeGate`；拦截位置在扩展 `tool_call` 钩子之后、`invalid_arguments` 检查之前，拒绝时生成 `ToolResultDetails::Blocked { reason, terminate: false }` 的错误结果
  - `read`、`grep` 的 `ToolMetadata.annotations.read_only_hint = Some(true)`

- [ ] **Step 1：写失败测试**

| 测试名 | 断言 |
|---|---|
| `plan_mode_blocks_edits_but_allows_reading` | Plan 下模型依次调用 `write`、`edit`、`read`、`grep`：前两个结果 `is_error`、详情为 `Blocked`、文本为 Global Constraints 中的拦截文本，文件未创建或未修改；后两个正常 |
| `plan_mode_runs_read_only_bash_only` | `bash` 执行 `ls` 成功；`echo hi > f` 被拒绝且文件不存在，拒绝文本含 `redirection`，不含 `echo hi`；捕获的 `tracing` 日志不含 `echo hi`（P13） |
| `read_only_hints_decide_other_tools` | 通过 `StaticExtensionFactory` 注册两个扩展工具：带 `read_only_hint = Some(true)` 的可执行，不带的被拒绝；扩展提供的同名 `subagent` 被拒绝 |
| `reserved_tool_names_are_rejected` | 静态扩展提供名为 `todo_write`、`exit_plan_mode` 的工具：会话正常创建，这两个扩展工具不在会话的工具表中，`tracing` 捕获到拒绝的 `warn`；运行中通过扩展宿主注册同名工具返回错误。“名字指向 Kernel 实现”在 Task 6 断言 |
| `default_mode_does_not_gate` | Default 下 `write` 成功 |
| `subagent_in_plan_mode_cannot_edit` | 父会话为 Plan 时创建的子代理调用 `write` 被拒绝，文件未创建（P11） |
| `tool_definitions_do_not_change_with_mode` | Default 与 Plan 下的请求工具定义完全相同，均包含 `exit_plan_mode` 与 `todo_write` |

`tool_definitions_do_not_change_with_mode` 在 Task 5、Task 6 完成前先只断言相等，两项名称的断言在 Task 6 补上。

- [ ] **Step 2：确认失败**

Run: `cargo test -p kernel --test protocol_alignment plan_mode`
Expected: 新测试失败。

- [ ] **Step 3：实现**（按 Interfaces）

- [ ] **Step 4：确认通过**

Run: `cargo test -p kernel && cargo test -p tools`
Expected: 全部通过。

---

### Task 5: `exit_plan_mode` 与审批

**Files:**
- Create: `crates/kernel/src/runtime/plan/approval.rs`
- Modify: `crates/protocol/src/plan.rs`、`crates/protocol/src/tool.rs`、`crates/protocol/src/event.rs`、`crates/kernel/src/runtime.rs`、`crates/kernel/src/runtime/plan/mod.rs`、`crates/kernel/src/runtime/tools/batch.rs`、`crates/kernel/src/runtime/session/assembly.rs`
- Test: `crates/kernel/tests/protocol_alignment/plan_mode.rs`

**Interfaces:**
- Consumes：Task 2 的 `SessionSettingsUpdate::mode`；Task 4 的 `register_kernel`。
- Produces（protocol）：
  - `pub struct PlanApprovalRequest { pub request_id: String, pub tool_call_id: ToolCallId, pub plan: String }`
  - `#[serde(rename_all = "snake_case")] pub enum PlanApprovalDecision { Approve, KeepPlanning, Cancelled }`
  - `pub struct PlanApprovalResponse { pub session_id: SessionId, pub request_id: String, pub decision: PlanApprovalDecision }`
  - `ToolResultDetails::PlanApproval { approved: bool, plan: Option<String> }`
  - `AgentEventPayload::PlanApprovalRequested { request: PlanApprovalRequest }`、`AgentEventPayload::PlanApprovalResolved { request_id: String, decision: PlanApprovalDecision }`；等待期间沿用现有 `ActionRequired`/`ActionResolved`
- Produces（kernel）：
  - `ExitPlanModeTool`（kind 由 ACP 映射为 `switch_mode`），参数 schema `{ plan: string }`
  - `SessionPlanState` 保存 `Mutex<Option<PendingPlanApproval>>`（请求与 oneshot 发送端），每会话最多一个
  - `pub async fn Kernel::respond_plan_approval(&self, response: PlanApprovalResponse) -> Result<(), KernelError>`；已结束的请求返回新变体 `KernelError::PlanApprovalResolved(String)`
  - `pub fn Kernel::plan_approval(&self, session_id: &SessionId) -> Result<Option<PlanApprovalRequest>, KernelError>`（供 ACP 恢复后重发）
  - `ToolBatchResult.terminate` 在任一结果详情为 `PlanApproval { approved: false, .. }` 时为真

- [ ] **Step 1：写失败测试**

| 测试名 | 断言 |
|---|---|
| `exit_plan_mode_requires_plan_mode` | Default 下调用：错误文本 `Plan mode is not active.` |
| `subagent_cannot_exit_plan_mode` | 子代理会话中调用：错误，文本提示交回父会话 |
| `second_exit_in_one_batch_is_rejected` | Review Focus 3：同一响应两次调用：一次发起审批，另一次错误“已有待审批” |
| `approval_switches_mode_and_continues` | 收到 `PlanApprovalRequested`（`plan` 为去掉首尾空白后的文本）与 `ActionRequired`；`respond_plan_approval(Approve)` 后：模式为 Default，`SettingsChanged` 已发出，结果文本为批准文本，详情 `PlanApproval { approved: true, plan: Some(..) }`，运行继续下一轮且该轮的 `write` 成功 |
| `keep_planning_ends_the_run` | `KeepPlanning` 与 `Cancelled` 各一次：模式为 Plan，详情 `PlanApproval { approved: false, plan: None }`，运行在本批后结束，`RunEnd` 为成功结束 |
| `manual_switch_wins_over_keep_planning` | Review Focus 4：等待审批时 `set_session_mode(Default)`，再回复 `KeepPlanning`：模式保持 Default |
| `cancel_withdraws_the_approval` | 等待中 `cancel_session`：`PlanApprovalResolved { decision: Cancelled }`，模式为 Plan |
| `late_response_is_reported_resolved` | 已结束的请求再回复：`KernelError::PlanApprovalResolved` |
| `approval_logs_omit_the_plan` | 捕获 `tracing`：日志不含计划文本 |

- [ ] **Step 2：确认失败**

Run: `cargo test -p kernel --test protocol_alignment plan_mode`
Expected: 编译失败，`PlanApprovalResponse` 不存在。

- [ ] **Step 3：实现**

审批流程照 `runtime/mcp.rs` 的 `elicit_mcp`/`resolve_mcp_elicitation`：插入待处理项 → 发 `PlanApprovalRequested` → 首个待处理项发 `ActionRequired` → `select!` 等待回复或运行取消 → 移除 → 发 `PlanApprovalResolved` 与 `ActionResolved`。批准时经 `SessionSettingsUpdate::mode(Default)` 切换（带 invocation 的 Turn ID）。新增事件变体需要补齐 `crates/acp/src/mapping.rs` 等处的穷举匹配（先映射为无更新，Task 8 再接入）。日志按 spec §5.4。

- [ ] **Step 4：确认通过**

Run: `cargo test -p kernel && cargo test -p acp`
Expected: 全部通过。

---

### Task 6: `todo_write`

**Files:**
- Create: `crates/kernel/src/runtime/plan/todo.rs`
- Modify: `crates/protocol/src/plan.rs`、`crates/protocol/src/tool.rs`、`crates/kernel/src/runtime/session/assembly.rs`
- Test: `crates/kernel/tests/protocol_alignment/plan_mode.rs`

**Interfaces:**
- Consumes：Task 4 的 `register_kernel` 与 `PlanModeGate`。
- Produces（protocol）：
  - `#[serde(rename_all = "snake_case")] pub enum TodoStatus { Pending, InProgress, Completed }`
  - `#[serde(rename_all = "camelCase")] pub struct TodoItem { pub content: String, pub status: TodoStatus, pub active_form: String }`
  - `ToolResultDetails::TodoList { todos: Vec<TodoItem> }`
- Produces（kernel）：`TodoWriteTool`，参数 `{ todos: TodoItem[] }`；`validate` 拒绝非法 `status`、去掉首尾空白后为空的 `content` 或 `activeForm`；结果为固定确认语加 `TodoList` 详情；`tracing::debug!` 只记会话 ID 与条目数

- [ ] **Step 1：写失败测试**

| 测试名 | 断言 |
|---|---|
| `todo_write_returns_the_validated_list` | 结果详情 `TodoList` 等于输入；空数组得到空清单 |
| `todo_write_rejects_invalid_items` | 非法 `status`、空 `content`、空 `activeForm` 各返回错误，没有 `TodoList` 详情 |
| `todo_write_works_in_plan_mode` | Plan 下调用成功 |
| `todo_logs_omit_items` | 捕获 `tracing`（级别 `debug`）：日志含条目数量，不含任何 `content` 或 `activeForm` 文本（P13） |
| `tool_definitions_do_not_change_with_mode` | 补上：两种模式的工具定义都包含 `exit_plan_mode` 与 `todo_write` |
| `reserved_names_resolve_to_kernel_tools` | 静态扩展提供同名工具时，模型调用 `todo_write` 得到 `TodoList` 详情、调用 `exit_plan_mode` 发起审批，即两者都是 Kernel 的实现 |

- [ ] **Step 2：确认失败**

Run: `cargo test -p kernel --test protocol_alignment plan_mode`
Expected: 编译失败。

- [ ] **Step 3：实现**（工具描述按 spec §6.4，保持简短）

- [ ] **Step 4：确认通过**

Run: `cargo test -p kernel`
Expected: 全部通过。

---

### Task 7: ACP 模式配置项与计划更新

**Files:**
- Modify: `crates/acp/Cargo.toml`、`crates/acp/src/config.rs`、`crates/acp/src/server.rs`、`crates/acp/src/mapping.rs`、`crates/acp/src/tool_call.rs`
- Test: `crates/acp/tests/plan_mode.rs`

**Interfaces:**
- Consumes：Task 2 的 `Kernel::set_session_mode`、`SessionSettings.mode`；Task 5、6 的结果详情。
- Produces：
  - `config.rs`：常量 `MODE = "mode"`，`ConfigChange::Mode(SessionMode)`；`ConfigOptionsOf` 输出第三个配置项
  - `tool_call.rs`：`ToolKindOf` 把 `protocol::EXIT_PLAN_MODE_TOOL` 映射为 `wire::ToolKind::SwitchMode`
  - `mapping.rs`：常量 `PLAN_MODE_PLAN_ID = "plan-mode"`、`TODOS_PLAN_ID = "todos"`。`MessageEnd(ToolResult)` 中：详情 `PlanApproval { approved: true, plan: Some(p) }` 时在 `tool_call_update` 之后追加 `plan_update`（`markdown`）；详情 `TodoList` 时只输出 `plan_update`（`items`，`priority = medium`，`in_progress` 用 `activeForm`）。`todo_write` 的 `ToolExecutionStart`、`ToolExecutionEnd`、助手消息中的工具调用块都不输出 `tool_call_update`
- 测试夹具沿用 `crates/acp/tests/v2_alignment.rs` 的 `alignment_kernel` 与脚本化模型。

- [ ] **Step 1：写失败测试**

| 测试名 | 断言 |
|---|---|
| `mode_config_option_switches_the_session` | `session/new` 的 `configOptions` 第三项为 `mode`（类别 `mode`，当前值 `default`）；`set_config_option` 设为 `plan` 后收到 `config_option_update`；`invalid` 返回 -32602 |
| `approved_plan_is_published_as_markdown` | 批准后依次收到工具的 `tool_call_update` 与 `plan_update { type: markdown, planId: "plan-mode", content }`；`content` 等于详情中的计划 |
| `kept_plan_is_not_published` | `keep_planning` 时没有 `plan_update` |
| `exit_plan_mode_is_a_switch_mode_call` | `exit_plan_mode` 的 `tool_call_update` 带 `kind = switch_mode`，实时与重放相同 |
| `rewritten_plan_is_published_live_and_on_replay` | 扩展 `tool_call` 钩子改写 `plan` 参数：实时与 `session/resume` 重放发布的都是改写后的计划 |
| `todo_write_updates_the_plan_without_a_tool_card` | 收到 `plan_update { type: items, planId: "todos" }`，条目映射正确；整个过程没有该调用的 `tool_call_update`；重放结果相同 |
| `blocked_todo_write_is_not_published` | 扩展 `tool_call` 钩子拦截 `todo_write`：没有 `plan_update`（P20） |
| `subagent_todos_stay_in_the_child` | 子代理调用 `todo_write`：`plan_update` 只出现在子会话的更新流中，父会话没有（P22） |
| `replay_publishes_plans_in_order` | 两次 `todo_write` 与一次批准后 `session/resume`：按顺序重发；没有任何 `plan_removed` |

- [ ] **Step 2：确认失败**

Run: `cargo test -p acp --test plan_mode`
Expected: 失败。

- [ ] **Step 3：实现**

`crates/acp/Cargo.toml` 的 `agent-client-protocol` features 增加 `"unstable_plan_operations"`（保持数组内字母序）；`server.rs` 的 `SessionConfigUpdate::run` 增加 `ConfigChange::Mode` 分支，调用 `set_session_mode`。开启特性后补齐 `wire::SessionUpdate` 新变体导致的穷举匹配。

- [ ] **Step 4：确认通过**

Run: `cargo test -p acp`
Expected: 全部通过。

---

### Task 8: ACP 权限请求

**Files:**
- Create: `crates/acp/src/permission.rs`
- Modify: `crates/acp/src/lib.rs`、`crates/acp/src/projection.rs`、`crates/acp/src/server.rs`
- Test: `crates/acp/tests/plan_mode.rs`

**Interfaces:**
- Consumes：Task 5 的事件、`Kernel::respond_plan_approval`、`Kernel::plan_approval`。
- Produces：
  - `pub(crate) enum PlanApprovalChange { Requested(Box<PlanApprovalRequest>), Resolved { request_id: String } }`，`EventGroup` 新字段 `plan_approval: Option<PlanApprovalChange>`
  - `pub(crate) struct PlanApprovalBridge`（结构照 `ElicitationBridge`：`outstanding` 表），方法 `request(&self, connection, kernel, session_id, request)`、`resolved(&self, session_id, request_id)`、`reestablish(&self, connection, kernel, session_id) -> Result<(), KernelError>`
  - 回复映射：`Selected(approve)` → `Approve`；`Selected(keep_planning)` → `KeepPlanning`；`Cancelled`、未知 `optionId`、请求失败 → `Cancelled`

- [ ] **Step 1：写失败测试**

| 测试名 | 断言 |
|---|---|
| `plan_approval_uses_request_permission` | 客户端收到 `session/request_permission`：`title` 为固定值，`subject` 为 `toolCall`（`toolCallId` 正确、`kind = switch_mode`、内容含计划），`options` 为 `approve`、`keep_planning` 及其 kind；回复 `approve` 后运行继续 |
| `cancelled_or_unknown_option_keeps_planning` | 回复 `cancelled`、回复未知 `optionId`：模式为 Plan，运行结束 |
| `first_client_wins_and_others_are_withdrawn` | 两个连接打开同一会话：一个答复后，另一个收到 `$/cancel_request` |
| `pending_approval_is_reestablished_after_resume` | 等待中重新 `session/resume`：再次收到权限请求；期间状态为 `requires_action` |

- [ ] **Step 2：确认失败**

Run: `cargo test -p acp --test plan_mode`
Expected: 失败。

- [ ] **Step 3：实现**

`server.rs` 的会话 watcher 在发送完通知后处理 `group.plan_approval`（紧接 elicitation 的处理）；`session/resume` 时在 `reestablish_elicitations` 旁调用 `reestablish`。

- [ ] **Step 4：确认通过**

Run: `cargo test -p acp`
Expected: 全部通过。

---

### Task 9: WebUI

**Files:**
- Create: `web/src/features/session/PermissionDialog.tsx`、`web/src/features/composer/PlanPanel.tsx`
- Modify: `web/src/acp/protocol.ts`、`web/src/domain/model.ts`、`web/src/workspace/state.ts`、`web/src/workspace/sessionState.ts`、`web/src/workspace/sessionStaging.ts`、`web/src/workspace/updateDecoder.ts`、`web/src/workspace/controller.ts`、`web/src/shell/AppShell.tsx`、`web/src/features/composer/Composer.tsx`、`web/src/features/conversation/ToolCallCard.tsx`、`web/src/theme/workbench.css`

**Interfaces:**
- Consumes：Task 7 的配置项与计划更新、Task 8 的权限请求格式。
- Produces：
  - `AcpProtocol.methods.sessionRequestPermission = "session/request_permission"`
  - `type PermissionPrompt = Readonly<{ key: string; sessionId: SessionId; title: string; description?: string; plan?: string; options: readonly Readonly<{ optionId: string; name: string; kind: string }>[] }>`；`WorkspaceState.permissions: readonly PermissionPrompt[]`，动作 `permission/asked`、`permission/settled`
  - `type SessionPlans = Readonly<{ todos?: readonly Readonly<{ content: string; status: string }>[]; approvedPlan?: string }>`；`SessionWorkspaceState.plans`，动作 `plans/todos`、`plans/approved`；`transcript/cleared` 时随初始状态重置
  - `WorkspaceController.answerPermission(key: string, optionId: string | undefined): void`（`undefined` 回复 `cancelled`）

- [ ] **Step 1：实现解码与状态**

`updateDecoder.ts` 解码 `plan_update`：`planId = "todos"` 且 `type = items` → `plans/todos`；`planId = "plan-mode"` 且 `type = markdown` → `plans/approved`；其他忽略。`sessionState.ts` 与 `sessionStaging.ts` 两套实现都加 `plans`。

- [ ] **Step 2：实现权限请求**

`controller.ts` 注册处理函数（写法同 `elicit`）：校验 `sessionId`、`title`、`options`；`subject.toolCall.kind === "switch_mode"` 时取其文本内容为 `plan`；`signal` 中止时移除并回复 `{ outcome: "cancelled" }`。`AppShell` 在没有 elicitation 时显示第一个权限请求。

- [ ] **Step 3：实现组件**

- `PermissionDialog`：计划审批显示标题“执行这个计划？”、`MarkdownRenderer` 渲染的计划（可滚动）、按钮“继续规划”（`data-safe-focus`，`keep_planning`）与“批准并执行”（`approve`）；其他请求显示 Agent 的标题、说明与选项，`reject_*` 在前；Esc 回复 `cancelled`；焦点用 `useModalFocus`。
- `PlanPanel`：放在输入框上方，可折叠；待办按状态显示图标（其他状态按 `pending`）与进度 `已完成/总数`；已批准的计划默认折叠，展开用 `MarkdownRenderer`；都没有时不渲染。
- `Composer`：当前会话 `mode` 配置值为 `plan` 时显示 `Plan · 只读` 标记。
- `ToolCallCard`：`switch_mode` 用 `ClipboardList` 图标、标题“计划”；`switch_mode` 且 `in_progress` 时默认展开。

- [ ] **Step 4：验证**

Run: `cd web && npm run check && npm run build && npm run test:elicitation`
Expected: 全部通过。

---

### Task 10: 文档、端到端与全量检查

**Files:**
- Create: `crates/app/tests/web/plan_mode.e2e.mjs`
- Modify: `README.md`、`README_zh.md`

**Interfaces:**
- Consumes：完整后端与 WebUI；`crates/app/tests/web/cdp_page.mjs` 的 `Page`、`startChrome`、`openBlankPage`。

- [ ] **Step 1：更新 README**

中英文各加 plan 模式小节，内容按 spec §8。

- [ ] **Step 2：写端到端脚本**

沿用 `provider_login.e2e.mjs` 的结构：由仓库 `claw.toml` 生成隔离配置（只替换数据目录），用 `target/debug/clawcode serve` 与 `web/dist`；会话目录是带一个已提交 `notes.txt` 的临时 git 仓库；模型用 `--model`，默认取 `claw.toml` 的 `active_model`。模式：

| 模式 | 验收 | 步骤与断言 |
|---|---|---|
| `keep-planning` | P15 | 下拉框切到 `plan` → 发送“先调研，然后调用 exit_plan_mode 提交计划：在 notes.txt 末尾加一行 plan-mode-e2e。”→ 等待弹窗出现且正文非空（最长 3 分钟）→ 点“继续规划” → 弹窗关闭、模式仍为 `plan`、`notes.txt` 未变、输入框可用 |
| `approve` | P16、P24 | 同上，点“批准并执行” → 下拉框变为 `default` → 等待 `notes.txt` 含 `plan-mode-e2e` 且运行结束（最长 3 分钟）→ 计划面板出现“已批准的计划”；若执行中收到过 `todos` 计划更新，面板显示进度 `n/m` → 刷新页面（完整重放）后面板仍显示该计划 → 打开检查器的会话树，点“根节点”再点 `Navigate`，面板消失 |

断言只针对弹窗、下拉框、文件与面板，不检查模型原文。

- [ ] **Step 3：运行端到端**

Run: `cargo build -p app --bin clawcode && (cd web && npm run build) && node crates/app/tests/web/plan_mode.e2e.mjs keep-planning && node crates/app/tests/web/plan_mode.e2e.mjs approve`
Expected: 两个模式都输出 `plan mode e2e passed`。

- [ ] **Step 4：全量检查（P17）**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast && pre-commit run --all-files`
Expected: 全部通过。必须带 `--all-files`，避免 pre-commit 临时 stash 未暂存改动。

- [ ] **Step 5：向用户报告**

列出 P01–P24 的状态与对应测试；未运行或未通过的如实标注；询问是否作为一个提交放在 main 上。

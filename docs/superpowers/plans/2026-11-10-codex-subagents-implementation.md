# Codex 式多 agent 协作实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Codex v2 的 `spawn_agent` / `send_message` / `followup_task` / `wait_agent` / `list_agents` / `interrupt_agent` 取代 `subagent` 与 `background_subagent`，agent 间通信全部经 mailbox。

**Architecture:** `protocol` 新增 `AgentPath`、昵称与 `AgentMail`；kernel 在 `PendingQueue` 增加内存 `mail` 通道和 `Kernel::deliver_mail`，模型请求前排空、run 关闭时写回，`new_task` 唤醒或延长 run；六个工具挂在 `subagent/` 下，名单注册表是子会话 header 的内存缓存；完成通知在 `RunSettlement` 之后投入 owner 的 mailbox。随后删除 `Kernel::submit` 一族并迁移测试，最后改 WebUI 与文档。

**Tech Stack:** Rust 2024（tokio、serde、typed-builder、thiserror、tracing），ACP v2，React/TypeScript WebUI。

**Spec:** [Codex 式多 agent 协作设计](../specs/2026-11-10-codex-subagents-design.md)

## 对照 spec 的要点

以下决定都已写入 spec，这里集中列出便于执行时对照：

- `Kernel::deliver_mail(MailDelivery)` 为 `pub` 原语；唤醒任务由接收方自己的 `SessionBackgroundWork` 跟踪；payload 统一截断到 `TOOL_TEXT_BYTES`（「投递」）。
- 空闲写入用新生成的 `TurnId`（「投递」第 4 步）。
- run 关闭时剩下的 `new_task`：被取消则写入记录、不唤醒；其他结局不写入，直接以它唤醒新 run（「`mail` 通道」）。
- 唤醒任务的 run 开始前失败时，`PendingInit` 改为 `Errored`；spawn 时投递失败则移除注册表条目（「投递」「spawn」）。
- 名单注册表是 `Mutex<Option<BTreeMap<…>>>` 加同步闭包 `with_children`；子 agent 读 owner 的注册表（「数据模型」）。
- fork 时压缩与分支摘要转为 `User` 文本（「spawn」第 5 步）。
- `create_owned_session(OwnedSessionRequest)`；`max_agents: NonZeroUsize`；删除 `TraceOrigin::Inherit`，开始行为 `as subagent linked to trace {parent}`；`QueueWaiter` 改为 `Option<Arc<InsertionNotice>>`。
- agent 工具名常量放在 `protocol`，与 `EXEC_COMMAND_TOOL`、`EXIT_PLAN_MODE_TOOL` 相同。
- `Kernel::submit` 被 exec、plan_mode、insertion 等约 50 处测试调用；删除后这些测试改用 `Kernel::run(RunRequest { input: RunInput::Blocks(..) }, sink)`，从 `RunResult.messages` 读答案（任务 7）。

## Global Constraints

- 不执行 `git commit`、不创建 worktree。每个任务结束时汇报改动与验证结果；全部完成后经用户明确允许，在 main 上做**一次**提交，包含 spec、本计划与代码；提交前跑 pre-commit（`fmt`、`clippy`、`rig-boundary`）并套用 `.gitmessage`。
- 新增和修改的函数必须有英文函数级注释；非平凡逻辑写英文注释。`docs/` 文档中文。
- 超过 3 个字段的结构体用 `typed-builder` 并通过 builder 构造；`Option` 字段加 `#[builder(default)]`。
- 克隆 `Arc` 字段写 `Arc::clone(&x)`。
- 日志用 `tracing::info!("...", ...)` 全限定路径和正文格式参数；不记录任务、消息、回答正文。
- 测试只放在 crate 的 `tests/` 目录或 `#[cfg(test)] mod tests`；生产代码不得含仅为测试存在的代码。前端不写单测；WebUI e2e 跑生产 `app` 后端与已配置 provider。
- 任务 3–6 的新测试一律以 `Kernel::run` 与 `Kernel::deliver_mail` 驱动，不使用现有基于 `Kernel::submit` 的 `invoke` 辅助函数，以免任务 7 返工。
- 不新增依赖。随机数用 `std::collections::hash_map::RandomState::new().hash_one(…)`。
- 共享上下文的函数挂在持有上下文的结构体上；转换用 `From` / `TryFrom`；不写无状态工具结构体。
- 不考虑任何历史数据兼容。
- clippy：`cargo clippy --workspace --tests --examples -- -Dwarnings`。
- 工具名：`spawn_agent`、`send_message`、`followup_task`、`wait_agent`、`list_agents`、`interrupt_agent`；`ROOT_ONLY_AGENT_TOOLS = ["spawn_agent", "followup_task", "interrupt_agent"]`。
- Extension `custom_type`：`agent_message`、`subagent_roster`。
- 数值：`max_agents` 默认 3；`wait_agent` 超时默认 30 000 ms、最小 10 000、最大 3 600 000；名单 note 最多 8 条、1024 字节；昵称表 101 个名字。
- 模型可见文案逐字使用 spec：工具描述、`Message sent to /root/x.`、`Task sent to /root/x.`、`Wait completed.`、`Wait interrupted by new input.`、`Wait timed out.`、`{task_name} already exists; use followup_task.`、`agent limit reached: {n} agents are running; wait for one to finish.`、``live agent path `/root/x` not found``、`root is not a spawned agent`、`an agent cannot message itself`、`message must not be empty`、``fork_turns must be `none` or `all` ``、`timeout_ms must be at most 3600000`、`Agent errored: {msg}`、`(no output)`、`[answer truncated; open the agent session for the full answer]`。

## Review Focus

1. **超长 payload**：`send_message` / `followup_task` / `spawn_agent` 的 `message` 有几百 KB 时，接收方上下文不能被撑爆：截断到 `TOOL_TEXT_BYTES` 并以 `[message truncated]` 结尾。测试归任务 3。
2. **不合规的名字**：`Reader`、`代码审查`、`root`、`a/b` 作为 `task_name` 时只返回 Codex 的校验文本，不创建子会话、不占用昵称。测试归任务 4。
3. **同一批里重名并行 spawn**：一条 assistant 消息里两个同名 `spawn_agent`，只能创建一个子会话，另一个得到 `reader already exists; use followup_task.`。测试归任务 4。
4. **协作过程中用户删除子会话**：之后对它 `send_message` 返回 ``live agent path `/root/reader` not found``，名单 note 更新，root 不会卡在 `wait_agent` 之外的任何地方。测试归任务 6。
5. **进程重启后的冷子会话**：重启后恢复 root，`list_agents` 报告子 agent 为 `{"completed": null}`；对未加载的子 agent `send_message` 会先恢复它、写入邮件且不启动 run。测试归任务 6。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/protocol/src/agent.rs`（新） | `AgentPath`、`AgentNickname`、`AgentMail`、`MailKind`、工具名与 custom type 常量 |
| `crates/protocol/assets/agent_names.txt`（新） | Codex 昵称表 |
| `crates/protocol/src/session.rs` | `SessionOwnership` 结构体、`RunInput::Mail` |
| `crates/kernel/src/runtime/subagent/mail.rs`（新） | `MailDelivery`、`MailPlacement`、`Kernel::deliver_mail`、写入与唤醒 |
| `crates/kernel/src/runtime/subagent/roster.rs`（新） | `ChildEntry`、`ChildStatus`、注册表、完成通知、`SubagentRosterNote` |
| `crates/kernel/src/runtime/subagent/fork.rs`（新） | `ForkedHistory` |
| `crates/kernel/src/runtime/subagent/path.rs`（新） | 调用者路径与目标解析 |
| `crates/kernel/src/runtime/subagent/tools.rs`（新） | 六个工具 |
| `crates/kernel/src/runtime/subagent/background.rs` | 只剩 `SessionBackgroundWork` |
| `crates/kernel/src/runtime/run/{queue.rs,lifecycle.rs,turn.rs,input.rs,settlement.rs}` | `mail` 通道、邮箱信号、排空 / 延长 / 写回、`Mail` 输入、完成通知触发 |
| `crates/kernel/tests/subagent/{mail,spawn,roster,collaboration}.rs`（新） | 集成测试 |
| `web/src/features/conversation/AgentMailCard.tsx`（新） | agent 消息卡片 |

---

### Task 1：protocol 的 agent 原语

**Files:**
- Create: `crates/protocol/src/agent.rs`、`crates/protocol/assets/agent_names.txt`（复制 `/home/isbest/Documents/WorkSpace/codex/codex-rs/core/assets/agent/agent_names.txt`）
- Modify: `crates/protocol/src/lib.rs`（导出）、`THIRD_PARTY_NOTICES.md`（`openai/codex`，Apache-2.0，说明昵称表来源）
- Test: `crates/protocol/tests/agent.rs`

**Interfaces:**
- Produces:
  - `pub struct AgentPath(String)`：serde `try_from = "String", into = "String"`；`pub const ROOT: &str = "/root"`；`root() -> Self`；`join(&self, name: &str) -> Result<Self, String>`；`name(&self) -> &str`；`is_root(&self) -> bool`；`resolve(&self, reference: &str) -> Result<Self, String>`；`validate_name(name: &str) -> Result<(), String>`；`TryFrom<&str>`、`TryFrom<String>`、`Display`、`AsRef<str>`。
  - `pub struct AgentNickname(String)`：`allocate(used: &HashSet<String>, index: usize) -> Self`；`into_inner(self) -> String`。
  - `pub enum MailKind { Message, NewTask, Result }`（serde snake_case）：`as_str(self) -> &'static str`、`triggers_turn(self) -> bool`。
  - `pub struct AgentMail { pub author: AgentPath, pub recipient: AgentPath, pub kind: MailKind, pub payload: String }`（typed-builder、serde）：`model_text(&self) -> String`；`impl From<AgentMail> for ExtensionMessage`。
  - 常量：`SPAWN_AGENT_TOOL`、`SEND_MESSAGE_TOOL`、`FOLLOWUP_TASK_TOOL`、`WAIT_AGENT_TOOL`、`LIST_AGENTS_TOOL`、`INTERRUPT_AGENT_TOOL`、`AGENT_MESSAGE_TYPE = "agent_message"`、`SUBAGENT_ROSTER_TYPE = "subagent_roster"`。

- [ ] **Step 1：写失败测试**（`crates/protocol/tests/agent.rs`）：
  - `agent_names_follow_codex_rules`：`validate_name("worker_1")` 成功；`""` → `agent_name must not be empty`；`"root"` → ``agent_name `root` is reserved``；`"."`、`".."` → 对应 reserved 文本；`"a/b"` → ``agent_name must not contain `/` ``；`"Reader"`、`"代码审查"` → `agent_name must use only lowercase letters, digits, and underscores`。
  - `paths_resolve_against_root`：`root().resolve("x") == "/root/x"`；`resolve("/root")` 为 root；`resolve("/root/y")` 成功；`resolve("root")` 报 reserved；`resolve("/root/a/b")` → ``agent path `/root/a/b` is deeper than one level``；`resolve("/other")` → ``absolute agent paths must start with `/root` ``；`"/root/x"` 的 `name() == "x"`，root 的 `name() == "root"`。
  - `nickname_allocation_is_deterministic_and_rolls_over`：从空集合起每次以 `index = 0` 分配并插入：第 1 个为 `Euclid`，第 102 个为 `Euclid the 2nd`，第 1011 个为 `Euclid the 11th`，第 1112 个为 `Euclid the 12th`，第 2021 个为 `Euclid the 21st`；同一 `used` 与 `index` 两次调用结果相同。
  - `mail_renders_codex_text_and_extension`：`/root` → `/root/x`、`NewTask`、`"Do it."` 的 `model_text()` 等于 `"Message Type: new_task\nTask name: /root/x\nSender: /root\nPayload:\nDo it."`；转换出的 `ExtensionMessage` 的 `extension_id == "clawcode"`、`custom_type == "agent_message"`、`display`、`include_in_context` 为真、`details == json!({"kind":"new_task","author":"/root","recipient":"/root/x"})`；只有 `NewTask.triggers_turn()` 为真。
- [ ] **Step 2：运行确认失败**：`cargo test -p protocol --test agent`，预期编译失败。
- [ ] **Step 3：实现 `agent.rs`。** 名字表用 `include_str!("../assets/agent_names.txt")` 读入。`allocate` 的算法：轮次 `r` 从 0 递增，第 `r` 轮的候选为名字表中「格式化后不在 `used` 中」的名字（`r == 0` 时为原名，否则 `"{name} the {r+1}{suffix}"`，后缀按 Codex `format_agent_nickname`：`% 100` 为 11–13 时为 `th`，否则 `% 10` 为 1/2/3 时为 `st`/`nd`/`rd`，其余 `th`）；取第一个非空轮次的 `candidates[index % len]`。
- [ ] **Step 4：运行确认通过**：`cargo test -p protocol --test agent` PASS。

### Task 2：ownership 结构体并移除旧工具

**Files:**
- Modify: `crates/protocol/src/session.rs:154-187`（`SessionOwnership` 改结构体，删除 `owner()`）
- Modify: `crates/kernel/src/runtime/session/{ownership.rs,assembly.rs,close.rs,open.rs}`、`crates/kernel/src/runtime/mcp/host.rs`、`crates/kernel/src/runtime.rs`（删除 `subagent_reply_as` 字段与 builder）、`crates/kernel/src/runtime/run/trace.rs` 与 `run.rs`（删除 `TraceOrigin::Inherit`，改开始行文案）
- Delete: `crates/kernel/src/runtime/subagent/foreground.rs`；`background.rs` 中除 `SessionBackgroundWork`（保留 `shutdown`、`tasks`、`join`）外的全部内容（`SubagentName`、`Reporter`、`BackgroundAction`、`BackgroundSubagentTool`、`reported`、`spawning`、`spawn`）；`subagent/mod.rs` 的 `register` 与 `SubagentError` 及 `assembly.rs:204` 的调用。这些在本任务后无人使用，留着会触发 `-Dwarnings`；`spawn` 由任务 3、`spawning` 与 `SubagentError`（原 `subagent/mod.rs:9-33` 的定义）由任务 4 重新加入
- Modify: `crates/config/src/subagent.rs`（`reply_as` → `max_agents: NonZeroUsize`，默认 3）、`crates/config/src/config.rs:504-520` 的测试、`crates/app/src/lib.rs:201`（删除 `subagent_reply_as` 接线）
- Delete tests: `crates/kernel/tests/subagent/{foreground.rs,background.rs}`；`delegation.rs` 的 `foreground_answer_respects_tool_budget`、`cold_runtime_status_does_not_parse_transcript_body`；`run_traces.rs`（4 个用例全是前台 / 后台用例，整个文件删除；同时从 `main.rs` 去掉 `#[path = "../support/spans.rs"] mod spans;`，否则测试目标出现未使用代码，任务 4 的 trace 用例再加回）
- Modify tests: `crates/kernel/tests/subagent/{fixture.rs,ownership.rs,delegation.rs}`、`crates/kernel/tests/mcp_host.rs`、`crates/store/tests/jsonl.rs`、`crates/acp/tests/extension.rs`

**Interfaces:**
- Consumes: 任务 1 的工具名常量。
- Produces:
  - `pub struct SessionOwnership { pub session_id: SessionId, pub name: String, pub nickname: String }`（serde camelCase，无 `kind`）。
  - `pub(in crate::runtime) const ROOT_ONLY_AGENT_TOOLS: [&str; 3] = [SPAWN_AGENT_TOOL, FOLLOWUP_TASK_TOOL, INTERRUPT_AGENT_TOOL]`（`session/ownership.rs`，取代 `SUBAGENT_TOOL_NAMES`，`OwnedSessionSpawn` 继续经 `without_tools` 使用）。
  - `config::SubagentConfig { pub max_agents: NonZeroUsize }`。

- [ ] **Step 1：改测试**：`crates/store/tests/jsonl.rs` 的 ownership 用例改名为 `owned_header_round_trips_struct_ownership`，断言 header JSON 的 `ownership == json!({"sessionId":"<owner>","name":"x","nickname":"Euclid"})` 且不含 `kind`；`config` 用例改为 `subagent_max_agents_defaults_to_three_and_rejects_zero`：默认值为 3，`[subagent]\nmax_agents = 5` 读到 5，`max_agents = 0` 解析失败；`acp/tests/extension.rs` 的 `session/list` 用例断言 `_meta.clawcode.ownership == {"sessionId","name","nickname"}`。
- [ ] **Step 2：运行确认失败**：`cargo test -p store --test jsonl` 与 `cargo test -p config`，预期编译失败。
- [ ] **Step 3：改类型并删除旧工具**，用 `cargo check --workspace --all-targets` 找出所有 `SessionOwnership::{Tool,Background}` 与 `owner()` 调用改为 `.session_id`。
- [ ] **Step 4：运行确认通过**：`cargo test -p store --test jsonl`、`cargo test -p config`、`cargo test -p kernel`（全部测试目标，`protocol_alignment` 与 `subagent` 共用 fixture）、`cargo test -p acp --test extension` PASS；`cargo clippy --workspace --tests --examples -- -Dwarnings` 无警告。

### Task 3：mailbox 投递核心

**Files:**
- Modify: `crates/protocol/src/session.rs`（`RunInput::Mail(AgentMail)`）
- Modify: `crates/kernel/src/runtime/run/input.rs`（`PromptInputExpansion::Mail`）、`run.rs:203-210`（`Mail` 分支）、`run/queue/operations.rs:73`（入队拒绝 `RunInput::Mail`；用户入队递增邮箱信号的 `input`）
- Modify: `crates/kernel/src/runtime/run/queue.rs`（`PendingQueue.mail`；`claim_next_or_close` 的 `new_task` 延长）、`run/lifecycle.rs`（`SessionExecution.mailbox`）、`run/turn.rs:460`（排空）与 `:1398`（写回）
- Create: `crates/kernel/src/runtime/subagent/mail.rs`；Modify: `subagent/mod.rs`、`crates/kernel/src/lib.rs`（导出 `MailDelivery`、`MailPlacement`）
- Test: `crates/kernel/tests/subagent/mail.rs`（在 `main.rs` 注册）、`crates/acp/src/mapping.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 任务 1 的 `AgentMail`、`MailKind`。
- Produces:
  ```rust
  impl SessionBackgroundWork {
      /// Tracks one background task, reaping settled ones; refused with `SessionClosing` after shutdown.
      pub(in crate::runtime) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> Result<(), KernelError>;
  }
  #[derive(typed_builder::TypedBuilder)]
  pub struct MailDelivery {
      pub recipient: SessionId,
      pub mail: protocol::AgentMail,
      /// Trace of the sending Run; a woken recipient Run links to it.
      #[builder(default)]
      pub link_trace: Option<TraceId>,
      /// Sender Turn token that aborts a wait for an idle recipient's gate.
      #[builder(default)]
      pub cancellation: Option<CancellationToken>,
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum MailPlacement { Queued, Written, Woken }
  impl Kernel { pub async fn deliver_mail(&self, delivery: MailDelivery) -> Result<MailPlacement, KernelError>; }

  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
  pub(in crate::runtime) struct MailboxActivity { pub(in crate::runtime) mail: u64, pub(in crate::runtime) input: u64 }
  impl SessionExecution { pub(in crate::runtime) fn mailbox(&self) -> tokio::sync::watch::Receiver<MailboxActivity>; }
  impl PendingQueue {
      pub(in crate::runtime) fn push_mail(&mut self, mail: AgentMail);
      pub(in crate::runtime) fn take_mail(&mut self) -> Vec<AgentMail>;
  }
  impl Session {
      /// Persists mail as `agent_message` Extension messages in one Turn and emits their lifecycle.
      pub(in crate::runtime) async fn write_mail(&self, kernel: &Kernel, emitter: &EventEmitter, turn_id: &TurnId, mail: Vec<AgentMail>) -> Result<(), KernelError>;
  }
  ```
  另有 `SessionExecution::pending_mail(&self) -> Result<bool, KernelError>`，供任务 6 的 `wait_agent` 判断「已有邮件」。

- [ ] **Step 1：写失败测试**（`mail.rs`，用 `TestKernel`，在 fixture 中给 `RoutedModel` 加 `request_count()`）：
  - `mail_to_idle_session_is_written_without_a_run`：`Message` → `Written`；`session_transcript` 末条为 `custom_type == "agent_message"` 的 Extension，文本以 `"Message Type: message\n"` 开头；`request_count() == 0`。
  - `new_task_to_idle_session_wakes_a_run`：`NewTask` → `Woken`；等到一次模型请求，其末条消息文本以 `"Message Type: new_task"` 开头；该 run 的首条持久化消息是 Extension，不是 `User`。
  - `queue_only_mail_does_not_extend_a_finishing_run`：首个请求用 `Route::Wait` 挂起，期间投递 `Message` → `Queued`；放行后模型 `EndTurn`；run 结束后邮件在记录中、位于 assistant 回答之后，`request_count() == 1`。
  - `mail_enters_the_next_request`：首个请求返回一次 `todo_write` 调用并挂起，期间投递 `Message`；第二个请求的末条消息是这封邮件。
  - `new_task_extends_a_finishing_run`：挂起期间投递 `NewTask` → `Queued`；放行后模型 `EndTurn`，仍有第二个请求且其末条为该邮件。
  - `mail_survives_cancel_and_clear`：挂起期间投递 `Message` 与 `NewTask`，再 `clear_pending_messages` 与 `cancel_session`；两封邮件都在记录中，之后没有新的模型请求，`pending_messages` 为空。
  - `leftover_new_task_wakes_after_turn_limit`：`KernelPolicy.max_turns = 1`，唯一的请求挂起期间投递 `NewTask`；run 以 `TurnLimit` 结束后自动开始新 run，其首条输入是该邮件，整个记录中这封邮件只出现一次。
  - `mail_input_never_dispatches_commands`：`NewTask` payload 为 `"!echo x"` 与 `"/compact"`；记录中没有 `BashExecution` 与 `SlashCommand`，首条为 Extension。
  - `oversized_mail_is_truncated`（Review Focus 1）：300 KB payload；写入的文本长度不超过 `TOOL_TEXT_BYTES` 加头部，以 `[message truncated]` 结尾。
  - `crates/acp/src/mapping.rs`：`agent_message_extension_projects_as_visible_message`，`display = true` 的 agent 邮件投影为可见消息并保留 `custom_type` 与 `details`。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test subagent mail::`，预期编译失败。
- [ ] **Step 3：实现。** 要点：
  - `deliver_mail` 按 spec「投递」四步走；未加载的接收方先 `resume_session(recipient, cwd)`，cwd 取 `header(recipient)`；空闲写入用 `id_generator.next(IdKind::Turn)` 生成的 `TurnId`。
  - 第 3 步与 run 关闭写回在同一把 `execution.queue` 锁内判断 `open`，保证不会在关闭后推入。
  - 唤醒：`recipient.background.spawn(async move { run_with_source(RunRequest { session_id, input: RunInput::Mail(mail) }, kernel.session_sink(&id), RunInvocation::builder().input_source(InputSource::Extension).trace(link_trace.map_or(TraceOrigin::New, TraceOrigin::LinkFrom)).build()) })`，失败只记 warn。
  - `claim_next_or_close(allow_follow_up)`：没有用户队列项、`allow_follow_up` 为真且 `mail` 中有 `triggers_turn()` 的邮件时，不关闭队列，返回一个让 Turn 循环继续的结果（新增 `ClaimOutcome::{Item(ClaimedItem), Continue, Closed}` 替代 `Option<ClaimedItem>`）。
  - run 关闭：`close_queue()` 之后立即 `take_mail()`；不唤醒的邮件 `write_mail` 后 `sync_store`，然后才是 `withdraw_pending` 与 `RunSettlement`。剩下的 `new_task`：run 被取消（`execution` 为取消错误或 `AgentOutcome::Cancelled`）时一并写入；否则不写入，结算之后逐封提交唤醒任务。
  - 日志：投递放置方式、写回条数及其中 `new_task` 条数。
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test subagent mail::` 与 `cargo test -p acp mapping` PASS；`cargo test -p kernel` 全部通过。

### Task 4：注册表、`spawn_agent` 与 fork

**Files:**
- Create: `crates/kernel/src/runtime/subagent/{roster.rs,fork.rs,path.rs,tools.rs}`
- Modify: `crates/kernel/src/runtime/subagent/{mod.rs,background.rs}`（注册工具并重新加入 `SubagentError`；`SessionBackgroundWork` 重新加入 `spawning: tokio::sync::Mutex<()>`，新增 `children`）、`subagent/mail.rs`（唤醒任务的 run 开始前失败时，若接收方是子 agent 且状态仍为 `PendingInit`，改为 `Errored(error)`）、`crates/kernel/tests/subagent/main.rs`（重新加入 `mod spans;`）、`session/ownership.rs`（`OwnedSessionRequest`、thinking 覆盖、写入继承历史）、`session/assembly.rs:204`（重新调用 `subagent::register`）、`runtime.rs`（`subagent_max_agents: NonZeroUsize` 字段与 builder，默认 3）、`crates/app/src/lib.rs`（接线 `snapshot.subagent.max_agents`）、`crates/kernel/tests/subagent/fixture.rs`（`KernelPolicy.max_agents`）
- Test: `crates/kernel/tests/subagent/spawn.rs`

**Interfaces:**
- Consumes: 任务 1 的 `AgentPath`、`AgentNickname`、`AgentMail`；任务 2 的 `SessionOwnership`、`ROOT_ONLY_AGENT_TOOLS`；任务 3 的 `deliver_mail`。
- Produces:
  ```rust
  pub(in crate::runtime) enum ChildStatus { PendingInit, Completed(Option<String>), Errored(String), Interrupted }
  pub(in crate::runtime) struct ChildEntry { pub(in crate::runtime) session_id: SessionId, pub(in crate::runtime) nickname: String, pub(in crate::runtime) status: ChildStatus }
  impl SessionBackgroundWork {
      /// Loads the owner's children from headers on first use, then runs `f` on the cached roster.
      pub(in crate::runtime) fn with_children<R>(&self, kernel: &Kernel, owner: &Session, f: impl FnOnce(&mut BTreeMap<String, ChildEntry>) -> R) -> Result<R, KernelError>;
  }
  impl Session {
      /// `/root` for an owner, `/root/{name}` for a child.
      pub(in crate::runtime) fn agent_path(&self) -> AgentPath;
      /// Resolves a tool target to its path and live-or-persisted child Session.
      pub(in crate::runtime) async fn resolve_agent(&self, kernel: &Kernel, target: &str) -> Result<Result<(AgentPath, Option<SessionId>), String>, KernelError>;
  }
  pub(in crate::runtime) struct ForkedHistory(pub(in crate::runtime) Vec<AgentMessage>);
  impl From<&[AgentMessage]> for ForkedHistory { /* spec「spawn」第 5 步的过滤规则 */ }
  #[derive(typed_builder::TypedBuilder)]
  pub struct OwnedSessionRequest {
      pub ownership: SessionOwnership,
      pub title: SessionTitle,
      #[builder(default)] pub model: Option<Arc<dyn Model>>,
      #[builder(default)] pub thinking_level: Option<ThinkingLevel>,
      #[builder(default)] pub history: Vec<AgentMessage>,
  }
  impl Kernel { pub async fn create_owned_session(&self, request: OwnedSessionRequest) -> Result<SessionId, KernelError>; }
  ```
  `resolve_agent` 的内层 `Err(String)` 是可纠正的用法错误文本；`Ok((path, None))` 只在 path 为 `/root` 时出现。调用者是子 agent 时，`resolve_agent` 与 `list_agents` 经 `kernel.session(&ownership.session_id)` 取 owner，在 owner 的 `background.with_children` 上解析；子会话自己的注册表始终为空。`ForkedHistory` 的过滤是纯函数，在 `fork.rs` 的 `#[cfg(test)] mod tests` 中直接测试。

- [ ] **Step 1：写失败测试**（`spawn.rs`）：
  - `spawn_returns_immediately_and_starts_child`：root 调用 `spawn_agent {"task_name":"reader","message":"Summarize."}`，子会话请求用 `Route::Wait` 挂起；root 的 run 在子会话挂起期间结束；工具结果文本为 `{"task_name":"/root/reader"}`、details 为 `Subagent { session_id }`；子会话 header ownership 为 `{session_id: root, name: "reader", nickname}`，nickname 属于昵称表，标题等于 nickname；子会话首条消息为 `new_task` 邮件，文本含 `"Task name: /root/reader\nSender: /root"`。
  - `fork_all_copies_filtered_history_without_usage`：root 先有一轮 `"hello"` / `"hi"`（assistant usage 非零），再发 spawn；子会话 `new_task` 之前依次是 `User "hello"`、`Assistant "hi"`（usage 全零、`model_message` 为 `None`）、触发 spawn 的 `User`；没有 `ToolResult`、没有仅含工具调用的 assistant、没有 Extension；`"fork_turns":"none"` 时 `new_task` 是第一条；`"fork_turns":"3"` 等同 `all`；`"fork_turns":"some"` 返回 ``fork_turns must be `none` or `all` ``。
  - `spawn_applies_model_and_reasoning_overrides`：`reasoning_effort: "high"` → 子会话 thinking 为 `High`、`"none"` → `Off`；`model` 为已注册引用时子会话使用它；未知模型与非法 `reasoning_effort` 返回纯文本且不创建子会话。
  - `invalid_and_duplicate_names_are_plain_replies`（Review Focus 2）：`Reader`、`代码审查`、`root`、`a/b` 各返回任务 1 的对应文本，`owned_children(root)` 为空；第二次 `reader` 返回 `reader already exists; use followup_task.`；`message` 为 `"  "` 返回 `message must not be empty`。
  - `concurrent_same_name_spawns_create_one_child`（Review Focus 3）：一条 assistant 消息含两个 `reader` spawn，只有一个子会话，另一个结果为 `reader already exists; use followup_task.`。
  - `spawn_respects_max_agents`：`max_agents = 1`，第一个子 agent 挂起期间再 spawn 返回 `agent limit reached: 1 agents are running; wait for one to finish.`；同一批内连续两个 spawn 也只成功一个。
  - `closing_an_unstarted_child_releases_its_slot`：`max_agents = 1`，spawn 后立刻 `close_session(child)`；之后 spawn 另一个名字成功。无论关闭发生在首个 run 开始前还是开始后，名额都必须释放。
  - `fork.rs` 单元测试 `forked_history_rewrites_summaries_and_strips_assistant_state`：输入含 `CompactionSummary`、`BranchSummary`、带 usage 与 `model_message` 的 assistant、仅含工具调用的 assistant、`ToolResult`、Extension；输出中两种摘要变为 `User` 文本且等于各自的 `model_text()`，assistant 只剩文本块、usage 为默认值、`model_message` 为 `None`，其余被丢弃。
  - `child_run_links_parent_trace`（取代 `run_traces.rs` 的后台用例）：子会话 RunStart 行含 `as subagent linked to trace {root trace}`。
  - `child_lacks_root_only_tools`：子会话模型请求的工具定义不含 `spawn_agent`、`followup_task`、`interrupt_agent`。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test subagent spawn::`，预期编译失败。
- [ ] **Step 3：实现。** 要点：参数在 `SpawnArgs: TryFrom<&ToolCall>` 中一次校验；spec「spawn」第 2–6 步全部在 `spawning` 锁内完成，只有最后的 `deliver_mail(NewTask)` 在锁外，它返回错误时移除注册表条目；运行中计数 = 注册表中 `session_runtime(id).running` 或状态为 `PendingInit` 的条目数；昵称下标用 `RandomState`；`fork_turns` 为全量时 `ForkedHistory::from(owner.transcript.history()?.as_slice())`，`create_owned_session` 在 `snapshot.apply` 之后、`register_session` 之前逐条写入；工具描述与参数 schema 逐字取 spec「工具描述」「参数校验」。日志：spawn 开始 / 完成 / 失败（owner、child、名称、fork 模式）与上限拒绝。
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test subagent` PASS，`cargo test -p app` 通过。

### Task 5：完成通知、`list_agents` 与名单 note

**Files:**
- Modify: `crates/kernel/src/runtime/run/settlement.rs`（`AgentSettled` 之后触发）、`subagent/roster.rs`（完成通知、`SubagentRosterNote`、`Session::refresh_subagent_roster`）、`subagent/tools.rs`（`list_agents`）、`session/transcript.rs`（`append_subagent_roster`，同 `append_command_sessions`）与重建上下文时还原 note 的分支、`run/turn.rs:460`（排空 mail 之后刷新名单）、`session/close.rs:119`（删除子会话时从 owner 注册表移除）
- Test: `crates/kernel/tests/subagent/roster.rs`

**Interfaces:**
- Consumes: 任务 3 的 `deliver_mail`；任务 4 的 `with_children`、`ChildStatus`、`agent_path`。
- Produces:
  - `impl Kernel { pub(in crate::runtime) fn notify_owner(&self, child: &Arc<Session>, answer: &SubmissionAnswer); }`：在 owner 的 `SessionBackgroundWork` 中提交任务，更新状态并投递 `Result` 邮件。
  - `pub(in crate::runtime) struct SubagentRosterNote { paths: Vec<AgentPath>, turn_id: TurnId }`：`impl ClawcodeEntry`（`CUSTOM_TYPE = SUBAGENT_ROSTER_TYPE`）、`message(&self, entry_id, at) -> Result<AgentMessage, KernelError>`、`stated(message: &AgentMessage) -> Option<Vec<AgentPath>>`。
  - `impl Session { pub(in crate::runtime) async fn refresh_subagent_roster(&self, kernel: &Kernel, turn_id: &TurnId) -> Result<(), KernelError>; }`

- [ ] **Step 1：写失败测试**（`roster.rs`）：
  - `child_answer_is_mailed_to_idle_owner_without_waking`：子 agent 回答 `"A whale."` 后，root 记录末条为 `"Message Type: result\nTask name: /root\nSender: /root/reader\nPayload:\nA whale."`，root 的模型请求数不变。
  - `child_answer_to_busy_owner_does_not_extend_its_run`：root 挂起时子 agent 完成；root 放行后 `EndTurn`，root 只有这一个请求，结果邮件在 assistant 回答之后。
  - `failed_and_interrupted_children`：子会话 `Route::Fail` → root 收到以 `Agent errored: ` 开头的 payload；对挂起的子会话 `cancel_session` → 没有结果邮件，`list_agents` 报 `"interrupted"`。
  - `user_chat_in_child_also_notifies_owner`：`Kernel::run` 直接在子会话里发用户输入，root 收到 `result` 邮件。
  - `long_answers_are_truncated_with_hint`：子 agent 回答 30 KB，payload 以 `[answer truncated; open the agent session for the full answer]` 结尾；空回答为 `(no output)`。
  - `list_agents_reports_codex_statuses`：root 运行中调用、子 agent 刚创建时结果为 `{"agents":[{"agent_name":"/root","agent_status":"running"},{"agent_name":"/root/reader","agent_status":"pending_init"}]}`；完成后为 `{"completed":"A whale."}`；`path_prefix: "/root/rea"` 只列 `reader`。
  - `roster_note_is_stated_only_on_change`：spawn 后 root 的下一次请求含一条隐藏消息，文本为 `"<subagents>\n  <agent name=\"/root/reader\" />\n</subagents>"`；再下一次请求不重复追加；删除子会话后追加 `"<subagents>\n</subagents>"`；fork root 后新会话首个请求追加空名单；超过 8 个名字或 1024 字节时截断；子会话的请求中没有该 note。
  - `registry_rebuilds_from_headers_after_restart`（Review Focus 5 的一半）：在同一个存储根上新建 `TestKernel`，恢复 root 后 `list_agents` 报 `{"completed":null}`。
  - `forked_child_is_standalone`：`fork_session` 一个子会话，新会话的 summary 没有 ownership，不出现在 root 的 `list_agents` 与名单 note 中；root 的主日志里没有任何子 agent 列表条目。
  - `closing_owner_with_pending_notification_does_not_hang`：子 agent 挂起时关闭 root，`tokio::time::timeout(10s)` 内完成，没有结果邮件。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test subagent roster::`。
- [ ] **Step 3：实现。** 要点：`settle` 拿到 `SubmissionAnswer` 后若 `session.ownership` 存在即调用 `notify_owner`；状态映射 `Completed → Completed(Some(截断文本))`、`Failed → Errored`、`Cancelled → Interrupted`（不投递）；名单渲染按 Codex `format_agent_paths`；root 状态为有 active run 时 `"running"`，否则 `{"completed":null}`。日志：完成通知已投递或跳过及原因、名单 note 更新的条目数。
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel --test subagent` PASS。

### Task 6：`send_message`、`followup_task`、`wait_agent`、`interrupt_agent`

**Files:**
- Modify: `crates/kernel/src/runtime/subagent/{tools.rs,mod.rs}`（注册剩余四个工具）、`run/queue/operations.rs`（用户 steer / follow-up 入队递增 `mailbox.input`）
- Test: `crates/kernel/tests/subagent/collaboration.rs`

**Interfaces:**
- Consumes: 任务 3 的 `deliver_mail`、`mailbox()`、`pending_mail()`；任务 4 的 `resolve_agent`、`with_children`；任务 5 的状态。

- [ ] **Step 1：写失败测试**（`collaboration.rs`，超时用例用 `#[tokio::test(start_paused = true)]`）：
  - `send_message_reaches_sibling_and_root`：子 agent `a` 发给 `"b"` 与 `"/root"`；空闲的 `b` 与空闲的 root 都写入 `message` 邮件且都没有新请求；结果文本为 `Message sent to /root/b.`、`Message sent to /root.`；`a` 调用 `list_agents` 得到 `/root`、`/root/a`、`/root/b` 三项（读的是 owner 的名单）。
  - `followup_task_wakes_idle_child_and_extends_busy_child`：对空闲子 agent → 新 run，首条为 `new_task`；对挂起的子 agent → 放行后它 `EndTurn` 仍再跑一个 Turn；结果文本为 `Task sent to /root/reader.`；`max_agents = 1` 且另一个子 agent 运行中时，唤醒空闲子 agent 返回上限文本。
  - `wait_agent_returns_on_mail_input_and_timeout`：root `wait_agent` 期间子 agent 完成 → `Wait completed.`；期间用户经 `Kernel::prompt` steer 或 follow-up 入队 → `Wait interrupted by new input.`；无活动 → `Wait timed out.`；调用前已有邮件则立即返回；`timeout_ms: 5` 的结果为 `"Wait timed out.\n\nRequested timeout of 5ms was clamped to the minimum of 10000ms."`；`timeout_ms: 4000000` → `timeout_ms must be at most 3600000`。
  - `interrupt_agent_cancels_child_and_rejects_root`：中断挂起的子 agent 返回 `{"previous_status":"running"}`，之后 `list_agents` 为 `"interrupted"`；目标 `/root` → `root is not a spawned agent`；`followup_task` 目标 `/root` 同样。
  - `self_targets_and_empty_messages_are_rejected`：root `send_message` 到 `/root`、子 agent 发给自己 → `an agent cannot message itself`；`message: "  "` → `message must not be empty`。
  - `children_see_only_mailbox_tools`：子会话请求中的 agent 工具恰为 `send_message`、`list_agents`、`wait_agent`。
  - `unknown_and_deleted_targets`（Review Focus 4）：`"ghost"` → ``live agent path `/root/ghost` not found``；删除 `reader` 后再发同样报错，下一次 root 请求的名单 note 为空名单。
  - `mail_to_unloaded_child_resumes_without_running`（Review Focus 5）：重启后恢复 root，`send_message` 到 `reader` → `reader` 已加载、末条为该邮件、没有模型请求。
  - `crossing_deliveries_do_not_deadlock`：root 在工具批次里 `send_message` 给正在结算的子 agent，同时该子 agent 的结果投向 root；`tokio::time::timeout(10s)` 内两边都完成。
- [ ] **Step 2：运行确认失败**：`cargo test -p kernel --test subagent collaboration::`。
- [ ] **Step 3：实现。** `wait_agent` 订阅 `mailbox()` 后先查 `pending_mail()`，再 `select` 计数变化、`tokio::time::sleep` 与 `context.cancellation`；`interrupt_agent` 先取状态再 `cancel_session`；四个工具共用任务 4 的目标解析与参数校验。日志：`wait_agent` 结束原因（mailbox、interrupted、timed_out）与 interrupt。
- [ ] **Step 4：运行确认通过**：`cargo test -p kernel` PASS；`cargo clippy --workspace --tests --examples -- -Dwarnings` 无警告。

### Task 7：删除 `Kernel::submit` 一族

**Files:**
- Modify: `crates/kernel/src/runtime/run/submit.rs`（删除 `SubmitRequest` 与 `submit`；剩下的 `install_event_sinks`、`session_sink` 移到新文件 `run/sinks.rs`，删除 `submit.rs`）
- Modify: `run/insertion.rs`（删除 `SubmissionReceipt`，保留 `SubmissionAnswer` / `AnswerOutcome`）、`run/queue.rs` 与 `run/queue/operations.rs`（`QueueWaiter` 改为 `Option<Arc<InsertionNotice>>`）、`run/lifecycle.rs`（删除 `submissions` 与 receipt 结算、`acquire_operation_for_prompt` 的 `cancellation_parent` 参数）、`run.rs`（删除 `RunInvocation.cancellation_parent`）、`crates/kernel/src/lib.rs`（导出）
- Delete tests: `crates/kernel/tests/subagent/submit.rs`；`delegation.rs::queued_submission_keeps_its_answer`；`tests/protocol_alignment/insertion.rs` 中直接测试 `submit` 的用例（`submit_maps_prompt_cancelled_to_cancelled_outcome` 及 `:951`、`:969` 所在用例）
- Modify tests: `crates/kernel/tests/exec/main.rs`、`crates/kernel/tests/protocol_alignment/plan_mode.rs`，以及 `crates/acp/src/projection.rs:1575-1600` 的测试（改为经 `deliver_mail(NewTask)` 唤醒一个会话，断言 RunStart 投影到该会话）

- [ ] **Step 1：迁移测试**：每个测试文件把本地 `request(..)` + `kernel.submit(..)` 改为 `kernel.run(RunRequest { session_id, input: RunInput::Blocks(vec![ContentBlock::Text { text }]) }, sink)`，答案文本取 `RunResult.messages` 中最后一条 assistant 的文本块；原先断言 `PromptCancelled` 等错误的用例直接断言 `run` 返回的 `KernelError`。
- [ ] **Step 2：删除生产代码**，以 `cargo check --workspace --all-targets` 收敛。
- [ ] **Step 3：运行确认通过**：`cargo test -p kernel`、`cargo test -p acp` PASS；`rg -n 'SubmitRequest|SubmissionReceipt|cancellation_parent|\.submit\(' crates` 无结果。

### Task 8：WebUI

**Files:**
- Modify: `web/src/domain/model.ts:14-17`、`web/src/workspace/controller.ts:865-873`（`SessionOwnership` 改为 `{ sessionId, name, nickname }`）
- Modify: `web/src/features/conversation/BackgroundSubagents.tsx`（筛选与昵称，悬停提示 `/root/<name>`，标题「子 agent」）、`ToolCallCard.tsx:98-103`（按 `runningSessionIds.has(childId)` 自动展开）、`SubagentPanel.tsx` 与 `Conversation.tsx`（紧凑视图隐藏首条 `new_task` 之前的行，显示「继承了 N 条父会话历史」；`customType === "agent_message"` 的 Extension 用 `AgentMailCard`）、`MessageView.tsx`（删除 `[subagent …]` 正则与回报样式）、`web/src/theme/workbench.css`
- Create: `web/src/features/conversation/AgentMailCard.tsx`

- [ ] **Step 1：实现。** `AgentMailCard` 从 `details` 读 `author`、`recipient`、`kind`（`message` → 消息、`new_task` → 任务、`result` → 结果），正文取文本中 `\nPayload:\n` 之后的部分，用 `MarkdownContent` 渲染。
- [ ] **Step 2：验证**：`cd web && npm run check` 与 `npm run build` 通过。

### Task 9：文档、端到端验收与提交

**Files:**
- Modify: `README.md`（`[subagent]` 配置改为 `max_agents`；第 257–267 行改写为六个工具与 mailbox 语义；第 419 行 plan mode 中 subagent 的说明；第 454–459 行 tracing：删除前台继承 trace，开始行改为 `as subagent linked to trace …`）、`README_zh.md` 对应段落
- Create: `crates/app/tests/web/subagent.e2e.mjs`（参照 `exec.e2e.mjs`）

- [ ] **Step 1：写 e2e 脚本**：用仓库 `claw.toml` 启动生产 `clawcode serve`；新建会话后让模型「spawn 两个子 agent `alpha`、`beta` 各回答一句话，然后调用 wait_agent 直到两个结果都到达」；断言：页面出现两张 `agent_message` 结果卡片，标签栏显示两个昵称，spawn 卡片在子 agent 运行时展开、结束后折叠，紧凑视图首行为继承历史提示；`page.errors` 为空。
- [ ] **Step 2：运行**：`cargo build -p app && (cd web && npm run build) && node crates/app/tests/web/subagent.e2e.mjs` 通过。
- [ ] **Step 3：全量验证**：`cargo test --workspace`、`cargo clippy --workspace --tests --examples -- -Dwarnings`、`pre-commit run --all-files` 全部通过。
- [ ] **Step 4：请求提交许可。** 获准后在 main 上做一次提交，包含 spec、本计划与全部代码，提交信息按 `.gitmessage`，例如 `feat(subagent): adopt Codex multi-agent tools with mailbox delivery`。

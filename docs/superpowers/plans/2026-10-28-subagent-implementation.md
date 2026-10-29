# Subagent 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 pi-durable 的所有权语义为 clawcode 增加前台 `subagent` 与后台 `background_subagent`，含共享 MCP、级联、ACP 投影与 WebUI 呈现。

**Architecture:** Kernel 提供原语（会话所有权、`create_owned_session`、`submit`、级联、`SessionEventSinks`），subagent 工具作为基于原语的独立模块在 `build_session` 中注册。子会话是普通 Session，header 记录 owner；子 run 的取消令牌派生自父 Turn 令牌，后台子会话不派生（background 边界）。

**Tech Stack:** Rust 2024（tokio、tokio-util、serde、thiserror、typed-builder、tracing），React/TypeScript WebUI。

**Spec:** `docs/superpowers/specs/2026-10-28-subagent-design.md`

## 对 spec 的实现细化

- ownership 不加到 `SessionCreateOptions`（107 处字面量构造，且加字段后触发规则 5 的 builder 要求）。改为与 `SessionForkOptions` 同构的 `OwnedSessionCreateOptions { create, ownership }` 加 `StoreFactory::create_owned`，默认实现返回 `StoreError::Unsupported`。
- 新增公开原语 `Kernel::create_owned_session`（对应 pi `createConversation({ ownership })`），`OwnedSessionSpawn` 是其内部实现，使原语可脱离工具测试。
- `KernelError::OwnerUnavailable(SessionId)` 用于 owner 会话不在运行时调用 `create_owned_session`。

## Global Constraints

- 代码注释英文，每个新增函数有函数级注释；`docs/` 下新文档中文。
- 超过 3 个字段的结构体用 `typed-builder` 构造；`Option` 字段 `#[builder(default)]`。
- 克隆 `Arc` 字段写 `Arc::clone(&x)`。
- tracing 宏全路径调用（`tracing::info!`），消息用正文格式参数；不记录任务正文、回报正文、prompt。
- 测试只放在 crate 的 `tests/` 或 `#[cfg(test)] mod tests`；生产代码不得有其他测试专用代码。
- 不新增依赖；若确需新增，在根 `[workspace.dependencies]` 声明后子 crate 继承。
- clippy 以 `--tests --examples -- -Dwarnings` 运行，工作区 lint 禁止 `unwrap`、索引切片等，生产代码不得使用。
- 不使用 worktree；未经用户明确允许不 commit。允许后，先跑 pre-commit（fmt + clippy），再按 `.gitmessage` 写提交信息。
- 工具名常量 `SUBAGENT_TOOL_NAMES = ["subagent", "background_subagent"]`，两个工具 `ToolExposure::Direct`。
- 模型可见文案逐字使用 spec 中的字符串（工具描述、`Started {name}.`、`Subagent was stopped before answering.` 等）。
- 前端代码不写单测；WebUI 验收用生产 `app` 后端与已配置 provider。
- 所有 shell 命令加 `rtk` 前缀。

## Review Focus

1. 模型给出空白 `task`（`"  "`）：期望直接回复 `subagent needs a task.`，不创建子会话。测试在 Task 7。
2. 同一个 assistant turn 里并行调用多次 `subagent`：期望各自得到独立子会话与答案。测试在 Task 7。
3. 前台工具等待期间用户删除子会话：期望父工具得到 `Subagent was stopped before answering.`，父 run 继续。测试在 Task 7。
4. 后台子会话回答非常长（30 KB）：回报作为用户消息进入父会话，不经过工具输出限额，期望截断到 `tools::TOOL_TEXT_BYTES` 并附提示。测试在 Task 9。
5. 后台名称非法（含空格或超过 64 字符）：期望固定回复而不是创建会话。测试在 Task 9。

---

## 阶段一：前台 subagent

### Task 1：所有权类型与持久化

**Files:**
- Modify: `crates/protocol/src/session.rs`（新增 `SessionOwnership`；`SessionSummary` 加字段）
- Modify: `crates/store/src/model.rs`（`OwnedSessionCreateOptions`、`SessionMetadata.ownership`、`StoreError::Unsupported`、`StoreFactory::create_owned`）
- Modify: `crates/store/src/factory.rs`（`V4Header.ownership`、`create_owned`、`list` 映射）
- Modify: `crates/kernel/src/runtime/session.rs:165-185`（`list_sessions` 映射 ownership）
- Test: `crates/store/tests/jsonl.rs`

**Interfaces:**
- Produces:
  ```rust
  // protocol
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
  pub enum SessionOwnership {
      Tool { session_id: SessionId, tool_call_id: ToolCallId },
      Background { session_id: SessionId, name: String },
  }
  impl SessionOwnership { pub fn owner(&self) -> &SessionId; }
  // SessionSummary 新字段: #[builder(default)] pub ownership: Option<SessionOwnership>
  // store
  pub struct OwnedSessionCreateOptions { pub create: SessionCreateOptions, pub ownership: SessionOwnership }
  // SessionMetadata 新字段: #[builder(default)] pub ownership: Option<SessionOwnership>
  // StoreError 新变体: #[error("store operation is unsupported: {0}")] Unsupported(&'static str)
  // StoreFactory 新方法（默认返回 Err(StoreError::Unsupported("create_owned"))）:
  fn create_owned(&self, options: OwnedSessionCreateOptions) -> Result<Box<dyn SessionStore>, StoreError>;
  ```

- [ ] **Step 1: 写失败测试**（`crates/store/tests/jsonl.rs`）

```rust
#[test]
fn owned_header_round_trips_through_list() {
    // create_owned with Tool { session_id: "owner", tool_call_id: "call-1" } under cwd
    let listed = factory.list(Some(&cwd)).expect("list");
    assert_eq!(listed[0].ownership, Some(ownership.clone()));
    let header: serde_json::Value = serde_json::from_str(first_line_of(&path)).expect("header");
    assert_eq!(header["ownership"], serde_json::json!({"kind":"tool","sessionId":"owner","toolCallId":"call-1"}));
}

#[test]
fn plain_header_has_no_ownership() {
    // factory.create(..) as today
    assert_eq!(listed[0].ownership, None);
    assert!(header.get("ownership").is_none());
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p store --test jsonl owned_header` → 编译失败（`create_owned` 未定义）。
- [ ] **Step 3: 实现** — `V4Header` 加 `#[serde(skip_serializing_if = "Option::is_none")] #[builder(default)] ownership: Option<SessionOwnership>`。把现有 `JsonlStoreFactory::create` 的主体移到私有方法 `fn create_with(&self, options: SessionCreateOptions, ownership: Option<SessionOwnership>)`，`create` 与 `create_owned` 都调用它。`list` 映射 `header.ownership`；`Kernel::list_sessions` 映射 `.ownership(metadata.ownership)`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p store --test jsonl` 全部 PASS；`rtk cargo check --workspace --tests` 通过。
- [ ] **Step 5: 检查点** — `rtk cargo fmt --all -- --check`，`rtk cargo clippy --tests --examples -- -Dwarnings`；经用户允许后提交 `feat(store): record session ownership in headers`。

### Task 2：`SettingChange` 收敛设置 entry 格式

**Files:**
- Create: `crates/kernel/src/runtime/settings.rs`
- Modify: `crates/kernel/src/runtime.rs`（`mod settings;`）
- Modify: `crates/kernel/src/runtime/session.rs:20-93`（`RestoredSessionSettings::load` 使用 `SettingChange::try_from`）
- Modify: `crates/kernel/src/runtime/extension/host.rs:581-700`（`set_model` / `set_thinking_level` 写入改用 `SettingChange`）

**Interfaces:**
- Produces:
  ```rust
  pub(in crate::runtime) enum SettingChange {
      Model { provider: String, model_id: String },
      ThinkingLevel(ThinkingLevel),
  }
  impl From<&SettingChange> for (EntryKind, serde_json::Value);
  impl TryFrom<&SessionEntry> for SettingChange { type Error = KernelError; }
  ```
  Payload 形状保持现状：`{"provider", "modelId"}` / `{"thinkingLevel"}`。非这两种 kind 返回 `KernelError::Protocol`。

- [ ] **Step 1: 写失败测试**（`settings.rs` 内 `#[cfg(test)] mod tests`）

```rust
#[test]
fn model_change_round_trips() {
    let change = SettingChange::Model { provider: "fixture".into(), model_id: "secondary".into() };
    let (kind, payload) = <(EntryKind, serde_json::Value)>::from(&change);
    assert_eq!(kind, EntryKind::ModelChange);
    assert_eq!(payload, serde_json::json!({"provider":"fixture","modelId":"secondary"}));
    // wrap into SessionEntry via SessionEntry::builder() and decode
    assert!(matches!(SettingChange::try_from(&entry), Ok(SettingChange::Model { .. })));
}

#[test]
fn thinking_change_round_trips() { /* ThinkingLevel::High ↔ {"thinkingLevel":"high"} */ }

#[test]
fn other_entry_kinds_are_rejected() { /* EntryKind::Custom → Err(KernelError::Protocol(_)) */ }
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --lib settings` → 编译失败。
- [ ] **Step 3: 实现转换并替换两个调用点**；host 写入仍走 `append_store_only_entry` + `sync_store`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --lib settings`；回归 `rtk cargo test -p kernel --test session_extension_state --test extensions_host` 全部 PASS。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `refactor(kernel): own setting entry payloads in SettingChange`。

### Task 3：`Kernel::submit`、事件 sink、派生取消令牌、结算 watch

**Files:**
- Create: `crates/kernel/src/runtime/submit.rs`
- Modify: `crates/kernel/src/sink.rs`（`SessionEventSinks` trait 与私有的丢弃 sink `DiscardEvents`）
- Modify: `crates/kernel/src/lib.rs`（导出 `SessionEventSinks`、`SubmitRequest`、`WhenBusy`、`SubmissionAnswer`、`AnswerOutcome`）
- Modify: `crates/kernel/src/runtime.rs`（`Kernel.event_sinks`、`install_event_sinks`、`KernelError::EventSinksInstalled`、`mod submit;`）
- Modify: `crates/kernel/src/runtime/lifecycle.rs`（`install_cancellation(parent)`、`SessionExecution.last_settlement`）
- Modify: `crates/kernel/src/runtime/run.rs:6`（`run_with_source` 增加 `cancellation_parent`）；调用点 `runtime.rs:277`、`extension/host.rs:262` 传 `None`；`user_bash.rs:66`、`compaction.rs:147`、`command/execution.rs:130` 调 `install_cancellation(None)`
- Modify: `crates/kernel/src/runtime/settlement.rs`（`settle` 发布 `RunSettlementSummary`）
- Create: `crates/kernel/tests/subagent/main.rs`、`crates/kernel/tests/subagent/fixture.rs`、`crates/kernel/tests/subagent/submit.rs`

**Interfaces:**
- Produces:
  ```rust
  pub trait SessionEventSinks: Send + Sync {
      fn sink(&self, session_id: &SessionId) -> Arc<dyn EventSink>;
  }
  impl Kernel {
      pub fn install_event_sinks(&self, sinks: Arc<dyn SessionEventSinks>) -> Result<(), KernelError>;
      pub async fn submit(&self, request: SubmitRequest) -> Result<SubmissionAnswer, KernelError>;
      /// Returns the installed sink for one Session, or a discarding sink.
      pub(in crate::runtime) fn session_sink(&self, session_id: &SessionId) -> Arc<dyn EventSink>;
  }
  pub enum WhenBusy { Steer, FollowUp }            // impl From<WhenBusy> for QueueKind
  #[derive(typed_builder::TypedBuilder)]
  pub struct SubmitRequest {
      session_id: SessionId,
      content: Vec<ContentBlock>,
      when_busy: WhenBusy,
      #[builder(default)] cancellation: Option<CancellationToken>,
      #[builder(default)] trace_id: Option<TraceId>,
  }
  pub struct SubmissionAnswer { pub message_id: Option<MessageId>, pub text: String, pub outcome: AnswerOutcome }
  pub enum AnswerOutcome { Completed, Cancelled, Failed(String) }
  pub(in crate::runtime) struct RunSettlementSummary { pub run_id: RunId, pub outcome: AgentOutcome, pub last_assistant: Option<MessageId> }
  // SessionExecution: last_settlement: tokio::sync::watch::Sender<Option<RunSettlementSummary>>
  // SessionExecution::install_cancellation(&self, parent: Option<&CancellationToken>) -> Result<CancellationToken, KernelError>
  ```
  `SubmissionAnswer` 由 `From<&RunResult>`（outcome 取最后一个 `TurnRecord.outcome`，无 turn 视为 `Completed`；text 为最后一条 assistant 消息全部 text 块的拼接）与 `TryFrom<(&RunSettlementSummary, &SessionTranscript)>` 构造。
- Fixture（`tests/subagent/fixture.rs`，供 Task 3–10 共用）：
  - `RoutedModel`：`Model` 实现，调用 `route: Box<dyn Fn(&ModelRequest) -> Route + Send + Sync>`。`Route::Events(Vec<ModelStreamEvent>)` 直接流出；`Route::Wait(Arc<tokio::sync::Notify>, Vec<ModelStreamEvent>)` 等到 `notify_one` 后流出（cancellation 先到则返回 `ModelError::Cancelled`）；`Route::Hang` 一直等到 cancellation 后返回 `ModelError::Cancelled`。记录每个请求的 `(最后一条消息文本, 工具名列表)`，提供 `last_texts(&self) -> Vec<String>`（按请求顺序）与 `tool_names_for(&self, last_text: &str) -> Vec<String>`。
  - `RecordingSinks`：实现 `SessionEventSinks`，记录 `(SessionId, AgentEvent)`。
  - `TestKernel { kernel: Kernel, root: TempDir, sinks: Arc<RecordingSinks> }`，`TestKernel::new(route)` 按 `crates/kernel/tests/turn.rs:415-450` 的方式构造（JsonlStoreFactory、FilesystemPromptFactory、StepClock、SequentialIds、`BuiltinToolFactory`），并安装 `RecordingSinks`。方法：`create(&self, id: &str) -> SessionId`、`async fn wait_until(&self, check: impl Fn() -> bool)`（10 ms 轮询，5 s 超时 panic）、`owned_children(&self, owner: &SessionId) -> Vec<SessionSummary>`（`list_sessions` 中 `ownership.owner() == owner` 的项）、`owner_user_texts(&self, owner: &SessionId) -> Vec<String>`（`session_transcript` 中用户消息的文本）。
  - 设置扩展参照 `crates/kernel/tests/session_extension_state.rs:95-160` 的 `StateCommand`：命令 `agent-state`，`{"action":"set"}` 把 model 设为 `fixture/secondary`、thinking 设为 `High`、active tools 设为 `["read"]`；模型目录同该文件，provider `fixture`，模型 `primary` / `secondary`。
  - `fn setting_payload(tree: &SessionTreeSnapshot, kind: &str) -> Option<serde_json::Value>`：返回最后一条该 kind 的 `payload`，用于断言子会话复制的设置。

- [ ] **Step 1: 写失败测试**（`tests/subagent/submit.rs`）

```rust
#[tokio::test]
async fn submit_runs_idle_session_and_returns_answer() {
    // route: any request → TextDelta("pong") + EndTurn
    let answer = kernel.submit(request(session, "ping", WhenBusy::FollowUp)).await.expect("submit");
    assert!(matches!(answer.outcome, AnswerOutcome::Completed));
    assert_eq!(answer.text, "pong");
    assert!(sinks.saw(&session, |payload| matches!(payload, AgentEventPayload::RunStart { .. })));
}

#[tokio::test]
async fn submit_treats_bang_and_slash_text_as_plain_message() {
    kernel.submit(request(session, "!echo pwned", WhenBusy::FollowUp)).await.expect("submit");
    let messages = kernel.session_transcript(&session).expect("transcript");
    assert!(messages.iter().all(|m| !matches!(m.content, MessageContent::BashExecution { .. })));
    assert_eq!(model.last_texts()[0], "!echo pwned");
}

#[tokio::test]
async fn submit_queues_when_busy_and_answers_after_run() {
    // first submit: route Hang until a Notify is released; second submit: FollowUp while busy
    assert_eq!(second.text, "second answer");
}

#[tokio::test]
async fn submit_rescues_message_left_queued_by_cancelled_run() {
    // run A hangs; submit B (FollowUp) is queued; kernel.cancel_session(&session)
    assert!(matches!(b.outcome, AnswerOutcome::Completed));
    assert_eq!(b.text, "answer for B");
}

#[tokio::test]
async fn submit_with_cancelled_parent_cancels_run() {
    let parent = CancellationToken::new();
    parent.cancel();
    let answer = kernel.submit(request_with_parent(session, "work", parent)).await.expect("submit");
    assert!(matches!(answer.outcome, AnswerOutcome::Cancelled));
}

#[tokio::test]
async fn installing_event_sinks_twice_fails() {
    assert!(matches!(kernel.install_event_sinks(sinks2), Err(KernelError::EventSinksInstalled)));
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent submit::` → 编译失败。
- [ ] **Step 3: 实现** — `install_cancellation` 用 `parent.map_or_else(CancellationToken::new, CancellationToken::child_token)`。`submit` 空闲分支调 `run_with_source(RunRequest { input: RunInput::Blocks(content) }, self.session_sink(id), InputSource::Extension, trace_id.unwrap_or(新 Trace), cancellation)`；忙碌分支 `queue_message(.., kind, RunInput::Blocks(..))` 后按 spec「Kernel::submit」四步循环（等空闲时用 `tokio::select!` 观察 `cancellation`）。日志：`started Submit for session {}`、`queued Submit {} for session {}`、`rescued stranded Submit {} for session {}`、`completed Submit for session {}`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent submit::` PASS；回归 `rtk cargo test -p kernel` 全部 PASS。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): add submit with derived cancellation and event sinks`。

### Task 4：`Kernel::create_owned_session`

**Files:**
- Create: `crates/kernel/src/runtime/ownership.rs`
- Modify: `crates/kernel/src/runtime.rs`（`Session` 加 `#[builder(default)] ownership: Option<SessionOwnership>`；`KernelError::OwnerUnavailable(SessionId)`；`mod ownership;`）
- Modify: `crates/kernel/src/runtime/session.rs`（`build_session(store, cwd, mcp: SessionMcpSource<'_>, ownership: Option<SessionOwnership>)`；本任务只有 `SessionMcpSource::Connect`；`create_session` / `resume_session` / fork 传 `Connect` 与 header ownership）
- Modify: `crates/kernel/src/runtime/tools/selection.rs`（`ToolSelectionUpdate::adopt`）
- Create: `crates/kernel/tests/subagent/ownership.rs`

**Interfaces:**
- Consumes: Task 1 `OwnedSessionCreateOptions`、`create_owned`；Task 2 `SettingChange`。
- Produces:
  ```rust
  impl Kernel {
      pub async fn create_owned_session(&self, ownership: SessionOwnership, title: SessionTitle) -> Result<SessionId, KernelError>;
  }
  pub(in crate::runtime) struct AgentSnapshot { model: Arc<dyn Model>, thinking_level: ThinkingLevel, tools: ToolSelection }
  impl TryFrom<&Session> for AgentSnapshot { type Error = KernelError; }
  impl AgentSnapshot {
      fn without_tools(self, names: &[&str]) -> Self;
      fn apply(&self, child: &Session, ids: &dyn IdGenerator) -> Result<(), KernelError>;
  }
  #[derive(typed_builder::TypedBuilder)]
  pub(in crate::runtime) struct OwnedSessionSpawn<'a> { kernel: &'a Kernel, owner: &'a Arc<Session>, ownership: SessionOwnership, title: SessionTitle }
  impl OwnedSessionSpawn<'_> { async fn create(self) -> Result<SessionId, KernelError>; }
  impl ToolSelectionUpdate<'_> { pub(in crate::runtime) fn adopt(&self, selection: ToolSelection) -> Result<(), KernelError>; }
  pub(in crate::runtime) const SUBAGENT_TOOL_NAMES: [&str; 2] = ["subagent", "background_subagent"];
  ```
  owner 取 `ownership.owner()`；owner 不在运行时返回 `OwnerUnavailable`。创建流程按 spec「OwnedSessionSpawn」1–6 步（本任务 MCP 用 `Connect`）。

- [ ] **Step 1: 写失败测试**（`tests/subagent/ownership.rs`）

```rust
#[tokio::test]
async fn owned_session_copies_owner_agent() {
    // owner: invoke_extension_command("agent-state", {"action":"set"})
    let child = kernel.create_owned_session(tool_ownership(&owner, "call-1"), title("subagent: x")).await.expect("child");
    let listed = kernel.list_sessions(Some(&cwd)).expect("list");
    let summary = listed.iter().find(|s| s.session_id == child).expect("child listed");
    assert_eq!(summary.ownership, Some(tool_ownership(&owner, "call-1")));
    assert_eq!(summary.name.as_deref(), Some("subagent: x"));
    let tree = kernel.session_tree(&child).expect("tree");
    assert_eq!(setting_payload(&tree, "model_change"), Some(serde_json::json!({"provider":"fixture","modelId":"secondary"})));
    assert_eq!(setting_payload(&tree, "thinking_level_change"), Some(serde_json::json!({"thinkingLevel":"high"})));
    let disabled = &setting_payload(&tree, "active_tools_change").expect("tools")["selection"]["disabled"];
    assert!(disabled.as_array().expect("array").contains(&"subagent".into()));
    assert!(disabled.as_array().expect("array").contains(&"background_subagent".into()));
}

#[tokio::test]
async fn creating_owned_session_requires_live_owner() {
    let result = kernel.create_owned_session(tool_ownership(&missing, "call-1"), title("x")).await;
    assert!(matches!(result, Err(KernelError::OwnerUnavailable(_))));
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent ownership::` → 编译失败。
- [ ] **Step 3: 实现** — `apply` 在构建后、注册前执行：`replace_model` / `replace_thinking_level` 加 `SettingChange` 写入，`ToolSelectionUpdate::adopt` 写入 `ActiveToolsChange`，不触发 `model_select`。日志：`started owned Session creation {} for owner {}`、`completed owned Session creation {} for owner {}`、失败 `tracing::error!`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent ownership::`；回归 `rtk cargo test -p kernel`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): create owned sessions with copied agent choices`。

### Task 5：子会话共享 owner 的 MCP

**Files:**
- Modify: `crates/kernel/src/runtime/mcp.rs`（`McpLease`、`SessionMcpState.lease` / `host`、`SessionMcpHost.routes`、`SessionMcpRuntime::shutdown` 区分 lease）
- Modify: `crates/kernel/src/runtime/session.rs`（`SessionMcpSource::Share(&SessionMcpRuntime)`；owned 会话使用 `Share`；关闭时 `detach` 路由）
- Modify: `crates/kernel/src/runtime.rs`（`mcp_reconnect`、`mcp_continue_authorization` 在 Shared 会话上返回 `KernelError::SharedMcpManagement(SessionId)`）
- Create: `crates/kernel/tests/subagent/mcp.rs`；fixture 增加 `CountingConnector`（参照 `crates/kernel/tests/mcp_host.rs:201-375` 的 `SamplingConnector` / `SamplingClient`，`connect` 时计数）

**Interfaces:**
- Consumes: Task 4 `build_session(.., SessionMcpSource, ..)`。
- Produces:
  ```rust
  pub(super) enum SessionMcpSource<'a> { Connect, Share(&'a SessionMcpRuntime) }
  pub(super) enum McpLease { Owned, Shared }
  impl SessionMcpHost {
      pub(super) fn attach(&self, runtime: &Arc<Session>) -> Result<(), KernelError>;
      pub(super) fn detach(&self, session_id: &SessionId);
  }
  ```

- [ ] **Step 1: 写失败测试**（`tests/subagent/mcp.rs`）

```rust
#[tokio::test]
async fn owned_session_reuses_owner_mcp_connection() {
    let child = kernel.create_owned_session(..).await.expect("child");
    assert_eq!(connector.connects(), 1);
    assert!(kernel.mcp_status(&child).expect("status").catalog.tools.len() > 0);
}

#[tokio::test]
async fn child_elicitation_routes_to_child_session() {
    // adapt mcp_host.rs:522: the child's run calls the MCP tool that elicits
    let pending = kernel.mcp_elicitations(&child).expect("child elicitations");
    assert_eq!(pending.items.len(), 1);
    assert!(kernel.mcp_elicitations(&owner).expect("owner").items.is_empty());
}

#[tokio::test]
async fn shared_session_rejects_mcp_management() {
    let result = kernel.mcp_reconnect(McpReconnectRequest { session_id: child.clone(), server_id }).await;
    assert!(matches!(result, Err(KernelError::SharedMcpManagement(_))));
}

#[tokio::test]
async fn closing_child_keeps_owner_mcp_alive() {
    kernel.close_session(&child).await.expect("close child");
    // owner's next run still calls the MCP tool successfully
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent mcp::` → FAIL（connects == 2）。
- [ ] **Step 3: 实现** — Elicitation / Sampling 按 `request.context.session_id` 查 `routes`，缺失返回 `McpError::Host("MCP Host route is unavailable for session {id}")`。日志：`attached MCP route for session {} to owner {}`、`detached MCP route for session {}`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent mcp::`；回归 `rtk cargo test -p kernel --test mcp_host --test mcp_startup`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): share owner MCP connections with owned sessions`。

### Task 6：级联 close / delete / resume

**Files:**
- Modify: `crates/kernel/src/runtime/ownership.rs`（`impl Kernel { fn live_children(&self, owner: &SessionId) -> Result<Vec<SessionId>, KernelError>; }`，`ponytail:` 注释 O(n) 扫描）
- Modify: `crates/kernel/src/runtime/session.rs`（`close_session`、`delete_session`、`resume_session`）
- Test: `crates/kernel/tests/subagent/ownership.rs`

**Interfaces:**
- Consumes: Task 4、Task 5。
- Produces: 公开 API 签名不变。close 先取消 owner run、获取 gate、进入 closing，再递归关闭 live 子会话，然后执行现有收尾（Task 10 再插入 reporter 步骤）。递归 async 调用用 `Box::pin`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn closing_owner_closes_live_children() {
    kernel.close_session(&owner).await.expect("close");
    assert!(matches!(kernel.session_runtime(&child), Err(KernelError::SessionNotFound(_))));
}

#[tokio::test]
async fn deleting_owner_deletes_child_store() {
    kernel.delete_session(&owner).await.expect("delete");
    assert!(kernel.list_sessions(Some(&cwd)).expect("list").is_empty());
}

#[tokio::test]
async fn resuming_child_resumes_owner_first() {
    kernel.close_session(&owner).await.expect("close");
    kernel.resume_session(child.clone(), cwd.clone()).await.expect("resume child");
    assert!(kernel.session_runtime(&owner).is_ok());
    assert_eq!(connector.connects(), 2); // owner twice, child never
}

#[tokio::test]
async fn resuming_orphan_child_connects_its_own_mcp() {
    // close owner, remove the owner's JSONL file from list metadata path
    let before = connector.connects();
    kernel.resume_session(child.clone(), cwd.clone()).await.expect("resume");
    assert!(kernel.session_runtime(&child).is_ok());
    assert_eq!(connector.connects(), before + 1); // child connected its own MCP
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent ownership::` → 新测试 FAIL。
- [ ] **Step 3: 实现** — resume 从 `list(Some(&cwd))` 的 metadata 读 ownership。owner 文件缺失时 `tracing::warn!("owner {} of session {} is unavailable; connecting its own MCP", ..)` 并用 `Connect`。日志：`started cascading close of session {} owned by {}` 及结果。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent`；回归 `rtk cargo test -p kernel --test lifecycle_order`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): cascade close, delete, and resume through session ownership`。

### Task 7：前台 `subagent` 工具

**Files:**
- Modify: `crates/protocol/src/tool.rs:183`（`ToolResultDetails::Subagent { session_id: SessionId }`）
- Create: `crates/kernel/src/runtime/subagent/mod.rs`（`SubagentError`、注册入口）
- Create: `crates/kernel/src/runtime/subagent/foreground.rs`（`SubagentTool`）
- Modify: `crates/kernel/src/runtime/session.rs:930`（`ToolSearchTool` 之后注册）
- Create: `crates/kernel/tests/subagent/foreground.rs`

**Interfaces:**
- Consumes: Task 3 `submit`、Task 4 `create_owned_session`、`SUBAGENT_TOOL_NAMES`。
- Produces:
  ```rust
  pub(in crate::runtime) struct SubagentTool { session: Weak<Session>, kernel: Kernel }
  #[derive(Debug, thiserror::Error)]
  pub(in crate::runtime) enum SubagentError {
      #[error("{tool}: {source}")] Kernel { tool: &'static str, #[source] source: KernelError },
      #[error("{tool}: owning session is no longer available")] SessionGone { tool: &'static str },
  }
  impl From<SubagentError> for ToolError; // ToolError::Execution { tool, message }
  /// Registers both subagent tools unless an extension already provides the name.
  pub(in crate::runtime) fn register(runtime: &Arc<Session>, kernel: &Kernel) -> Result<(), KernelError>;
  ```
  参数 schema：`{"type":"object","properties":{"task":{"type":"string","description":"What the subagent should do"}},"required":["task"]}`。描述逐字用 spec。`register` 只是 `SubagentTool` / `BackgroundSubagentTool` 的注册入口（后者 Task 9 加入），extension 分区已有同名工具时跳过并 `tracing::info!`。

- [ ] **Step 1: 写失败测试**（`tests/subagent/foreground.rs`）

```rust
#[tokio::test]
async fn subagent_returns_child_answer() {
    // parent "delegate" → ToolCall subagent {"task":"Name three primes."}; child → "2, 3, 5"; parent → "done"
    assert_eq!(tool_result.blocks, vec![ContentBlock::Text { text: "2, 3, 5".into() }]);
    assert_eq!(tool_result.details, Some(ToolResultDetails::Subagent { session_id: child.clone() }));
    assert_eq!(child_summary.name.as_deref(), Some("subagent: Name three primes."));
    assert_eq!(child_summary.ownership, Some(SessionOwnership::Tool { session_id: owner.clone(), tool_call_id: call_id("call-1") }));
    assert!(!model.tool_names_for("Name three primes.").iter().any(|n| ["subagent", "background_subagent"].contains(&n.as_str())));
    assert!(kernel.session_runtime(&child).is_ok()); // child stays live after the call
}

#[tokio::test]
async fn aborting_parent_cancels_child() {
    // child route Hang; wait_until child RunStart in sinks; kernel.cancel_session(&owner)
    assert!(tool_result.is_error);
    assert_eq!(text(&tool_result), "Operation aborted");
}

#[tokio::test]
async fn stopping_child_directly_reports_stop() {
    // child Hang; kernel.cancel_session(&child); parent continues and answers "ok"
    assert_eq!(text(&tool_result), "Subagent was stopped before answering.");
    assert!(tool_result.is_error);
}

#[tokio::test]
async fn deleting_child_during_call_reports_stop() {
    // Review Focus 3: child Hang; kernel.delete_session(&child)
    assert_eq!(text(&tool_result), "Subagent was stopped before answering.");
}

#[tokio::test]
async fn blank_task_is_rejected_without_child() {
    // Review Focus 1: task "  "
    assert_eq!(text(&tool_result), "subagent needs a task.");
    assert_eq!(kernel.list_sessions(Some(&cwd)).expect("list").len(), 1);
}

#[tokio::test]
async fn parallel_calls_get_separate_children() {
    // Review Focus 2: one assistant turn with two subagent calls
    assert_eq!(owned_children(&kernel, &owner).len(), 2);
}

#[tokio::test]
async fn extension_subagent_tool_wins() {
    // static test extension registers tool "subagent" returning "extension"
    assert_eq!(text(&tool_result), "extension");
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent foreground::` → 编译失败。
- [ ] **Step 3: 实现 `SubagentTool::execute`** — 按 spec「前台」1–4 步。`Box::pin(self.kernel.submit(..))`；`Cancelled` 依据 `context.cancellation.is_cancelled()` 分流；标题为 task 前 60 个字符（按 `char` 截断）。日志：`started subagent {} for session {} call {}`、`completed subagent {}`、`subagent {} was stopped before answering`、`failed subagent {}: {}`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent`；回归 `rtk cargo test -p kernel`、`rtk cargo test -p protocol`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): add foreground subagent tool`。

### Task 8：ACP 安装 sink 与会话列表 ownership

**Files:**
- Modify: `crates/acp/src/projection.rs`（`pub(crate) struct ProjectionSinks(pub(crate) Arc<ProjectionRegistry>)` 实现 `SessionEventSinks`）
- Modify: `crates/acp/src/server.rs:465-480`（`AcpServerFactory::new` 安装；失败时 `tracing::warn!`）
- Modify: `crates/acp/src/server.rs:715-725`（`_meta.clawcode.ownership`）
- Test: `crates/acp/src/projection.rs` 的 `#[cfg(test)] mod tests`；`crates/acp/tests/extension.rs`

**Interfaces:**
- Consumes: Task 3 `SessionEventSinks`、`Kernel::submit`；Task 1 `SessionSummary.ownership`。

- [ ] **Step 1: 写失败测试**

```rust
// projection.rs mod tests
#[tokio::test]
async fn kernel_initiated_runs_are_projected() {
    kernel.install_event_sinks(Arc::new(ProjectionSinks(Arc::clone(&registry)))).expect("install");
    let feed = registry.subscribe_from_start(&session).expect("feed");
    kernel.submit(..).await.expect("submit");
    // the feed receives the RunStart group of the kernel-initiated run
}
// tests/extension.rs, reuse integration_kernel() and send_request()
#[tokio::test]
async fn session_list_reports_ownership() {
    assert_eq!(listed["sessions"][i]["_meta"]["clawcode"]["ownership"]["kind"], "tool");
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p acp kernel_initiated_runs_are_projected session_list_reports_ownership` → FAIL。
- [ ] **Step 3: 实现** — meta 用 `serde_json::to_value(&session.ownership)`，为 `None` 时写 `null`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p acp` 全部 PASS。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(acp): project kernel-initiated runs and session ownership`。

## 阶段二：后台 subagent

### Task 9：`background_subagent` 工具与 Reporter

**Files:**
- Create: `crates/kernel/src/runtime/subagent/background.rs`（`BackgroundSubagentTool`、`Reporter`、`SubagentName`）
- Modify: `crates/kernel/src/runtime/subagent/mod.rs`（`register` 加入后台工具）
- Modify: `crates/kernel/src/runtime.rs`（`Session` 加 `background: Arc<SessionBackgroundWork>`）
- Create: `crates/kernel/tests/subagent/background.rs`

**Interfaces:**
- Consumes: Task 3、Task 4、Task 6（`live_children`、resume）、Task 7（`SubagentError`、`register`）。
- Produces:
  ```rust
  pub(in crate::runtime) struct SessionBackgroundWork {
      pub(in crate::runtime) shutdown: CancellationToken,
      tasks: Mutex<JoinSet<()>>,
      reported: Mutex<HashMap<SessionId, HashSet<MessageId>>>,
  }
  impl SessionBackgroundWork {
      pub(in crate::runtime) fn spawn(&self, reporter: Reporter) -> Result<(), KernelError>;
      pub(in crate::runtime) async fn join(&self);
      fn mark_reported(&self, child: &SessionId, message: &MessageId) -> Result<bool, KernelError>; // false when already reported
  }
  /// Validated background subagent name: [A-Za-z0-9_-]{1,64}.
  pub(in crate::runtime) struct SubagentName(String);
  impl TryFrom<&str> for SubagentName { type Error = (); }
  ```
  `Reporter` 字段按 spec「Reporter」。
- 固定回复文案（spec 之外补充）：非法名称 `Invalid subagent name: use 1-64 letters, digits, '_' or '-'.`；回报截断后缀 `\n\n[subagent answer truncated; open the subagent session for the full answer]`。截断用 `tools::truncate_head(text, tools::DEFAULT_MAX_LINES, tools::TOOL_TEXT_BYTES)`。

- [ ] **Step 1: 写失败测试**（`tests/subagent/background.rs`）

```rust
#[tokio::test]
async fn spawn_reports_answer_to_idle_owner() {
    // owner "start reader" → background_subagent {"action":"spawn","name":"reader","message":"Summarize."}
    assert_eq!(text(&spawn_result), "Started reader.");
    wait_until(|| owner_user_texts().iter().any(|t| t == "[subagent reader answered, no reply needed] A whale.")).await;
    // the child's first message carries the identity line
    assert!(model.last_texts().iter().any(|t| t.starts_with("You are the subagent \"reader\". Answer the main agent's requests.")));
}

#[tokio::test]
async fn stop_suppresses_report() {
    assert_eq!(text(&stop_result), "Stopped reader.");
    // wait for child idle, then a 100 ms settle; no "[subagent reader" message in owner
}

#[tokio::test]
async fn status_lists_children() {
    assert_eq!(text(&empty_status), "No subagents.");
    assert_eq!(text(&busy_status), "reader: working");
}

#[tokio::test]
async fn user_errors_are_plain_replies() {
    assert_eq!(text(&dup), "reader already exists; use send.");
    assert_eq!(text(&missing), "No subagent named ghost.");
    assert_eq!(text(&no_name), "send needs a name.");
    assert_eq!(text(&no_message), "spawn needs a message.");
    assert_eq!(text(&bad_name), "Invalid subagent name: use 1-64 letters, digits, '_' or '-'."); // Review Focus 5
    assert!(!dup.is_error);
}

#[tokio::test]
async fn steered_messages_report_one_answer_once() {
    // child busy (gated); send steer twice; release → exactly one "[subagent reader answered" message
}

#[tokio::test]
async fn closed_child_is_idle_for_stop_and_status() {
    kernel.close_session(&child).await.expect("close child");
    assert_eq!(text(&status), "reader: idle");
    assert_eq!(text(&stop), "Stopped reader.");
}

#[tokio::test]
async fn send_resumes_closed_child() {
    // close child, send follow-up → report arrives
}

#[tokio::test]
async fn long_answer_report_is_truncated() {
    // Review Focus 4: child answers 30 KiB
    assert!(report.len() <= tools::TOOL_TEXT_BYTES + 200);
    assert!(report.ends_with("[subagent answer truncated; open the subagent session for the full answer]"));
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent background::` → 编译失败。
- [ ] **Step 3: 实现** — 动作语义与文案逐字按 spec「后台」表格。Reporter deliver / report 都用 `cancellation: None`，内容为 `ContentBlock::Text`；`submit` 出错时 `tracing::warn!` 后结束。日志：`started subagent report delivery {} -> {}`、`subagent {} answered for owner {}`、`skipped subagent report for {}: {}`（reason：cancelled / already reported / shutdown）。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent`；回归 `rtk cargo test -p kernel`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `feat(kernel): add background subagents with reporters`。

### Task 10：关闭顺序纳入 reporter

**Files:**
- Modify: `crates/kernel/src/runtime/session.rs`（`close_session` 按 spec「级联 close」①–⑤）
- Test: `crates/kernel/tests/subagent/background.rs`

**Interfaces:**
- Consumes: Task 6 close 流程、Task 9 `SessionBackgroundWork::{shutdown, join}`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn closing_owner_with_reporter_in_delivery_does_not_hang() {
    // spawn reader whose model Hangs; reporter is in deliver
    tokio::time::timeout(Duration::from_secs(5), kernel.close_session(&owner)).await.expect("no hang").expect("close");
    // reopen owner: no "[subagent reader" user message persisted
}
```

- [ ] **Step 2: 运行确认失败** — `rtk cargo test -p kernel --test subagent closing_owner_with_reporter` → 超时 FAIL。
- [ ] **Step 3: 实现** — 顺序：`background.shutdown.cancel()` → 取消 owner run、取 gate、`begin_closing` → 递归关闭子会话 → `background.join().await` → 现有 shutdown hook、MCP 关闭、同步与移除。日志：`abandoned subagent reports for closing session {}`。
- [ ] **Step 4: 运行确认通过** — `rtk cargo test -p kernel --test subagent`；回归 `rtk cargo test -p kernel --test lifecycle_order`。
- [ ] **Step 5: 检查点** — fmt + clippy；经允许后提交 `fix(kernel): close owners after their children and reporters`。

## 阶段三：WebUI

前端代码不写单测；每个任务以 `rtk npm run build`（在 `web/` 下）通过为准，最终在 Task 14 做端到端验收。

### Task 11：数据层：ownership 与附加会话

**Files:**
- Modify: `web/src/domain/model.ts`（`SessionOwnership` 类型；`SessionSummary.ownership?`）
- Modify: `web/src/workspace/controller.ts:70-110`（`sessionSummary` 解析 `_meta.ownership`；从 `openSession` 抽出 `attachSession(sessionId)`：加载 transcript、tree、pending、MCP，不切换活动会话、不改 URL；`openSession` 改为 `session/activated` 加 `attachSession`）

**Interfaces:**
- Produces: `type SessionOwnership = { kind: "tool"; sessionId: SessionId; toolCallId: string } | { kind: "background"; sessionId: SessionId; name: string }`；`controller.attachSession(sessionId: SessionId): Promise<void>`。

- [ ] **Step 1: 实现上述类型与方法。**
- [ ] **Step 2: 构建** — `cd web && rtk npm run build` 通过；`rtk lint` 无新增问题。
- [ ] **Step 3: 检查点** — 经允许后提交 `feat(webui): parse session ownership and attach sessions`。

### Task 12：`SubagentPanel`

**Files:**
- Create: `web/src/features/conversation/SubagentPanel.tsx`
- Modify: `web/src/features/conversation/ToolCallCard.tsx`（`rawOutput.details.type === "subagent"` 时渲染面板）
- Modify: `web/src/features/conversation/Conversation.tsx`（新增紧凑模式 prop `compact`）

**Interfaces:**
- Consumes: Task 11 `attachSession`。
- 行为：工具 `status` 为运行中时自动展开并 `attachSession(details.session_id)`；完成后折叠，可手动展开。面板内用 `<Conversation sessionId={child} compact />` 渲染子会话消息、工具调用与 MCP elicitation；「打开会话」按钮调用 `controller.openSession(child)`。

- [ ] **Step 1: 实现组件与接入。**
- [ ] **Step 2: 构建** — `cd web && rtk npm run build` 通过。
- [ ] **Step 3: 检查点** — 经允许后提交 `feat(webui): show subagent sessions inside tool cards`。

### Task 13：侧栏嵌套、后台面板、回报渲染、MCP 只读

**Files:**
- Modify: `web/src/features/sessions/SessionSidebar.tsx`（有 `ownership` 的会话折叠在 owner 下，显示标题）
- Create: `web/src/features/conversation/BackgroundSubagents.tsx`（列出 `ownership.kind === "background"` 且 owner 为当前会话的子会话；状态复用 runtime 轮询；「打开」「停止」，停止调用 `controller.cancel(child)`）
- Modify: `web/src/features/conversation/Conversation.tsx`（用户消息匹配 `^\[subagent (\S+) ([^\]]*)\] ?(.*)$` 时渲染回报样式：名称加正文）
- Modify: `web/src/features/mcp/` 中的 MCP 面板（当前会话有 ownership 时隐藏重连 / OAuth 操作，显示「连接由 owner 会话管理」）

- [ ] **Step 1: 实现。**
- [ ] **Step 2: 构建** — `cd web && rtk npm run build` 通过。
- [ ] **Step 3: 检查点** — 经允许后提交 `feat(webui): nest subagent sessions and render reports`。

### Task 14：端到端验收

- [x] **Step 1: 启动生产后端** — 按项目 `run` 方式启动 `app`（使用已配置 provider），打开 WebUI。
- [x] **Step 2: 前台验收** — 发送「用 subagent 工具列出当前目录下的 Cargo crate」：工具卡片运行中自动展开并实时显示子会话；结束后折叠；「打开会话」能进入子会话继续追问；侧栏中子会话折叠在 owner 下。
- [x] **Step 3: 中止验收** — 子会话运行中点击父会话停止：子会话停止，父工具显示 `Operation aborted`。
- [x] **Step 4: 后台验收** — 发送「用 background_subagent spawn 一个 reader 总结 README」：父会话立即回复；稍后出现 reader 回报样式消息并自动运行一轮；后台面板显示 working → idle；「停止」可停止运行中的 reader。
- [x] **Step 5: 记录结果** — 截图与结论写入 `docs/superpowers/reports/2026-10-28-subagent-verification.md`（中文）。

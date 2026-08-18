# Session 运行时状态拆分实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task. 当前项目禁止 SubAgent 和 worktree，因此只能在当前会话内联执行。步骤使用 checkbox（`- [ ]`）跟踪。

**目标：** 将 `SessionRuntime` 重命名为 `Session`，并把 26 个字段重组为职责明确、保持现有并发与持久化语义的 Session 领域组件。

**架构：** `Session` 作为门面持有 transcript、resources、model、tools、extensions、MCP 和 execution 组件。组件拥有自己的锁和状态机；跨组件 Queue 操作由 `Session` 编排，严格保留 durable-first 顺序。

**技术栈：** Rust、Tokio、`std::sync::{Arc, Mutex, OnceLock, RwLock}`、`typed-builder`、Kernel 集成测试。

**Spec：** `docs/superpowers/specs/2026-08-18-session-runtime-refactor-design.md`

## 全局约束

- 所有 spec 和 plan 使用中文。
- 所有新增函数提供英文函数级注释；修改旧逻辑时用英文注释说明原因。
- 超过 3 个字段的结构使用 `typed-builder` 并通过 builder 构造；`Option` 字段使用 `#[builder(default)]`。
- 克隆 `Arc<T>` 字段时使用 `Arc::clone(&value)`。
- 不新增 crate 或第三方依赖，不修改公开协议、JSONL 格式和 Extension Hook 顺序。
- Rust 测试只放在 crate 级 `tests/` 或标准 `#[cfg(test)] mod tests` 中。
- 所有命令使用 `rtk` 前缀。
- 不使用 SubAgent，不创建 worktree，不创建 commit。

---

### 任务 1：建立基线并完成 Session 类型改名

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/**/*.rs`

**接口：**

- 将私有类型 `SessionRuntime` 重命名为 `Session`。
- 将 `Kernel.sessions` 改为 `Arc<RwLock<HashMap<SessionId, Arc<Session>>>>`。
- 将 `build_session_runtime(...) -> Result<Arc<SessionRuntime>, KernelError>` 改为 `build_session(...) -> Result<Arc<Session>, KernelError>`。
- 不创建 `type SessionRuntime = Session` 兼容别名。

- [x] **步骤 1：运行重构前基线测试**

运行：

```bash
rtk cargo test -p kernel --test session_capabilities --test lifecycle_order --test extensions_host --test mcp_host
```

预期：全部通过，记录现有基线。

- [x] **步骤 2：执行纯符号改名**

修改所有 `SessionRuntime` 类型引用、`impl SessionRuntime`、参数和返回类型；同时将 `build_session_runtime` 的定义及三个调用点改为 `build_session`。只改符号，不调整字段和行为。

- [x] **步骤 3：验证改名完整且可编译**

运行：

```bash
rtk rg -n "SessionRuntime|build_session_runtime" crates/kernel/src crates/kernel/tests
rtk cargo check -p kernel --all-targets
```

预期：搜索无结果，Kernel 全目标检查通过。

---

### 任务 2：提取 SessionResources 与 SessionModelState

**文件：**

- 新增：`crates/kernel/src/runtime/resources.rs`
- 新增：`crates/kernel/src/runtime/model_state.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/prompt.rs`
- 修改：`crates/kernel/src/runtime/input.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 测试：`crates/kernel/src/runtime/resources.rs` 中的 `#[cfg(test)] mod tests`
- 测试：`crates/kernel/src/runtime/model_state.rs` 中的 `#[cfg(test)] mod tests`

**接口：**

```rust
struct SessionResources {
    prompt: Arc<::prompt::PromptSession>,
    skills: Option<Arc<skill::SkillCatalog>>,
}

struct ModelSelection {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
}

struct SessionModelState {
    catalog: Arc<ModelCatalog>,
    selection: RwLock<ModelSelection>,
}
```

- `SessionResources::new(prompt, skills) -> Self`
- `SessionResources::prompt(&self) -> &Arc<PromptSession>`
- `SessionResources::skills(&self) -> Option<&Arc<SkillCatalog>>`
- `SessionModelState::new(catalog, model, thinking_level) -> Self`
- `SessionModelState::active(&self) -> Result<Arc<dyn Model>, KernelError>`
- `SessionModelState::thinking_level(&self) -> Result<ThinkingLevel, KernelError>`
- `SessionModelState::snapshot(&self) -> Result<(Arc<dyn Model>, ThinkingLevel), KernelError>`
- `SessionModelState::resolve(&self, provider_id, model_id) -> Option<Arc<dyn Model>>`
- `SessionModelState::profiles(&self) -> Vec<ModelProfile>`
- `SessionModelState::replace_model(&self, model) -> Result<(), KernelError>`
- `SessionModelState::replace_thinking_level(&self, level) -> Result<(), KernelError>`

- [x] **步骤 1：写 Resources 与 ModelState 的 RED 测试**

在两个新模块的标准测试模块中定义期望 API。Resources 测试使用 `PromptSession::builder()` 构造最小 Prompt，并验证 Prompt 与 `None` Skill 一次组成完整资源；ModelState 测试使用本地 `TestModel`，验证一次 snapshot 返回同一 Model 和 Thinking Level，并验证分别替换 Model、Thinking 时不会覆盖另一个字段。

关键断言：

```rust
let (model, level) = state.snapshot().expect("model snapshot");
assert_eq!(model.profile().model_id, "secondary");
assert_eq!(level, ThinkingLevel::High);
```

- [x] **步骤 2：运行 RED**

运行：

```bash
rtk cargo test -p kernel --lib runtime::resources::tests
rtk cargo test -p kernel --lib runtime::model_state::tests
```

预期：因模块或目标类型尚不存在而编译失败。

- [x] **步骤 3：实现最小组件并替换字段**

在 `Session` 中将 `prompt + skills` 替换为 `resources: OnceLock<SessionResources>`，将 `models + model + thinking_level` 替换为 `model: SessionModelState`。`start_extensions` 必须先成功创建 Prompt 与 Skills，再执行一次 `resources.set(...)`；所有读取通过组件方法完成。

Model 更新继续保持“写 Store → 原地修改目标字段 → 发 Extension 事件”，不得用旧 snapshot 替换整个 `ModelSelection`。

- [x] **步骤 4：运行 GREEN 与资源/模型集成测试**

运行：

```bash
rtk cargo test -p kernel --lib runtime::resources::tests
rtk cargo test -p kernel --lib runtime::model_state::tests
rtk cargo test -p kernel --test session_capabilities --test session_extension_state
```

预期：全部通过。

---

### 任务 3：提取 SessionExecution 并原子化 Active Run

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/lifecycle.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/extension/diagnostics.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/src/runtime/command/execution.rs`
- 测试：`crates/kernel/src/runtime/lifecycle.rs` 中的 `#[cfg(test)] mod tests`
- 测试：`crates/kernel/tests/extensions_host.rs`

**接口：**

```rust
#[derive(Clone)]
struct ActiveRunContext {
    run_id: RunId,
    turn_id: TurnId,
    sink: Arc<dyn EventSink>,
}

#[derive(typed_builder::TypedBuilder)]
struct SessionExecution {
    lifecycle: RwLock<SessionLifecycle>,
    gate: AsyncMutex<()>,
    active_run: Mutex<Option<ActiveRunContext>>,
    idle_notify: Notify,
    cancellation: Mutex<CancellationToken>,
    queue: Mutex<PendingQueue>,
    sequence: AtomicU64,
}
```

- `SessionExecution::acquire_operation(&self) -> Result<MutexGuard<'_, ()>, KernelError>`
- `SessionExecution::try_acquire_operation(&self) -> Result<Option<MutexGuard<'_, ()>>, KernelError>`
- `SessionExecution::install_cancellation(&self) -> Result<CancellationToken, KernelError>`
- `SessionExecution::cancel(&self) -> Result<(), KernelError>`
- `SessionExecution::active_run(&self) -> Result<Option<ActiveRunContext>, KernelError>`
- `SessionExecution::install_run(self: &Arc<Self>, context) -> Result<ActiveRunLease, KernelError>`
- `SessionExecution::next_sequence(&self) -> Result<Sequence, KernelError>`
- lifecycle、queue snapshot/insert/remove 方法保持 spec 的 owned-value 约束。
- `ActiveRunLease` 改为持有 `Arc<SessionExecution>` 与 `RunId`。

- [x] **步骤 1：写 Active Run 与 lifecycle RED 测试**

测试覆盖：安装 Run 后一次 snapshot 同时返回 run、turn、sink；Drop lease 后 snapshot 为 `None`；等待 gate 的 operation 在 lifecycle 变为 Closing 后被拒绝；`wait_for_idle` 在 lease Drop 后结束。

关键测试结构：

```rust
let lease = execution.install_run(context).expect("install run");
assert_eq!(execution.active_run().expect("snapshot").unwrap().run_id, run_id);
drop(lease);
assert!(execution.active_run().expect("snapshot").is_none());
```

- [x] **步骤 2：运行 RED**

运行：

```bash
rtk cargo test -p kernel --lib runtime::lifecycle::tests
```

预期：因 `SessionExecution` 和新 API 尚不存在而编译失败。

- [x] **步骤 3：实现 SessionExecution 并迁移调用方**

将 lifecycle、gate、active Run、idle、cancellation、queue、sequence 收入组件。必须保留 acquire 前后 lifecycle 双检、Extension Command 调用 handler 前释放 gate、Drop 中按 run_id 清除后再 notify，以及事件序号共享。

`KernelExtensionDiagnostics` 改为持有 `Arc<SessionExecution>`，通过组件方法读取当前 Turn/sink 和分配 sequence；不持有 execution 锁跨越 sink `.await`。

- [x] **步骤 4：运行 GREEN 与并发生命周期测试**

运行：

```bash
rtk cargo test -p kernel --lib runtime::lifecycle::tests
rtk cargo test -p kernel --test extensions_host shutdown_reentrant_operations_are_rejected
rtk cargo test -p kernel --test slash_commands
rtk cargo test -p kernel --test turn
```

预期：全部通过且无超时。

---

### 任务 4：提取 SessionTranscript 并迁移复合状态变更

**文件：**

- 新增：`crates/kernel/src/runtime/transcript.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/session/replay.rs`
- 修改：`crates/kernel/src/runtime/tree.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/user_bash.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/extension/diagnostics.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 测试：`crates/kernel/src/runtime/transcript.rs` 中的 `#[cfg(test)] mod tests`
- 测试：`crates/kernel/tests/compaction.rs`
- 测试：`crates/kernel/tests/lifecycle_order.rs`
- 测试：`crates/kernel/tests/session_capabilities.rs`

**接口：**

```rust
struct SessionTranscript {
    lane: LaneId,
    state: Mutex<SessionTranscriptState>,
}

struct SessionTranscriptState {
    store: Box<dyn SessionStore>,
    history: Vec<AgentMessage>,
}
```

- `SessionTranscript::new(store, lane, history) -> Self`
- `SessionTranscript::session_id/path/name/history/leaf_id/tree_snapshot` 返回 owned value。
- `SessionTranscript::append_context_message(entry_id, message)` 同锁更新 Store 与 history。
- `SessionTranscript::append_store_only_entry(entry)` 与 `append_record(record)` 不改变 history。
- `SessionTranscript::commit_compaction(entry, context)` 同锁追加并替换 history。
- Tree planning 返回 owned `TreeNavigationPlan`；commit 方法在同锁内 move lane、可选 append summary、重建 history。
- `SessionTranscript::sync()`、`set_name()`、`set_label()` 和 `append_extension_entry()` 封装对应 Store 行为。

- [x] **步骤 1：写 Transcript RED 测试**

测试使用 `JsonlStoreFactory` 创建真实临时 SessionStore，验证：Context Message 同时出现在 Store tree 和 history；Store-only record 不进入 history；Compaction commit 后 history 只包含 summary 与 retained tail；重新打开 Store 后恢复 history 与内存一致。

关键断言：

```rust
assert_eq!(transcript.history().expect("history"), expected_context);
assert_eq!(replayed_history, expected_context);
```

- [x] **步骤 2：运行 RED**

运行：

```bash
rtk cargo test -p kernel --lib runtime::transcript::tests
```

预期：因 `SessionTranscript` 尚不存在而编译失败。

- [x] **步骤 3：实现 Transcript 组件并迁移简单读写**

先迁移 session metadata、message append、record append、history snapshot、sync、rename 和 extension entry。所有方法只返回 owned 数据，不返回 Store 或 Mutex guard。

- [x] **步骤 4：迁移 Compaction 与 Tree 复合操作**

Compaction 必须通过一次 transcript 锁完成 entry append 与 history replacement。Tree Hook 前只获取 owned plan；Hook await 后通过一次 transcript 锁完成 lane move、summary append 和 history rebuild。不得持有 transcript 锁跨越 Extension Hook 或 Model await。

- [x] **步骤 5：运行 GREEN 与恢复回归**

运行：

```bash
rtk cargo test -p kernel --lib runtime::transcript::tests
rtk cargo test -p kernel --test compaction --test lifecycle_order --test session_capabilities
```

预期：全部通过。

---

### 任务 5：在 GREEN 状态下迁移 Queue durable-first 跨组件协议

**文件：**

- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

- `Session::enqueue_pending(...)`：record → sync → execution queue insert。
- `Session::remove_pending(...)`：owned ID snapshot → cancellation record → sync → execution queue remove。
- `Session::clear_pending(...)`：owned ID snapshot → cancellation records → sync → execution queue removals。
- `Session::consume_pending(...)`：message append → consumed record → execution queue remove；settlement 负责 sync。
- 这些方法调用 transcript 前不持有 queue 锁。

- [x] **步骤 1：补充 Queue 竞争特征测试**

在 `session_capabilities.rs` 增加测试：阻塞一个活跃 Run，在 Run 即将结束时并发 enqueue follow-up，随后关闭并 Resume，断言该 QueueId 只恢复一次；消费后再次 Resume，断言消息不复活。

- [x] **步骤 2：运行特征测试确认现有 durable-first 行为**

运行：

```bash
rtk cargo test -p kernel --test session_capabilities queue_racing_final_checkpoint_recovers_exactly_once
```

预期：测试通过，证明当前外部行为已经满足 exactly-once 恢复要求。该任务属于前面 RED/GREEN 周期之后的纯 REFACTOR，不引入新行为；后续每次迁移后重复运行该测试，保持 GREEN。

- [x] **步骤 3：实现 Session 门面编排并迁移 Queue 调用**

严格按 spec 6.7 的四类顺序实现。Queue 方法只对内存容器执行短临界区操作，Transcript 方法只执行持久化操作，两类锁不嵌套。

- [x] **步骤 4：运行 GREEN 与 durability 测试**

运行：

```bash
rtk cargo test -p kernel --test session_capabilities queue
rtk cargo test -p kernel --test lifecycle_order queue_mutations_sync_before_returning
```

预期：全部通过。

---

### 任务 6：提取 SessionExtensions 与 SessionMcpRuntime，补全失败清理

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/extension.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/src/runtime/extension/provider.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/tests/mcp_host.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

```rust
#[derive(typed_builder::TypedBuilder)]
struct SessionExtensions {
    runtime: Arc<ExtensionRuntime>,
    commands: DynamicCommandRegistry,
    flags: Arc<[ExtensionFlagDefinition]>,
    events: broadcast::Sender<ExtensionEventData>,
}

enum SessionMcpRuntime {
    Disabled,
    Enabled(SessionMcpState),
}

#[derive(typed_builder::TypedBuilder)]
struct SessionMcpState {
    session: Arc<::mcp::McpSession>,
    projection: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    elicitations: Mutex<HashMap<String, PendingMcpElicitation>>,
}
```

- Extensions 提供 runtime Arc clone、Command snapshot/upsert、Flags snapshot、event publish/subscribe。
- MCP 提供 status、subscribe、session handle、elicitation snapshot/insert/remove、start/stop projection 和 shutdown。
- Disabled 分支严格返回 spec 6.5 定义的默认值或 `ServerNotFound`。
- `build_session` 使用统一异步错误出口关闭已创建的 MCP 和 projection。

- [x] **步骤 1：写 MCP Disabled 与构造清理 RED 测试**

增加测试覆盖：无 MCP factory 时 status/default、subscribe/None、elicitation/empty 和请求错误；可观察 MCP factory 在后续构造失败时收到 shutdown；Resume 的 `session_before_switch` 取消时 shutdown 已创建 MCP 且不删除 Store。

- [x] **步骤 2：运行 RED**

运行：

```bash
rtk cargo test -p kernel --test mcp_host disabled_mcp_preserves_public_semantics
rtk cargo test -p kernel --test session_capabilities resume_cancellation_shuts_down_unregistered_mcp
```

预期：Disabled 聚合 API 尚不存在或 Resume 取消后 shutdown 计数仍为 0，测试失败。

- [x] **步骤 3：实现 Extensions 与 MCP 组件**

迁移所有直接字段访问。Provider Hook 使用 `Arc::clone` 的 Extension Runtime；MCP projection task 只捕获 MCP Session 与 ToolState，不捕获顶层 `Session`。

- [x] **步骤 4：实现统一构造错误出口与关闭顺序**

MCP 创建后的 fallible 构造步骤放入一个 `Result` 路径；错误时 invalidate 已创建 Extension、shutdown MCP、等待已启动 projection，再返回原始错误。注册后 startup rollback、注册前 Resume 取消和正常 close 分别保留 spec 指定的删除与 Hook 顺序。

- [x] **步骤 5：运行 GREEN 与 MCP/Extension 回归**

运行：

```bash
rtk cargo test -p kernel --test mcp_host --test session_capabilities --test extensions_host --test session_extension_state
```

预期：全部通过。

---

### 任务 7：收口 Session 门面并执行完整验证

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：所有仍直接展开组件内部锁的 `crates/kernel/src/runtime/**/*.rs`

**完成条件：**

`Session` 最终只保留：

```rust
struct Session {
    cwd: PathBuf,
    transcript: Arc<SessionTranscript>,
    resources: OnceLock<SessionResources>,
    model: SessionModelState,
    tools: Arc<SessionToolState>,
    extensions: SessionExtensions,
    mcp: SessionMcpRuntime,
    execution: Arc<SessionExecution>,
}
```

- [x] **步骤 1：搜索遗留名称和直接锁访问**

运行：

```bash
rtk rg -n "SessionRuntime|build_session_runtime|session\.(store|lane|history|run_gate|active_run_id|event_sink|diagnostic_turn_id|cancellation|queue|models|thinking_level|commands|flags|event_bus|mcp_projection|pending_mcp_elicitations)" crates/kernel/src
```

预期：旧类型和旧字段搜索无结果；组件内部实现中的自有字段访问除外。

- [x] **步骤 2：格式化并检查差异**

运行：

```bash
rtk cargo fmt --all
rtk git diff --check
rtk git diff --stat
```

预期：无格式错误和 whitespace 错误，差异只涉及本次 spec、plan、Kernel 实现与测试。

- [x] **步骤 3：运行 Kernel 全量测试**

运行：

```bash
rtk cargo test -p kernel
```

预期：全部通过。

- [x] **步骤 4：运行 workspace 检查与 Clippy**

运行：

```bash
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

预期：全部通过且无 warning。

- [x] **步骤 5：核对工作区且不提交**

运行：

```bash
rtk git status --short
rtk git diff --name-only
```

预期：仅显示本计划范围内文件；不执行 `git add` 或 `git commit`。

### 任务 8：修复最终 review 发现的并发与部分提交窗口

- [x] **步骤 1：先增加三个回归测试并确认 RED**

覆盖未注册 Resume 与 Delete 竞争、live Resume Hook 与 Close 竞争，以及 branch summary 后首次 `sync` 失败但重试成功的场景。

- [x] **步骤 2：统一 SessionId 互斥预留并补充 live Resume 生命周期仲裁**

Delete 从 Close 前到 Store 删除完成持有预留；live Resume 在 Hook 返回后获取 operation gate 并重新检查 lifecycle。

- [x] **步骤 3：区分 navigation 的可恢复 durability 失败**

仅当逻辑变更已全部完成、history 对齐成功且重试 `sync` 成功时返回 committed result；其他部分提交继续保留原始错误。

- [x] **步骤 4：执行完整验证且不提交**

运行格式化、目标测试、Kernel 与 workspace 全量测试、严格 Clippy 和差异检查；不执行 `git add` 或 `git commit`。

### 任务 9：封闭启动可见性、补偿根导航并聚合 Tool 模块

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/lifecycle.rs`
- 修改：`crates/kernel/src/runtime/transcript.rs`
- 新增：`crates/kernel/src/runtime/tools/mod.rs`
- 移动：`crates/kernel/src/runtime/tools/{batch,state,update}.rs`
- 测试：`crates/kernel/tests/session_capabilities.rs`

- [x] **步骤 1：增加启动公开访问与根导航失败的 RED 测试**

使用可阻塞的 `SessionStartPoint` Extension 证明注册后的半初始化 Session 曾可被 Close；使用真实 Jsonl Store 外包一层单次同步失败注入，证明 root navigation 曾在返回错误后丢失 leaf 和 history。

- [x] **步骤 2：增加 Starting 生命周期并延后公开激活**

`build_session` 以 `Starting` 构造 execution。注册后的启动流程直接持有 runtime 引用，资源安装和 checkpoint 完成后调用 `finish_starting`；按 SessionId 进入的普通操作统一拒绝 `Starting` 状态。

- [x] **步骤 3：补偿 root navigation 的同步失败**

移动 lane 前保存旧 leaf，清空 history 时保留旧值；首次同步失败后恢复 lane、history 并再次同步。补偿失败时按 Store 当前 lane 重建 history，同时保留原始错误作为返回值。

- [x] **步骤 4：聚合 Tool 执行模块**

将原 `tool_batch.rs`、`tool_state.rs`、`tool_updates.rs` 分别移动为 `runtime/tools/batch.rs`、`state.rs`、`update.rs`，并由 `tools/mod.rs` 以最小可见性重导出。外部 `tools` crate 引用改用绝对路径。

- [x] **步骤 5：执行完整验证且不提交**

运行目标回归、Kernel/Workspace 全量测试、严格 Clippy、格式与差异检查；不执行 `git add` 或 `git commit`。

### 任务 10：修复最终 review 发现的动态快照时序问题

**文件：**

- 修改：`crates/kernel/src/runtime/tools/state.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/transcript.rs`
- 修改：`crates/kernel/src/runtime/tree.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/kernel/tests/lifecycle_order.rs`
- 测试：`crates/kernel/tests/session_capabilities.rs`

- [x] **步骤 1：增加动态 Tool、Root 事件和 Queue 诊断 RED 测试**

覆盖持久化动态 Tool 离线恢复、Tool 验证后目录变化、Root 事件旧叶节点，以及 Skill 同步展开期间 Run 推进 Turn 的场景；逐项确认旧实现以预期原因失败。

- [x] **步骤 2：把 activeTools 定义为可恢复偏好**

恢复时只把当前可用且被选择的 Tool 投影到 active registry；暂时不可用的选择不阻断 Session。Host 使用同一个已验证快照更新 disabled preference，避免目录并发变化造成持久化与内存状态分裂。

- [x] **步骤 3：修正导航和诊断的快照读取时机**

`clear_navigation` 返回变更前 leaf，Root 事件直接使用该值；Queue 在输入展开后重新读取同一 Run 的 `ActiveRunContext`，以最新 TurnId 和对应 sink 发布 Skill 诊断。

- [x] **步骤 4：执行完整验证且不提交**

运行格式化、目标回归、Kernel/Workspace 全量测试、严格 Clippy 和差异检查；不执行 `git add` 或 `git commit`。

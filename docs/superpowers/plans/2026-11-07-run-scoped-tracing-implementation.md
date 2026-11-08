# Run 级 Tracing 实施计划

> **实施后变更（2026-11-08）：** 本计划中自定义格式器、`CorrelationLayer`/`SpanCorrelation` 与 `[trace run]` 前缀相关的任务已被 tracing-subscriber 默认格式替代，见 [spec 顶部说明](../specs/2026-11-07-run-scoped-tracing-design.md)。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让一个 trace 对应一个顶层 Kernel Run，而不是一条 ACP 消息；同时清理日志层（模型调用一对日志、去掉 span 生命周期事件、颜色自动判断、依赖库事件显示 target）。

**Architecture:** Kernel 在每个生成 `RunId` 的位置用 `RunSpan` 打开 `run` span（顶层声明 `trace_id` 与 `run_id`，嵌套只声明 `run_id`），trace 来源由 `TraceOrigin` 决定；ACP 不再生成 trace，只按请求类别记日志；app 的 `CorrelationLayer` 记录两个字段，格式化器输出最近的 `[trace run]`。

**Tech Stack:** Rust、tracing 0.1.44（`{ CONST }` 字段名）、tracing-subscriber、rig-core 0.46、agent-client-protocol 5、typed-builder。

**Spec:** `docs/superpowers/specs/2026-11-07-run-scoped-tracing-design.md`（以下简称"spec"，章节号指它）。

## Global Constraints

- 不新增依赖；Cargo 依赖表规则（CLAUDE.md 11、12）不变。删除失去用途的依赖项只在其所在 crate 的 `Cargo.toml` 中进行。
- span 字段名只用 `protocol::TRACE_ID_FIELD`（`"trace_id"`）与 `protocol::RUN_ID_FIELD`（`"run_id"`）；`run` span 只由 `kernel::RunSpan` 创建，级别为 ERROR。
- tracing 事件宏写全路径（`tracing::info!`），正文是可读文本加格式化参数，不用结构化事件字段（规则 14）。
- 新函数写英文文档注释，非平凡逻辑写英文注释（规则 1）；超过 3 个字段的结构体用 typed-builder，`Option` 字段加 `#[builder(default)]`（规则 5）；`Arc::clone(&x)`（规则 6）；测试只放在 crate 级 `tests/`（规则 10）。
- ACP DEBUG 参数日志及其递归脱敏不变，INFO 不记录参数（规则 13）。
- 测试代码不加 `#[allow(dead_code)]`：只被部分测试二进制使用的辅助代码放进单独文件，由需要的二进制用 `#[path = "…"] mod …;` 引入（先例：`crates/kernel/tests/provider_runtime.rs:9`）。
- 执行期间不 `git commit`；用户明确要求时才提交。每个任务以检查点结束。
- 每个任务结束时整个 workspace 必须能编译：`cargo build --workspace --tests`。

## Review Focus

1. 过滤器为 `warn` 时，Run 内的 WARN 行仍带 `[trace run]`（span 用 ERROR 级别）——Task 3 的 `run_span_prefix_survives_a_warn_filter`。
2. 同一工具批次里并发的两个前台子代理：两个子 Run 的行都带父 trace、各自不同的 run id——Task 6 的 `concurrent_foreground_children_keep_distinct_runs`。
3. 提供方第一次请求失败、重试成功：失败的 WARN 行与之后的完成行属于同一个 trace——Task 8 的 `failed_and_retried_provider_requests_share_the_run_trace`。
4. `NO_COLOR=""`（设置为空）视为未设置，仍允许颜色——Task 2 的颜色表格。
5. 会话忙时提交的输入在运行中的 Run 内处理，不生成新 trace——Task 3 的 `queued_follow_up_runs_under_the_running_trace`。

---

### Task 1: protocol 中的共享定义

**Files:**
- Create: `crates/protocol/src/trace.rs`
- Modify: `crates/protocol/src/lib.rs`（`mod trace;` 与 `pub use trace::{RUN_ID_FIELD, TRACE_ID_FIELD};`）
- Modify: `crates/protocol/src/identity.rs`（`impl AcpExtensionMethod`，约 174 行）
- Modify: `crates/protocol/src/usage.rs`（`ModelUsage`）
- Test: Create `crates/protocol/tests/logging.rs`

**Interfaces:**
- Produces: `pub const protocol::TRACE_ID_FIELD: &str = "trace_id"`、`pub const protocol::RUN_ID_FIELD: &str = "run_id"`；`pub const fn AcpExtensionMethod::is_query(self) -> bool`；`impl fmt::Display for protocol::usage::ModelUsage`。

- [ ] **Step 1: 写失败测试** `crates/protocol/tests/logging.rs`

```rust
use protocol::AcpExtensionMethod as M;
use protocol::usage::{ModelUsage, Usage};

#[test]
fn query_methods_are_exactly_the_read_only_snapshots() {
    let queries = [M::Tree, M::PendingMessages, M::SessionRuntime, M::SkillList,
        M::McpStatus, M::McpPromptGet, M::McpResourceRead, M::McpComplete];
    let changes = [M::Navigate, M::Branch, M::Fork, M::Compact, M::PendingMessageRemove,
        M::PendingMessagePromote, M::ClearQueue, M::InvokeSkill, M::McpReconnect,
        M::McpOAuthContinue, M::ExtensionCommand, M::UserBash];
    assert!(queries.iter().all(|method| method.is_query()));
    assert!(changes.iter().all(|method| !method.is_query()));
}

#[test]
fn usage_text_lists_only_reported_counts() {
    let mut usage = Usage::default();
    usage.input_tokens = Some(10);
    usage.output_tokens = Some(0);
    usage.reasoning_tokens = Some(3);
    assert_eq!(ModelUsage::from(usage).to_string(), "input 10, output 0, reasoning 3");
}

#[test]
fn usage_text_without_counts_says_so() {
    assert_eq!(ModelUsage::default().to_string(), "no usage reported");
}
```

（`Usage` 是 `#[non_exhaustive]`，只能从 `Usage::default()` 起逐字段赋值。两个字段名常量没有独立测试，Task 2、3 的测试实际使用它们。）

- [ ] **Step 2: 确认失败** —— `cargo test -p protocol --test logging`，预期编译失败（`is_query`、`Display` 不存在）。

- [ ] **Step 3: 实现**
  - `trace.rs`：两个带文档注释的常量；模块注释说明它们是 Kernel 的 Run span 与日志格式化器之间的约定。
  - `is_query`：对所有变体写穷举 `match`（不用 `matches!`），新增变体时编译器强制决定类别；查询为上面 8 个。
  - `Display for ModelUsage`：按顺序输出已报告的项，键名与字段对应：`input`←`input_tokens`、`output`←`output_tokens`、`cache read`←`cached_input_tokens`、`cache write`←`cache_creation_input_tokens`、`reasoning`←`reasoning_tokens`；以 `", "` 连接；一项都没有时输出 `no usage reported`。

- [ ] **Step 4: 确认通过** —— `cargo test -p protocol --test logging`，3 个测试 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 2: app 日志层——前缀、无生命周期事件、颜色、依赖库来源

**Files:**
- Modify: `crates/app/src/logging.rs`
- Modify: `crates/app/src/logging/trace.rs`
- Modify: `crates/app/src/logging/format.rs`
- Delete: `crates/app/src/logging/telemetry.rs`
- Test: `crates/app/tests/logging.rs`

**Interfaces:**
- Consumes: `protocol::{TRACE_ID_FIELD, RUN_ID_FIELD}`（Task 1）。
- Produces: `pub fn app::LoggingFactory::color_enabled(allowed: bool, terminal: bool, no_color: Option<&std::ffi::OsStr>) -> bool`；`LoggingFactory::new(filter, color)` 签名不变；测试辅助 `fn line<'a>(output: &'a str, needle: &str) -> &'a str`（本测试文件内）。

- [ ] **Step 1: 写失败测试**（`crates/app/tests/logging.rs`；删除 `logging_factory_flattens_span_context_to_one_trace_id`、`rig_telemetry_logs_final_metadata_without_content`、`rig_telemetry_waits_for_close_and_preserves_unknown_usage`、`rig_telemetry_respects_existing_log_filters`）

```rust
use protocol::{RUN_ID_FIELD, TRACE_ID_FIELD};

/// Returns the first captured line that contains `needle`.
fn line<'a>(output: &'a str, needle: &str) -> &'a str {
    output.lines().find(|line| line.contains(needle)).unwrap_or_else(|| panic!("missing {needle}"))
}

#[test]
fn logging_factory_prefixes_lines_with_nearest_trace_and_run() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false).expect("valid filter").build(logs.clone());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("outside any run");
        tracing::error_span!("run", { TRACE_ID_FIELD } = "trace-a", { RUN_ID_FIELD } = "run-a").in_scope(|| {
            tracing::info!("in the outer run");
            tracing::error_span!("run", { RUN_ID_FIELD } = "run-b")
                .in_scope(|| tracing::info!("in a nested run"));
            tracing::error_span!("run", { TRACE_ID_FIELD } = "trace-c", { RUN_ID_FIELD } = "run-c")
                .in_scope(|| tracing::info!("in a child run"));
        });
    });
    let output = logs.content();
    assert!(line(&output, "outside any run").contains(" [-] "));
    assert!(line(&output, "in the outer run").contains(" [trace-a run-a] "));
    assert!(line(&output, "in a nested run").contains(" [trace-a run-b] "));
    assert!(line(&output, "in a child run").contains(" [trace-c run-c] "));
    assert!(!output.contains("trace_id=") && !output.contains("run{"));
}

#[test]
fn logging_factory_writes_no_span_lifecycle_lines() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("trace", false).expect("valid filter").build(logs.clone());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!(target: "rig::completions", "chat").in_scope(|| {});
        tracing::error_span!("run", { RUN_ID_FIELD } = "run-x").in_scope(|| {});
    });
    assert_eq!(logs.content(), "");
}

#[test]
fn color_needs_permission_a_terminal_and_no_no_color() {
    use std::ffi::OsStr;
    let cases = [
        (true, true, None, true),
        (true, true, Some(OsStr::new("")), true),
        (true, true, Some(OsStr::new("1")), false),
        (true, false, None, false),
        (false, true, None, false),
    ];
    for (allowed, terminal, no_color, expected) in cases {
        assert_eq!(LoggingFactory::color_enabled(allowed, terminal, no_color), expected);
    }
}
```

- [ ] **Step 2: 确认失败** —— `cargo test -p app --test logging`，预期前缀与颜色测试失败、生命周期测试因 Rig span 输出而失败。

- [ ] **Step 3: 实现**
  - `trace.rs`：`TraceContextLayer` 改名为 `CorrelationLayer`；一个 visitor（`record_str` 与 `record_debug`，后者用 `{:?}` 取 Display 值，与现有做法相同）同时识别两个常量，分别插入 `TraceContext(Arc<str>)` 与 `RunContext(Arc<str>)`。新增 `pub(super) struct SpanCorrelation { trace: Option<TraceContext>, run: Option<RunContext> }`，`fn current<S, N>(context: &FmtContext<'_, S, N>) -> Self` 沿 `context.event_scope()` 由内向外遍历一次，各取第一个；`Display` 两项都缺时输出 `[-]`，否则输出 `[{trace} {run}]`，缺的一项写 `-`。删除本文件的 `TRACE_ID_FIELD` 常量。
  - `format.rs`：删除 `RigSpan` 引用、`ModelLifeCycle` 与 `event.metadata().is_span()` 分支；前缀改用 `SpanCorrelation::current(context)`；`EventSource` 的 `Display` 对绝对路径输出 `self.target` 且不写行号，相对路径不变。保留 `AcpFields`。
  - `logging.rs`：删除 `mod telemetry;`、`ModelTracingLayer` 与 `.with_span_events(...)`，改正 `.without_time()` 的注释；`with(CorrelationLayer)`；新增 `color_enabled`（`allowed && terminal && no_color.is_none_or(OsStr::is_empty)`）；`from_config` 用 `Self::color_enabled(config.color, std::io::stderr().is_terminal(), std::env::var_os("NO_COLOR").as_deref())`。
  - 删除 `telemetry.rs`。

- [ ] **Step 4: 确认通过** —— `cargo test -p app --test logging` 全部 PASS（含保留的过滤、ANSI、时间戳与源位置、ACP handler 链、环境变量优先等测试）。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 3: Kernel 的 Run span 与 trace 来源（机制）

本任务只引入机制，现有对外接口保持可用：`PromptRequest.trace_id`、`SubmitRequest.trace_id`、`run_traced` 暂时映射到 `TraceOrigin`，由 Task 4 清理。

**Files:**
- Create: `crates/kernel/src/runtime/run/trace.rs`
- Modify: `crates/kernel/src/runtime/run.rs`（`mod trace;`、`RunInvocation`、`run_with_source`、`run_until_settled`、起始行 `:126`）、`crates/kernel/src/runtime.rs`（`Kernel::run`、`run_traced`）、`crates/kernel/src/lib.rs`（导出）
- Modify: `crates/kernel/src/runtime/command/user_bash.rs:106`、`crates/kernel/src/runtime/run/submit.rs:62,95`、`crates/kernel/src/runtime/run/admission.rs:276`、`crates/kernel/src/runtime/extension/host.rs:327`
- Test: Create `crates/kernel/tests/support/spans.rs`、`crates/kernel/tests/tracing.rs`；Modify `crates/app/tests/logging.rs`

**Interfaces:**
- Consumes: `protocol::{TRACE_ID_FIELD, RUN_ID_FIELD}`（Task 1）；app 的前缀格式（Task 2）。
- Produces:
  - `pub enum kernel::TraceOrigin { Inherit(TraceId), LinkFrom(TraceId), #[default] New }`（`Debug, Clone, Default`）；`pub(crate) fn trace_id(&self, ids: &dyn IdGenerator) -> Result<TraceId, KernelError>`：`Inherit` 返回原值，其余生成新 id。
  - `pub struct kernel::RunSpan(tracing::Span)`：`pub fn top_level(trace_id: &TraceId, run_id: &RunId) -> Self`、`pub fn nested(run_id: &RunId) -> Self`、`pub async fn instrument<F: Future>(self, future: F) -> F::Output`。两个构造函数是全仓库唯一的 `tracing::error_span!("run", …)`，字段写成 `{ protocol::TRACE_ID_FIELD } = %trace_id`。
  - `RunInvocation.trace: TraceOrigin`（`#[builder(default)]`，替换 `trace_id`）；`fn origin_phrase(&self) -> String`：`New` → `format!("from {:?}", input_source)`，`Inherit` → `"as foreground subagent"`，`LinkFrom(t)` → `format!("as background subagent linked to trace {t}")`。
  - 测试辅助 `pub fn run_fields(line: &str) -> (Option<String>, Option<String>)`（`crates/kernel/tests/support/spans.rs`）：从左到右扫描行内所有 `run{…}` 段，返回最后出现的 `trace_id=` 值与最后出现的 `run_id=` 值（tracing-subscriber 默认格式如 `run{trace_id=trace-a run_id=run-1}:run{run_id=run-2}: …`）。由需要的测试二进制用 `#[path = "support/spans.rs"] mod spans;` 引入。

- [ ] **Step 1: 写失败测试** `crates/kernel/tests/tracing.rs`（`mod support;` 复用全局日志捕获；`#[path = "support/spans.rs"] mod spans;`）

```rust
#[tokio::test]
async fn kernel_run_lines_share_one_top_level_run_span() {
    // Kernel::run with a scripted one-answer model (pattern: crates/kernel/tests/turn.rs).
    // For the new session, the "started Kernel Run" and "started Turn" lines yield the same
    // run_fields(..) == (Some(trace), Some(run)) with trace starting "trace-" and run starting "run-";
    // the start line contains "from Rpc" (Kernel::run keeps InputSource::Rpc).
}

#[tokio::test]
async fn user_bash_runs_in_a_run_span() {
    // Kernel::run with input "!true": the "started user bash" line has run_fields(..).0 starting "trace-".
}

#[tokio::test]
async fn direct_slash_command_runs_in_a_run_span() {
    // Kernel::run with a builtin direct slash command (pattern: crates/kernel/tests/slash_commands.rs):
    // the "started Slash Command" line has run_fields(..).0 starting "trace-".
}

#[tokio::test]
async fn extension_message_starts_its_own_trace() {
    // An extension calls send_user_message while the session is idle (fixture pattern:
    // RestoredDynamicToolModule in crates/kernel/tests/extensions_host.rs, around line 620).
    // The triggered Run's "started Kernel Run" line contains "from Extension", and its trace
    // differs from the trace of the Run that hosted the extension call.
}

#[tokio::test]
async fn queued_follow_up_runs_under_the_running_trace() {
    // While a first Run is blocked in its model call, submit a FollowUp input to the same session
    // (pattern: crates/kernel/tests/protocol_alignment/queue.rs). The "started Turn" line of the
    // Turn that processes it has the same run_fields as the first Run, and the session has exactly
    // one "started Kernel Run" line.
}
```

追加到 `crates/app/tests/logging.rs`：

```rust
#[tokio::test]
async fn run_span_prefix_survives_a_warn_filter() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("warn", false).expect("valid filter").build(logs.clone());
    let _guard = tracing::subscriber::set_default(subscriber);
    let trace = protocol::TraceId::try_from("trace-w").expect("trace");
    let run = protocol::RunId::try_from("run-w").expect("run");
    kernel::RunSpan::top_level(&trace, &run)
        .instrument(async { tracing::warn!("slow tool") })
        .await;
    assert!(line(&logs.content(), "slow tool").contains(" [trace-w run-w] "));
}
```

`crates/app/tests/logging.rs` 已有 `#[tokio::test]` 测试，可直接使用。

- [ ] **Step 2: 确认失败** —— `cargo test -p kernel --test tracing` 与 `cargo test -p app --test logging run_span_prefix_survives_a_warn_filter`，预期编译失败（`RunSpan` 不存在）。

- [ ] **Step 3: 实现**
  - `trace.rs`：`TraceOrigin` 与 `RunSpan`；`lib.rs` 与 `runtime` 按 `SubmitRequest` 的导出路径导出两者。
  - `run_with_source`：先 `let trace_id = invocation.trace.trace_id(&*self.id_generator)?;` 并生成 `run_id`，再 `RunSpan::top_level(&trace_id, &run_id).instrument(Box::pin(self.run_until_settled(request, sink, &invocation, run_id, trace_id)))`；`run_until_settled` 删除 `run.rs:82` 处的生成，改用参数，`RunScope` 的 `trace_id` 取自参数。
  - 用户命令：`try_execute_user_bash_request` 增加参数 `run_id: &RunId`，删除 `user_bash.rs:106` 的生成。
  - 现有接口映射（Task 4 删除）：`admission.rs:276` 用 `.trace(TraceOrigin::Inherit(request.trace_id))`；`submit.rs:62` 用 `request.trace_id.map_or(TraceOrigin::New, TraceOrigin::Inherit)`；`run_traced` 用 `TraceOrigin::Inherit(trace_id)`；`Kernel::run` 删除自生成的 trace，走默认 `New`；`host.rs:327` 删除 trace 生成与 `.trace_id(...)`，走默认 `New`。
  - `run.rs:126` 起始行改为 `tracing::info!("started Kernel Run for session {} {}", request.session_id, invocation.origin_phrase())`。

- [ ] **Step 4: 确认通过** —— `cargo test -p kernel --test tracing`、`cargo test -p app --test logging`、`cargo test -p kernel`、`cargo test -p acp` 全部 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 4: 清理 trace 相关的对外接口

**Files:**
- Modify: `crates/kernel/src/runtime/run/admission.rs`（删除 `PromptRequest.trace_id` 与映射）
- Modify: `crates/kernel/src/runtime/run/submit.rs`（`trace_id: Option<TraceId>` → `trace: TraceOrigin`）
- Modify: `crates/kernel/src/runtime.rs`（`run_traced` → `run_with_insertion`）
- Modify: `crates/kernel/src/runtime/subagent/foreground.rs:139`
- Modify: `crates/acp/src/server.rs:1565`（删除 `.trace_id(trace_id)`）
- Modify（迁移）: `crates/kernel/tests/turn.rs:1536`、`mcp_host.rs:864,989`、`protocol_alignment/insertion.rs:54`、`protocol_alignment/title.rs:50`、`protocol_alignment/queue.rs:32,70,146,214,865,957`

**Interfaces:**
- Consumes: `TraceOrigin`（Task 3）。
- Produces: `pub struct kernel::SubmitRequest { …, #[builder(default)] pub trace: TraceOrigin }`；`pub async fn Kernel::run_with_insertion(&self, request: RunRequest, sink: Arc<dyn EventSink>, insertion: Option<Arc<InsertionNotice>>) -> Result<RunResult, KernelError>`；`kernel::PromptRequest` 不再有 `trace_id`。

- [ ] **Step 1: 改接口并让编译器列出调用点** —— 按 Interfaces 修改三个类型/方法，`cargo build --workspace --tests`，预期在上述文件报错。
- [ ] **Step 2: 修正调用点**
  - `foreground.rs:139`：`.trace(TraceOrigin::Inherit(context.trace_id.clone()))`。
  - `submit.rs`：把 `request.trace` 直接传给 `RunInvocation`。
  - `admission.rs`：删除映射，`RunInvocation` 走默认 `New`。
  - `acp/src/server.rs`：删除 `.trace_id(trace_id)` 与 `let trace_id = operation.trace_id().clone();`。`AcpOperationTrace::trace_id()` 因此暂时未被使用，产生的未使用警告由 Task 9 删除该方法时消除。
  - 测试迁移：`run_traced(request, sink, <trace>, insertion)` → `run_with_insertion(request, sink, insertion)`，删除因此未使用的 `TraceId` 导入。
- [ ] **Step 3: 确认通过** —— `cargo test -p kernel`、`cargo test -p acp` 全部 PASS；`cargo test -p kernel --test tracing` 仍 PASS。
- [ ] **Step 4: 检查点** —— `cargo build --workspace --tests`。

---

### Task 5: 压缩与 Tree 摘要的 span

**Files:**
- Modify: `crates/kernel/src/runtime/compaction.rs`（`compact_session_with_reason` 约 149 行、`perform_compaction` 约 205–230 行）
- Modify: `crates/kernel/src/runtime/session/tree.rs:205`
- Test: `crates/kernel/tests/compaction.rs`（新增 `#[path = "support/spans.rs"] mod spans;`；在测试的 `ScriptedModel::stream` 开头加 `tracing::info!("scripted model stream started")`，使每次模型调用都在当前 span 内留下一行）

**Interfaces:**
- Consumes: `RunSpan::{top_level, nested, instrument}`、`TraceOrigin::New.trace_id(..)`（Task 3）；`spans::run_fields`（Task 3）。

- [ ] **Step 1: 写失败测试**（复用 `CompactionFixture` 与现有 `CapturedLogs`）

```rust
#[tokio::test]
async fn standalone_compaction_opens_a_top_level_run_span() {
    // setup as compact_streams_summary_and_persists_pi_v4_payload, then compact_session.
    // The "scripted model stream started" line of the summary call contains exactly one "run{"
    // segment, and run_fields(..).0 starts with "trace-".
}

#[tokio::test]
async fn threshold_compaction_nests_under_the_loop_trace() {
    // setup as threshold_usage_compacts_without_repeating_assistant_call.
    // The compaction lifecycle line ("Compaction Turn") contains two "run{" segments; its
    // run_fields(..).0 equals the trace on that session's "started Kernel Run" line and its
    // run_fields(..).1 differs from that line's run.
}

#[tokio::test]
async fn manual_compact_command_opens_no_extra_span() {
    // "/compact" through run_with_insertion: the compaction lifecycle line contains exactly one "run{" segment.
}

#[tokio::test]
async fn branch_summary_opens_a_top_level_run_span() {
    // navigate_session_with_summary(.., true) with the fixture model answering the summary
    // (navigation call pattern: crates/kernel/tests/extensions_host.rs:1161). The
    // "scripted model stream started" line of the summary call contains exactly one "run{"
    // segment and run_fields(..).0 starts with "trace-".
}
```

- [ ] **Step 2: 确认失败** —— `cargo test -p kernel --test compaction`，新增 4 个测试失败。

- [ ] **Step 3: 实现**
  - `compact_session_with_reason`：自己创建 `CompactionIdentity::new(run_id, turn_id, now)`，用 `RunSpan::top_level(&TraceOrigin::New.trace_id(&*self.id_generator)?, &identity.run_id)` instrument `perform_compaction(… .identity(Some(identity)) …)`。
  - `perform_compaction`：`identity` 为 `None` 时生成身份，并把其后的工作放进 `RunSpan::nested(&identity.run_id).instrument(...)`；调用方提供身份时直接执行。
  - `tree.rs:205`：用 `RunSpan::top_level(&TraceOrigin::New.trace_id(..)?, &run_id)` instrument `generate_compaction_summary(...)`。

- [ ] **Step 4: 确认通过** —— `cargo test -p kernel --test compaction` 全部 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 6: 子代理的 trace

**Files:**
- Modify: `crates/kernel/src/runtime/subagent/background.rs`（`Reporter` 字段、`:135` 的提交、`:130` 附近的投递日志、`:497` 的创建）
- Modify: `crates/kernel/tests/subagent/main.rs`（`#[path = "../support/mod.rs"] mod support;`、`#[path = "../support/spans.rs"] mod spans;`、`mod tracing;`）
- Test: Create `crates/kernel/tests/subagent/tracing.rs`

**Interfaces:**
- Consumes: `TraceOrigin::{Inherit, LinkFrom}`、`SubmitRequest.trace`（Task 3、4）；`spans::run_fields`。
- Produces: `Reporter` 新字段 `parent_trace: TraceId`（typed-builder 必填）。

- [ ] **Step 1: 写失败测试**（复用 `fixture::TestKernel` 与 `Route`；调用写法参照同目录 `foreground.rs`、`background.rs`）

```rust
#[tokio::test]
async fn foreground_child_shares_the_parent_trace() {
    // parent calls `subagent`: the child session's "started Kernel Run" line has run_fields equal to
    // (parent trace, child run != parent run) and contains "as foreground subagent".
}

#[tokio::test]
async fn concurrent_foreground_children_keep_distinct_runs() {
    // one Tool batch with two `subagent` calls: both child start lines carry the parent trace and
    // two distinct runs.
}

#[tokio::test]
async fn background_child_starts_a_linked_trace() {
    // parent calls `background_subagent` while the child is idle: the child's start line has a trace
    // different from the parent's and contains "as background subagent linked to trace {parent trace}".
}

#[tokio::test]
async fn background_message_to_a_busy_child_is_linked_by_the_reporter() {
    // send to an already running background child: no new "started Kernel Run" line for the child, and
    // the line "started subagent delivery from trace {parent trace} to session {child}" exists.
}
```

- [ ] **Step 2: 确认失败** —— `cargo test -p kernel --test subagent tracing`。

- [ ] **Step 3: 实现**
  - 创建 `Reporter` 处（`background.rs:497`）传入 `parent_trace: context.trace_id.clone()`。
  - 向子会话提交（`:135`）使用 `.trace(TraceOrigin::LinkFrom(self.parent_trace.clone()))`；回报交回发起方（`:217`）保持默认 `New`。
  - 投递日志 `"started subagent report delivery {} -> {}"` 改为 `"started subagent delivery from trace {} to session {}"`（父 trace、子会话）。

- [ ] **Step 4: 确认通过** —— `cargo test -p kernel --test subagent` 全部 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 7: Run 内日志正文去重（spec 5.2、5.3）

**Files:**（清单来自对正文含 `Run {}` 或 `trace {}` 的日志的全量检索；`run/queue/operations.rs:160` 发生在目标 Run 之外，不改）
- Modify: `crates/kernel/src/runtime/run.rs:346`
- Modify: `crates/kernel/src/runtime/run/turn.rs:296,418,765,1031,1278,1326,1360,1379`
- Modify: `crates/kernel/src/runtime/run/lifecycle.rs:522`
- Modify: `crates/kernel/src/runtime/tools/batch.rs:87,103,169,194,220,258,306,381,444`
- Modify: `crates/kernel/src/runtime/command/execution.rs:160,182,233,292,321,424`，并删除 `SlashCommandExecution.trace_id` 与 builder 调用
- Modify: `crates/kernel/src/runtime/command/user_bash.rs:142,169,182,285`
- Modify: `crates/kernel/src/runtime/compaction.rs:237,266,536,545,581`
- Modify: `crates/acp/src/projection.rs:1031`
- Test: `crates/kernel/tests/tracing.rs`

**Interfaces:** 无新增。`RunScope`、`TurnScope`、`ToolBatch` 的 `trace_id` 字段保留（数据链）。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn run_messages_do_not_repeat_correlation_ids() {
    // Kernel::run with a scripted model that calls one Tool then answers (pattern: crates/kernel/tests/turn.rs).
    // For every line whose run_fields(..).1 is this Run's run:
    //   line.matches("trace-").count() == 1 and line.matches(run).count() == 1 (span fields only);
    // the session id appears on exactly one of these lines, the "started Kernel Run" line.
}
```

- [ ] **Step 2: 确认失败** —— `cargo test -p kernel --test tracing run_messages_do_not_repeat_correlation_ids`。

- [ ] **Step 3: 改写正文**（只删 trace、本 Run id、会话、Turn 尾巴，其余措辞不变）：
  - `turn.rs`：`"started Turn {}"`、`"cancelled prompt after message hooks before insertion"`、`"Assistant message {} lost its role before settlement"`、`"settled Turn {} in {} ms with outcome {:?}"`、`"completed Kernel Run because an extension handled the input"`、`"settled Kernel Run with outcome {:?} after {} Turns"`、`"cancelled Kernel Run before insertion: {}"`、`"failed Kernel Run: {}"`。
  - `run.rs:346`：`"persisted {:?} input after cancellation before insertion"`；`lifecycle.rs:522`：`"failed to release submission waiters: {}"`（在 `ActiveRunLease` 的 `Drop` 中，`Instrumented` future 在析构时进入 span，仍在 Run 内）。
  - `batch.rs`：`"started Tool batch of {} calls"`、`"started Tool call {} ({})"`、`"blocked Tool call {} ({})"`、`"blocked Tool call {} ({}) in plan mode"`、`"rejected invalid Tool call {} ({}): {}"`、`"failed Tool call {} ({}): {}"`、`"cancelled Tool batch"`、`"completed Tool call {} ({}) in {} ms with error {}"`、`"completed Tool batch with {} results; cancelled {}, terminate {}"`。
  - `execution.rs`：起始行 `"started Slash Command /{} for session {}"`（这类 Run 没有 "started Kernel Run" 行）；其余 `"rejected busy Slash Command /{}"`、`"failed to persist Slash Command /{} invocation: {}"`、`"failed Extension Slash Command /{}: {}"`、`"failed Slash Command /{}: {}"`、`"settled Slash Command /{} with status {:?}"`。
  - `user_bash.rs`：起始行 `"started user bash for session {} with message {}"`；其余 `"user bash failed after acceptance: {}"`、`"finished user bash with outcome {:?}"`、`"failed to report user bash output: {}"`。
  - `compaction.rs`：`"{} Compaction Turn {} for session {} with reason {:?} in {} ms; will retry {}"`、`"cancelled Compaction Turn {} for session {} with reason {:?} by extension"`、`"failed Compaction Turn {} for session {} with reason {:?}: {}"`、`"failed to settle compaction after execution failure {}: {}"`、`"failed to settle Compaction Turn {} for session {} with reason {:?}: {}"`（压缩行是压缩 Run 的起始与结束行，保留会话）。
  - `projection.rs:1031`：`"recorded ACP Operation {:?} for Session {}, sequence {}"`。

- [ ] **Step 4: 确认通过** —— `cargo test -p kernel`、`cargo test -p acp` 全部 PASS（现有断言旧文本的测试按新文本更新，只改字符串）。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 8: 每次模型调用一对日志

**Files:**
- Modify: `crates/kernel/src/provider.rs`（约 237–310 行）
- Modify: `crates/kernel/Cargo.toml`（若 `tracing-futures` 不再使用则删除该行）
- Test: `crates/kernel/tests/provider_runtime.rs`（复用 `support/provider_server.rs` 与 `CapturedLogs`；新增 `#[path = "support/spans.rs"] mod spans;`）

**Interfaces:**
- Consumes: `Display for ModelUsage`（Task 1）；`spans::run_fields`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn completed_provider_request_reports_response_and_usage() {
    // provider server returns response id "resp_1" and usage input 12 / output 3, nothing else:
    // the "completed Provider request" line contains " ms; input 12, output 3; response id resp_1"
    // and does not contain "cache read".
}

#[tokio::test]
async fn failed_provider_request_reports_provider_request_id() {
    // provider server fails with HTTP 500 and the request-id header Rig reads for the fixture's
    // protocol (OpenAI-compatible: `x-request-id`; Anthropic: `request-id`; rig-core wire.rs:48)
    // set to "req_fixture": the WARN failure line ends with "; provider request req_fixture".
}

#[tokio::test]
async fn failed_and_retried_provider_requests_share_the_run_trace() {
    // first attempt fails transiently, retry succeeds: the WARN failure line and the
    // "completed Provider request" line have equal run_fields(..).
}
```

- [ ] **Step 2: 确认失败** —— `cargo test -p kernel --test provider_runtime`。

- [ ] **Step 3: 实现**
  - 完成行：`"completed Provider request {} in {} ms; {}{}"`，参数依次为 identity、耗时、`ModelUsage::from(response.usage)`、响应身份。响应身份是 `provider.rs` 内的私有 Display 新类型，包装 `response.response_id()` 与 `response.model()`：有 id 时输出 `"; response id {id}"`，有 model 时再输出 `", model {model}"`（无 id 时为 `"; response model {model}"`），都没有时输出空。
  - 失败行：准备、流、收尾三处失败位置在 `ModelFailure::from(error)` 之前读取 `error.provider_request_id()`，用同文件另一个私有 Display 新类型在原文本后追加 `"; provider request {id}"` 或空。
  - 删除 `tracing::info_span!("provider_request")` 及对 stream 的 instrument，返回 `Ok(Box::pin(stream))`（stream 由 Turn 在 Run span 内轮询）。

- [ ] **Step 4: 确认通过** —— `cargo test -p kernel --test provider_runtime` 全部 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 9: ACP 请求日志与回复任务的 span

**Files:**
- Modify: `crates/acp/src/trace.rs`（类型改名与分类）、`crates/acp/src/server.rs`（字段 `traces` → `request_logs`、全部调用点、prompt 处理 1528–1650、`AcpServerFactory::new`）、`crates/acp/src/transport.rs`（`http_router`）
- Modify: `crates/acp/src/permission.rs:91`、`crates/acp/src/elicitation.rs:249,271`
- Modify: `crates/app/src/lib.rs`（删除 `Application.id_generator`：`:79`、`:215`；`:182` 处的生成器仍传给 Kernel）、`crates/app/src/main.rs:65`、`crates/app/src/web.rs:107`
- Modify（构造调用迁移）: `crates/acp/tests/{tracing,elicitation,disconnect,extension,login,plan_mode,v2_alignment}.rs`
- Test: `crates/acp/tests/tracing.rs`、`crates/acp/tests/plan_mode.rs`、`crates/acp/tests/elicitation.rs`

**Interfaces:**
- Consumes: `AcpExtensionMethod::{parse, is_query}`（Task 1）。
- Produces: `pub(crate) enum AcpRequestKind { Query, Change }` 与 `impl From<&str> for AcpRequestKind`（`"session/list"` 或可解析且 `is_query()` 的扩展方法为 `Query`）；`AcpRequestLogs::new(transport: AcpTransportKind) -> Self`；`request<T, P>(&self, responder: &Responder<T>, parameters: &P) -> AcpRequestLog`、`notification<P>(&self, method: &str, parameters: &P) -> AcpRequestLog`（都不可失败）；`AcpRequestLog::{start, run, settle, fail}` 语义不变，不再有 span、`trace_id()`、`instrument()`；`AcpServerFactory::new(kernel: Arc<Kernel>, max_batch_size: NonZeroUsize)`；`AcpServerFactory::http_router(kernel, max_batch_size, options)`。

- [ ] **Step 1: 写失败测试**
  - 改写 `prompt_trace_reaches_the_provider_background_task` 为 `prompt_run_trace_reaches_the_provider`：`AcpServerFactory::new(kernel, batch)`（删除 `TraceIds`）；提供方行包含 `run{trace_id=trace-`；"started Kernel Run" 与 "started Turn" 行的 `trace_id=` 值与提供方行相同；"completed ACP request session/prompt:" 行与所有 "parameters for ACP …" 行都不含 `trace_id=`；`session/prompt` 完成行在 "reported prompt insertion" 之后、"settled Kernel Run" 之前（R08）。脱敏断言（`sensitive prompt body` 到 secret 循环）原样保留。
  - 新增：

```rust
#[tokio::test]
async fn read_only_requests_are_silent_at_info() {
    // filter "info" (no acp::trace=debug); initialize, session/new, session/list, _clawcode/session/tree.
    // output has no line containing "ACP request session/list" or "ACP request _clawcode/session/tree";
    // exactly one line contains "completed ACP request session/new:" and none "started ACP request session/new:".
}
```

  - `plan_mode.rs` 新增 `plan_approval_answer_is_logged_inside_the_run`：复用现有批准流程，安装与 `tracing.rs` 相同的日志捕获；"received plan approval" 行包含 `run{trace_id=trace-`（R13）。
  - `elicitation.rs` 新增 `elicitation_answer_is_logged_inside_the_run`：复用现有经 `session/prompt` 触发 elicitation 并回复的流程（`prompt_new_session`、`next_asked`），同样安装日志捕获；"received elicitation answer" 行包含 `run{trace_id=trace-`（R13）。

- [ ] **Step 2: 确认失败** —— `cargo test -p acp --test tracing --test plan_mode --test elicitation`。

- [ ] **Step 3: 实现**
  - `trace.rs`：`AcpTraceFactory` → `AcpRequestLogs`（删除 `id_generator`）；`AcpOperationTrace` → `AcpRequestLog`（删除 `trace_id`、`span`、`trace_id()`、`instrument()`）；`AcpOperationIdentity` 的 `quiet_success: bool` → `kind: AcpRequestKind`。开始行一律 DEBUG；完成行 `Query` 为 DEBUG、`Change` 为 INFO，文本不变（`"completed {description} in {ms} ms"`）；失败 WARN。模块路径不变。
  - `server.rs`：去掉 `context.traces.request(...)` 后的 `?`；prompt 处理删除 `trace_id`、`operation.instrument(...)` 与 `task_operation.settle(...)`，后台任务直接调用 `kernel.prompt(...)`；`connection.spawn` 中计算响应的 future 用 `operation.settle(...)` 包住，使完成行在响应发出时写出。
  - `permission.rs:91`、`elicitation.rs:249,271`：`connection.spawn(future.instrument(tracing::Span::current()))`（`use tracing::Instrument;`）。
  - 删除 `AcpServerFactory`、`http_router` 的 `id_generator` 参数与字段，删除 `app::Application.id_generator`，更新 `main.rs`、`web.rs` 与所有测试构造调用（`Application::http_router(options)` 的签名不变）。

- [ ] **Step 4: 确认通过** —— `cargo test -p acp`、`cargo test -p app` 全部 PASS。

- [ ] **Step 5: 检查点** —— `cargo build --workspace --tests`。

---

### Task 10: 文档与整体验收

**Files:**
- Modify: `README.md`（"Logging" 约 435–453 行）、`README_zh.md`（对应小节）

- [ ] **Step 1: 改写文档**（spec 6.5）：trace = 一个顶层 Run；前缀 `[trace run]` 与 `[-]`；模型调用完成行的内容（耗时、用量、响应 id）与失败行的提供方请求 id；颜色规则（`color`、终端、`NO_COLOR`）；`[logging] filter`/`RUST_LOG` 的控制；删除 Rig 摘要段落；保留 ACP DEBUG 参数脱敏说明并注明 `acp::trace=debug` 也会输出只读查询的 DEBUG 日志；示例配置 `filter` 不变。

- [ ] **Step 2: 全量验证**
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --tests --examples -- -Dwarnings`
  - `cargo test --workspace`
  - `pre-commit run --files $(git diff --name-only; git ls-files --others --exclude-standard)`
  预期全部通过。

- [ ] **Step 3: 手动验收**（spec 第 1 节成功标准与 R04）：`cargo build -p app` 后以 `target/debug/clawcode serve` 启动、stderr 重定向到文件，跑一次 2 次工具调用的真实运行，确认：只读 ACP 请求不在 INFO；Run 内每行同一个 `[trace run]`、正文无重复 id；每次模型调用 2 行；依赖库事件（如 Rig 的 `skipping unmodeled wire payload`）显示 target；文件中无 ANSI 转义码。把改动前后的行数与示例行记录在交付说明里。

- [ ] **Step 4: 检查点** —— 所有改动留在工作区，等待用户审阅与提交指示。

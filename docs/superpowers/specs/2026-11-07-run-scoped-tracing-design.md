# Run 级 Tracing 设计规格

日期：2026-11-07。状态：五段设计已在对话中逐段确认，已按审阅意见修订并补充代码组织，待再次审阅。

> **实施后变更（2026-11-08）：** 日志改用 tracing-subscriber 自带的默认格式与 ANSI 着色，删除自定义的事件格式、`[trace run]` 前缀渲染（`CorrelationLayer`、`SpanCorrelation`）以及自定义的颜色判断函数。Run 关联改由默认格式渲染的 span 字段 `run{trace_id=… run_id=…}:` 表示，以最内层的 `run{…}` 为准（循环内压缩追加 `:run{run_id=…}`，前台子代理追加自己的 `run{trace_id=… run_id=…}`）。`[logging] color` 只在 stderr 是终端时允许颜色（`from_config` 的 `is_terminal` 判断），`NO_COLOR` 由 tracing 处理。项目代码不再显示 `路径:行号`，每行显示事件的 target（模块路径）。ACP SDK 告警中的 `handler` 字段不再裁剪，按默认格式完整输出；默认过滤器保持 `info`。下文第 1 节成功标准中的前缀、第 5 节、§6.4、§7.2/7.3 中的 `CorrelationLayer`/`SpanCorrelation`/`EventSource`，以及 R01/R03/R04 的格式断言均以此为准。

## 1. 目标与已确认决策

目标：一个 trace 对应一次完整的 agent loop（一个顶层 Kernel Run），而不是一条 ACP 消息。按一个 trace id 过滤日志，得到的就是这次 Run 的全部内容：每个 Turn、每次模型调用、每个工具调用、循环内的自动压缩、同一循环里处理的排队消息，以及它启动的前台子代理。不属于 Run 的 ACP 消息不再生成 trace。

成功标准：用同一条真实运行（2 次工具调用、4 次模型调用）对比改动前后的 stderr 日志——只读 ACP 请求不再出现在 INFO；Run 内每一行带同一个 trace 的 `[trace run]` 前缀，正文不再重复这些 id；每次模型调用只有一对开始/完成日志；输出重定向到文件时没有 ANSI 转义码。

| 决策 | 结论 |
|---|---|
| trace 范围 | 一个顶层 Kernel Run 一个 trace；前台子代理与循环内的自动压缩沿用所在 Run 的 trace；后台子代理启动新 Run 时使用新 trace，并在日志中写明父 trace |
| 非 Run 的 ACP 消息 | 不生成 trace，前缀为 `[-]`；只读查询只在 DEBUG 记录；有修改的操作保留一行 INFO |
| 日志前缀 | `[trace run]`：trace 取最近的带 `trace_id` 的 span，run 取最近的带 `run_id` 的 span |
| 导出 | 只用本地日志，不引入新依赖，保留 `trace-…` id 格式 |
| 实现方式 | Kernel 拥有 trace：顶层 Run 打开 `run{trace_id, run_id}`；在已有 Run 内生成的 Run id 只打开 `run{run_id}`；ACP 不再生成或传递 trace id |
| 附带修复 | 每次模型调用一对日志；去掉 `FmtSpan` 生命周期事件；颜色自动判断并遵守 `NO_COLOR`；依赖库事件显示 target |
| 代码组织 | 每项职责只有一个归属类型；方法挂在拥有数据的类型上；删除失去用途的代码（见第 7 节） |

## 2. 现状

- ACP 每收到一个请求或通知，`AcpTraceFactory`（`crates/acp/src/trace.rs`）就生成一个新的 `TraceId` 并打开 `acp_operation{trace_id}` span，再分别写一行 `started …` 和 `completed …`。只有 `_clawcode/session/runtime` 的成功日志是 DEBUG，由布尔字段 `quiet_success` 控制。
- `session/prompt` 把自己的 trace 传给 `kernel.prompt`，并用 `task_operation.settle` 包住后台任务（`crates/acp/src/server.rs:1558`）。因此"ACP 请求完成"那行要等整个 Run 结束才输出，而 JSON-RPC 响应其实在消息插入时就已发出。
- prompt 先经过准入（`crates/kernel/src/runtime/run/admission.rs:151`）：扩展输入钩子、登录检查，然后尝试入队。会话忙时 prompt 被排进运行中循环的队列，Kernel 记录 "admitted prompt N for session S into queue Q"（`admission.rs:253`），不会进入 `run_with_source`；这条 prompt 的 JSON-RPC 响应等到运行中的循环把它插入历史时才发出。会话空闲时才进入 `run_with_source`。
- Kernel 中 `trace_id` 的去向：
  - 作为数据沿 `RunInvocation` → `RunScope` → `TurnScope`（`crates/kernel/src/runtime/run/turn.rs:900`）→ `ToolBatch`（`crates/kernel/src/runtime/tools/batch.rs:245`）→ `ToolExecutionContext.trace_id`（`crates/tools/src/contract.rs:35`）传递，供子代理继承 trace、MCP 拼请求 id；
  - 同时被各层拼进日志正文，例如 `started Tool call {} ({}) for session {}, Run {}, Turn {}, trace {}`；
  - `SlashCommandExecution.trace_id` 只用于日志正文（`crates/kernel/src/runtime/command/execution.rs:162–326`）。
- 不是由 ACP prompt 发起的 Run 会自己生成 trace id，但不打开 span：扩展触发的 Run（`crates/kernel/src/runtime/extension/host.rs:327`）、`Kernel::run`、`kernel.submit` 未指定 trace 时（`crates/kernel/src/runtime/run/submit.rs:62`）。这些 Run 的日志前缀是 `[-]`，trace 只出现在正文里。`SubmitRequest` 在 Kernel 之外也被使用（`crates/acp/src/projection.rs`）。
- 子代理：前台子代理沿用父 trace（`crates/kernel/src/runtime/subagent/foreground.rs:139`）；子会话空闲时，`kernel.submit` 在调用方的 future 里直接执行子 Run（`submit.rs:84`），因此子 Run 天然处于父 Run 的 span 上下文中。后台子代理的 Reporter 在独立任务中运行，向子会话提交任务（`crates/kernel/src/runtime/subagent/background.rs:135`）和把回报交回发起方（`background.rs:217`）时都不带 trace；给已在运行的后台子代理发消息（`background.rs:493` 附近）时，消息排进子会话当前的 Run，不会启动新 Run。
- Kernel 生成 `RunId` 的位置：
  - `run_until_settled`（`crates/kernel/src/runtime/run.rs:82`）：agent 循环与直接斜杠命令；
  - 用户 `!` 命令（`crates/kernel/src/runtime/command/user_bash.rs:106`），也在 `run_until_settled` 内，先于上面一处；
  - Tree 导航的分支摘要（`crates/kernel/src/runtime/session/tree.rs:205`）；
  - 压缩（`crates/kernel/src/runtime/compaction.rs:218`，调用方未提供 `identity` 时）：独立压缩 `compact_session`（`compaction.rs:135`），以及循环内的三种自动压缩——发提示前（`turn.rs:221`）、上下文溢出恢复（`turn.rs:1067`）、结算时达到阈值（`crates/kernel/src/runtime/run/settlement.rs:105`）。只有 `/compact`（`crates/kernel/src/runtime/command/builtin.rs:115`）传入所在命令的身份、不另生成 Run id。
- agent 循环、用户命令与斜杠命令都经过 `run_with_source`（`run.rs:36`）；独立压缩与 Tree 摘要不经过。
- 排队的 steering 与 follow-up 消息在同一个 Run 的 Turn 循环里被认领（`claim_next_or_close`，`turn.rs:1188`），所以一个 Run 就是一次完整的 agent loop。
- ACP 等待客户端回复权限请求与 elicitation 的任务用 `connection.spawn` 启动（`crates/acp/src/permission.rs:91`、`crates/acp/src/elicitation.rs:249`、`:271`），不继承 Run 的 span。其余生产代码中的 spawn 属于连接、会话或 ACP 请求级别（MCP 传输与会话、登录、ACP 请求任务、MCP 投影监听），或只读取进程输出（bash），不代表某个 Run 执行工作。
- 日志层（`crates/app/src/logging.rs`）：`TraceContextLayer`（`crates/app/src/logging/trace.rs`）只从 span 读取 `trace_id`，字段名常量 `TRACE_ID_FIELD` 定义在 app 内，ACP 则以字面量声明同名字段；`ModelTracingLayer`（`crates/app/src/logging/telemetry.rs`）在 Rig span 上汇总模型调用信息；格式化器（`crates/app/src/logging/format.rs`）特殊处理 Rig span 和 ACP SDK 的 handler 链。`with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)` 让 fmt 层为每个 span 计时并生成生命周期事件，格式化器除 Rig 外全部丢弃。
- 颜色由 `[logging] color` 决定，不检查 stderr 是否为终端，也不读取 `NO_COLOR`。依赖库事件的来源只显示文件名，如 `mod.rs:405`。
- `trace_id` 不持久化，只用于日志关联与上面那条数据链。
- 实测：一次 27 秒的运行共 92 行日志，其中 34 行是 `started/completed ACP request`，大多是耗时 0 ms 的快照请求；每次模型调用 4 行（Rig 的 `model request start/done` 加 Kernel 的 `started/completed Provider request`）；最长一行 515 个字符。

## 3. Kernel：Run span 与 trace 归属

### 3.1 定义

trace 等于一个顶层 Kernel Run。每个生成 `RunId` 的位置打开且只打开一个 `run` span：

- 顶层 Run 声明新的或继承来的 trace：
  - `run_until_settled`：agent 循环（包括它在循环内处理的排队消息）、用户 `!` 命令、直接斜杠命令；
  - 独立压缩 `compact_session`；
  - Tree 导航的分支摘要。导航持有会话执行锁，不会发生在 Run 之内。
- 嵌套 Run：在已有 Run 内生成的 Run id，即循环内的三种自动压缩，只声明 `run_id`，trace 由外层 span 提供。
- `/compact` 复用所在斜杠命令的 Run id，不再打开 span。

### 3.2 span 的两种形状

两种形状只由 `RunSpan`（第 7 节）创建，字段名取自 `protocol` 中的常量：

```rust
// Top-level Run: declares its trace.
tracing::error_span!("run", { TRACE_ID_FIELD } = %trace_id, { RUN_ID_FIELD } = %run_id)
// Run nested inside another Run: inherits the trace from the enclosing span.
tracing::error_span!("run", { RUN_ID_FIELD } = %run_id)
```

只带规则 14 允许的关联字段。操作的 future 用这个 span instrument。span 使用 ERROR 级别：过滤器按级别启用 span，`filter = "warn"` 时 INFO span 会被关闭，Run 内的警告与错误就失去前缀；span 不再输出生命周期事件（6.2），级别只决定它是否存在。

### 3.3 打开位置

- `run_with_source` 在调用 `run_until_settled` 之前生成 `RunId`、确定 trace，并用顶层 span 包住整个调用；`run_until_settled` 接收这两个 id，再进入用户命令、斜杠命令和 agent 循环三个分支。用户命令改为接收传入的 `run_id`，不再自己生成；斜杠命令已经从上层接收 `run_id`，不变。
- `compact_session` 自己创建 `CompactionIdentity`、打开顶层 span，并把身份传给 `perform_compaction`。
- `perform_compaction` 在调用方未提供身份时（此后只剩循环内的自动压缩）生成 Run id，并打开嵌套 span。
  - 无可压缩上下文时返回 `None`，调用方不压缩、继续执行；上下文溢出恢复只在真正压缩后才重试。
  - 结算时的自动压缩失败只记一行 WARN，Run 仍发出 `AgentSettled`：回答已经完成，压缩失败由它自己的生命周期上报；否则客户端会一直等这个 Run 结束，收到下一个 Run 时只能整体重连。持久化错误仍由随后的 `sync_store` 返回。
- Tree 摘要在生成 Run id 的位置（`tree.rs:205`）打开顶层 span。
- 以后新增在 Run 的 future 内启动、替 Run 执行工作的 spawn 要带上 `tracing::Span::current()`。ACP 中等待客户端回复权限请求与 elicitation 的三个任务（`permission.rs:91`、`elicitation.rs:249`、`:271`）不在此列：这些请求由 Run 之外的任务发出（投影/watcher 路径，以及 MCP 客户端自己的任务），启动处没有 Run span（见第 9 节）。

### 3.4 trace 来源

新增枚举 `TraceOrigin`，替换 `SubmitRequest.trace_id: Option<TraceId>` 和 `RunInvocation.trace_id: TraceId`：

```rust
/// Where a new top-level Run's trace comes from.
pub enum TraceOrigin {
    /// Foreground subagent: the child Run continues the caller's trace.
    Inherit(TraceId),
    /// Background subagent: a new trace whose start line names the caller's trace.
    LinkFrom(TraceId),
    /// Any other Run: a new trace.
    New,
}
```

- 前台子代理：`Inherit(context.trace_id)`。
- 后台子代理：Reporter 在创建时记录发起方的 trace，向子会话提交任务时使用 `LinkFrom(parent)`。
- 其他 Run 一律 `New`：ACP prompt、扩展触发的 Run、后台回报投递给发起方、`Kernel::run`。独立压缩与 Tree 摘要不经过 `RunInvocation`，直接使用新 trace。
- `TraceOrigin` 实现 `Default`（`New`），`SubmitRequest` 与 `RunInvocation` 的 builder 用 `#[builder(default)]`；`crates/acp/src/projection.rs` 中现有的 `SubmitRequest` 调用不需修改。
- `TraceOrigin` 只在提交启动新 Run 时生效；若提交被排进目标会话正在运行的 Run，它在那个 Run 的 trace 下处理。因此 Reporter 自己的投递日志（不在任何 Run 内）同时写出父 trace 与目标会话，即使没有启动新 Run 也能关联。

### 3.5 删除与保留

- 删除 `kernel::PromptRequest` 的 `trace_id` 字段；ACP 不再向 Kernel 传入 trace id。
- `Kernel::run_traced(request, sink, trace_id, insertion)` 改为 `Kernel::run_with_insertion(request, sink, insertion)`：去掉 trace 参数，保留测试与宿主需要的插入通知入口。
- 删除 `SlashCommandExecution.trace_id`，它只用于日志正文。
- 保留 `RunScope` → `TurnScope` → `ToolBatch` → `ToolExecutionContext` 这条链上的 `trace_id`：它是数据，子代理工具用它决定 `TraceOrigin`，MCP 用它拼请求 id（`crates/mcp/src/tool.rs:200` 传入、`crates/mcp/src/client/handler.rs:287` 使用）。值为所在顶层 Run 的 trace。各层只删除日志正文对它的打印。MCP host 上下文不变。

## 4. ACP

### 4.1 请求日志器

`AcpTraceFactory` 与 `AcpOperationTrace` 改为不生成 id、不打开 span 的请求日志器，类型分别更名为 `AcpRequestLogs` 与 `AcpRequestLog`（第 7 节）。模块路径 `acp::trace` 不变，使现有 `[logging] filter` 中的 `acp::trace=debug` 等指令继续生效。现有方法（`request`、`notification`、`run`、`start`、`settle`、`fail`）按 4.2 的级别输出。ACP 请求处理中的日志前缀为 `[-]`；在 Run 内发生的事件带所在 Run 的 `[trace run]`。

### 4.2 日志级别

| 类别 | 方法 | 日志 |
|---|---|---|
| 只读查询 | 标准方法 `session/list`；扩展方法 `AcpExtensionMethod::{Tree, PendingMessages, SessionRuntime, SkillList, McpStatus, McpPromptGet, McpResourceRead, McpComplete}` | 开始与完成只在 DEBUG；失败 WARN |
| 有修改的操作 | 其余全部请求，包括 `initialize`、`session/new`、`session/resume`、`session/set_config_option`、`session/prompt`、登录，以及扩展方法 `Navigate`、`Branch`、`Fork`、`Compact`、`PendingMessageRemove`、`PendingMessagePromote`、`ClearQueue`、`InvokeSkill`、`McpReconnect`、`McpOAuthContinue`、`ExtensionCommand`、`UserBash` | 开始行降为 DEBUG；完成时一行 INFO，包含方法、请求 id 与耗时；失败 WARN |
| 通知 | `session/cancel` | 完成时一行 INFO |

- INFO 行不写会话：参数只在 DEBUG 时序列化（`crates/acp/src/trace/parameters.rs:58`），为 INFO 行取会话要么每个请求都序列化（prompt 含图片），要么为约 10 种请求类型各写一个访问器。各处理逻辑自己的生命周期日志已写明会话（如 "completed Session Resume session-…"、Kernel 的 "started Kernel Run for session …"）。

- 分类见第 7 节：`AcpExtensionMethod::is_query()` 与 `AcpRequestKind`。
- 参数日志不变：DEBUG 级别记录完整参数并递归脱敏凭据字段，INFO 不记录参数（规则 13）。

### 4.3 `session/prompt`

- ACP 请求在 JSON-RPC 响应发出时完成，用与其他请求相同的完成行记一行 INFO："completed ACP request session/prompt:N over T in M ms"，不另设专门的 prompt 日志。直接启动 Run 与排进运行中循环两种情况相同：排队时响应要等运行中的循环插入这条消息才发出，M 如实反映等待时间。会话与消息 id 已由 Kernel 记录："started Kernel Run for session …"、"admitted prompt … into queue Q"、"reported prompt insertion of message …"。
- 插入前被拒绝或取消时记 WARN。
- 不再用 `task_operation.settle` 包住后台任务。Run 的开始、Turn 与结束只由 Kernel 在 Run span 内记录；后台任务在 `kernel.prompt` 返回后记录的失败日志位于 Run 之外，正文保留会话 id。

### 4.4 不变的部分

- 会话 watcher 与投影的日志级别不变。在 Run 内发出的投影日志（如 "recorded ACP Operation Start"）自动带上 `[trace run]`，正文不再重复 Run id。
- Agent 向客户端发出的请求（`session/request_permission`、`elicitation/create`）由 Run 之外的任务发出，发出与回复的日志前缀为 `[-]`，正文写明会话与请求 id（见第 9 节）。

## 5. 日志前缀与消息

### 5.1 前缀

- `CorrelationLayer`（由现有 `TraceContextLayer` 改名）在 span 创建时用同一个 visitor 识别 `TRACE_ID_FIELD` 与 `RUN_ID_FIELD`，分别写入 `TraceContext`、`RunContext` 两种类型化扩展数据。
- 格式化器通过 `SpanCorrelation` 沿事件所在 span 链由内向外遍历一次，分别取最近的 `TraceContext` 与最近的 `RunContext`，输出 `[trace-… run-…]`；两者都没有时输出 `[-]`。
- 由此：前台子 Run 声明了继承来的 trace，显示 `[父 trace 子 run]`；循环内的自动压缩只声明 `run_id`，显示 `[循环 trace 压缩 run]`。

改动后的样例（id 已截短）：

```
INFO [trace-HJWZ… run-umuQ…] crates/kernel/src/runtime/run.rs | started Kernel Run for session session-6KD… from Rpc
INFO [trace-HJWZ… run-umuQ…] crates/kernel/src/runtime/run/turn.rs | started Turn turn-MU1n…
INFO [-] crates/acp/src/trace.rs | completed ACP request session/prompt:14 over http in 1 ms
INFO [trace-HJWZ… run-umuQ…] crates/kernel/src/runtime/tools/batch.rs | started Tool call call_45… (bash)
INFO [trace-HJWZ… run-umuQ…] crates/kernel/src/runtime/tools/batch.rs | completed Tool call call_45… (bash) in 6 ms
INFO [trace-HJWZ… run-c0mp…] crates/kernel/src/runtime/compaction.rs | started Compaction Turn turn-9Fq… for session session-6KD… with reason Threshold in 0 ms; will retry false
INFO [trace-HJWZ… run-umuQ…] crates/kernel/src/runtime/run/turn.rs | settled Kernel Run in 26991 ms with outcome Succeeded
```

### 5.2 消息规则

仅适用于在 Run span 内发出的事件：

- 正文不再包含 trace id 与本 Run 的 id。
- 会话 id 只出现在每个 Run 的起始行。agent 循环的起始行是 "started Kernel Run for session S …"，同时写明来源：现有 `InputSource`（`Interactive`、`Rpc`、`Extension`）；`TraceOrigin::Inherit` 写 "as foreground subagent"；`TraceOrigin::LinkFrom` 写 "as background subagent linked to trace T"。不经过 agent 循环的 Run 没有这一行，各自的起始行写明会话：直接斜杠命令 "started Slash Command /x for session S"、用户命令 "started user bash for session S with message M"、独立压缩与循环内压缩的 "started Compaction Turn T for session S …"。
- Turn id 只出现在 Turn 的开始与结束行；工具调用 id 只出现在工具调用的开始与完成行。删除工具批次和工具调用日志末尾的 ", Run R, Turn T, trace X"。
- Run 之外的事件（ACP 请求、准入、watcher、后台 Reporter 的投递日志）照常在正文写会话 id。
- 正文保持可读文本与格式化参数，不使用结构化事件字段（规则 14）。

### 5.3 范围

所有在 Run 内输出且重复上述 id 的 Kernel 与 ACP 日志位置：`run.rs`、`run/turn.rs`、`run/lifecycle.rs`、`tools/batch.rs`、`command/execution.rs`、`command/user_bash.rs`、`compaction.rs`、`acp/src/projection.rs` 与子代理模块。实现计划逐行列出（按正文含 `Run {}` 或 `trace {}` 的日志全量检索得到）。`run/queue/operations.rs` 的入队日志发生在目标 Run 之外，保留其中的 Run id。

## 6. 附带修复

### 6.1 每次模型调用一对日志

- 保留 Kernel `crates/kernel/src/provider.rs` 中的 "started / completed Provider request <provider/model>"。完成行追加：耗时；提供方返回的响应模型与响应 id（`CompletionResponse::model()`、`response_id()`，有才写）；提供方报告的各项 token 数（输入、输出、缓存读取、缓存写入、推理），未报告的不写，报告为 0 的照写 0。用量文本由 `ModelUsage` 的方法生成（第 7 节）。
- 失败、取消和流错误沿用现有 WARN/INFO 日志。提供方请求 id 只存在于 Rig 错误上（`provider_request_id()`），在 `provider.rs` 的失败位置、调用 `ModelFailure::from(error)` 之前读取并写入日志；`ModelFailure` 不变。
- 取舍：成功时 `CompletionResponse` 不提供提供方请求 id，成功日志因此不再包含它（现有 Rig 摘要会在提供方返回时记录）；响应 id 仍然保留。
- 删除 `ModelTracingLayer` / `RigSpan`、格式化器中的 Rig 分支，以及 `provider.rs` 中没有字段的 `provider_request` span（第 7.3 节）。
- 提示与响应正文、认证头和原生载荷仍然不记录。

### 6.2 去掉 span 生命周期事件

- 删除 `.with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)`，fmt 层不再为每个 span 计时、生成事件。
- 改正 `.without_time()` 旁边与此相关的过时注释。
- 格式化器只负责 `[trace run]` 前缀和现有的 ACP SDK handler 链省略。

### 6.3 颜色

- `[logging] color` 的含义改为"允许使用颜色"。
- 只有当该项为 `true`、stderr 是终端（标准库 `std::io::IsTerminal`）、且 `NO_COLOR` 未设置或为空时才输出 ANSI 转义码。
- `LoggingFactory::new(filter, color)` 仍接收明确的颜色开关，供测试使用；`from_config` 与读取 `RUST_LOG` 一起计算实际是否使用颜色（计算逻辑的归属见第 7 节）。

### 6.4 依赖库事件来源

依赖库事件（源文件为绝对路径）显示 target，如 `rig::completions`，不再显示文件名与行号；本项目代码仍显示 `crates/…/x.rs:123`。

### 6.5 文档

重写 `README.md` 与 `README_zh.md` 的 "Logging" / 日志 小节：trace 的含义（一个顶层 Run）、前缀格式、模型调用日志的内容、颜色规则、`[logging] filter` 与 `RUST_LOG` 对这些输出的控制；删除 Rig 摘要相关说明；保留 ACP DEBUG 参数日志及其凭据脱敏的说明，并注明 `acp::trace=debug` 同时会输出只读查询的 DEBUG 日志。示例配置中的 `filter` 保持不变。

## 7. 代码组织

### 7.1 原则

- 每项职责只有一个归属：span 字段名、span 的创建、trace 的决定、请求分类、前缀渲染、颜色判断、用量文本各在一处定义，其他地方只调用。
- 方法挂在拥有数据的类型上（规则 8、15），转换用标准 trait（`From`、`Default`、`Display`）。不新增无状态工具结构体，不新增只接收一个项目类型参数的自由辅助函数。
- 用枚举表达状态，替换布尔标志与语义重载的 `Option`：`quiet_success: bool` → `AcpRequestKind`；`Option<TraceId>`（"有则继承"）→ `TraceOrigin`。
- 删除失去用途的代码，不保留兼容层或空壳。
- 遵守项目规则：新增与修改的非平凡逻辑写英文注释，每个新函数写文档注释（规则 1）；超过 3 个字段的结构体用 typed-builder，`Option` 字段加 `#[builder(default)]`（规则 5）；`Arc` 用 `Arc::clone(&x)`（规则 6）；tracing 事件宏写全路径并使用可读消息（规则 14）；不新增依赖（规则 11、12 无需改动）。

### 7.2 职责归属

| 职责 | 归属 | 位置 |
|---|---|---|
| span 字段名 | 常量 `TRACE_ID_FIELD`、`RUN_ID_FIELD`；Kernel 以 `{ CONST }` 声明字段（tracing 0.1.44 支持），app 与测试捕获层按同一常量识别 | 新模块 `crates/protocol/src/trace.rs`；app 中原有的 `TRACE_ID_FIELD` 删除 |
| 新 Run 的 trace 来源与解析 | `TraceOrigin`；方法 `trace_id(&self, ids: &dyn IdGenerator)` 对 `Inherit` 返回原值、对其余生成新 id | 新模块 `crates/kernel/src/runtime/run/trace.rs`，由 `kernel` 公开导出 |
| 打开 Run span | `RunSpan`，持有 `tracing::Span`：`top_level(trace_id, run_id)`、`nested(run_id)`、`instrument(future)`；全仓库只有这里创建 `run` span。由 `kernel` 公开导出，使 app 的格式化器测试能用真实的 Run span 验证前缀 | 同上 |
| Run 起始行的来源短语 | `RunInvocation` 的方法，它同时持有 `InputSource` 与 `TraceOrigin` | `crates/kernel/src/runtime/run.rs` |
| token 用量的日志文本 | `ModelUsage` 的方法，只写提供方报告的项 | `crates/protocol/src/usage.rs` |
| ACP 请求分类 | `AcpExtensionMethod::is_query()`；`AcpRequestKind { Query, Change }` 实现 `From<&str>`（方法名），在此一并判断标准方法 `session/list` | `crates/protocol/src/identity.rs`、`crates/acp/src/trace.rs` |
| ACP 请求日志 | `AcpRequestLogs`（持有传输类型，创建单个请求的日志器，不再可失败）；`AcpRequestLog`（持有请求身份与开始时间，身份含描述、参数与 `AcpRequestKind`） | `crates/acp/src/trace.rs` |
| 记录 span 上的关联 id | `CorrelationLayer`，一个 visitor 同时识别两个字段，写入 `TraceContext` / `RunContext` | `crates/app/src/logging/trace.rs` |
| 渲染前缀 | `SpanCorrelation { trace, run }`：一次遍历 span 链取得两者，`Display` 输出 `[trace run]` 或 `[-]` | 同上 |
| 颜色判断 | `LoggingFactory` 的关联函数，输入配置值、是否为终端、`NO_COLOR` 的值；`from_config` 调用它 | `crates/app/src/logging.rs` |
| 事件来源 | 现有 `EventSource` 的 `Display` 增加依赖库分支 | `crates/app/src/logging/format.rs` |

### 7.3 删除的代码

- `crates/app/src/logging/telemetry.rs` 整个文件（`ModelTracingLayer`、`RigSpan`）；格式化器中的 `ModelLifeCycle` 与 Rig 分支；`FmtSpan` 配置；app 内的 `TRACE_ID_FIELD` 常量。
- `AcpOperationTrace` 的 span 字段、`trace_id()` 与 `instrument()`；`quiet_success`；`AcpTraceFactory` 中生成 `TraceId` 的逻辑与 `IdGenerator` 依赖。由此 `AcpServerFactory::new` 与 `AcpServerFactory::http_router` 的 `id_generator` 参数、只为它存在的 `app::Application.id_generator` 字段一并删除。
- `kernel::PromptRequest.trace_id`、`SlashCommandExecution.trace_id`；`Kernel::run_traced` 改为 `run_with_insertion`（3.5）。
- `provider.rs` 中的空 `provider_request` span。
- 各日志正文中重复的 trace、本 Run、会话、Turn 尾巴（第 5.2 节）。

## 8. 测试与验收

Rust 按 TDD 进行：先写失败测试。测试只放在 crate 级 `tests/` 目录（规则 10）。Kernel 与 ACP 测试不能依赖 app 的格式化器，沿用两处已有的日志捕获（`crates/kernel/tests/support/mod.rs`、`crates/acp/tests/tracing.rs`）：tracing-subscriber 默认格式会输出 span 链及其字段，如 `run{trace_id=… run_id=…}:run{run_id=…}:`，断言直接检查这些字段，不另写捕获层。两个 crate 已有 `tracing-subscriber` 开发依赖，不需新增。app 测试用 `kernel::RunSpan` 创建真实的 Run span，验证生产格式化器的前缀。

| 编号 | 位置 | 场景 | 必须证明的结果 |
|---|---|---|---|
| R01 | `crates/app/tests/logging.rs` | 前缀 | 在 `run{trace_id, run_id}` 内的事件前缀为 `[trace run]`；嵌套的 `run{run_id}` 继承外层 trace、显示自己的 run；嵌套的顶层 span 取最内层的值；Run 之外为 `[-]`。替换现有"扁平化为一个 trace id"的测试 |
| R02 | `crates/app/tests/logging.rs` | 无生命周期事件 | 打开、关闭 span（包括 `rig::completions` span）不产生日志行 |
| R03 | `crates/app/tests/logging.rs` | 颜色 | `color = true` 时，写入目标不是终端或 `NO_COLOR` 非空都不输出 ANSI；三个条件都满足时才输出 |
| R04 | 手动验收 | 来源 | 依赖库事件显示 target；本项目事件显示路径与行号（现有测试已覆盖）。tracing 宏总是写入调用处的相对路径，集成测试无法构造来自依赖库的绝对路径事件，因此依赖库分支在手动验收中确认 |
| R05 | `crates/app/tests/logging.rs` | 删除 | 删除 3 个 Rig 摘要测试 |
| R06 | `crates/acp/tests/tracing.rs` | prompt 的 trace | 提供方看到的 trace 是 Kernel 在 Run 开始时生成的；ACP 请求自身的日志前缀为 `[-]`。改写 `prompt_trace_reaches_the_provider_background_task` 中与 trace 相关的断言，保留其中 DEBUG 参数脱敏的断言（规则 13） |
| R07 | `crates/acp/tests/tracing.rs` | 日志级别 | INFO 级别下，只读查询（`tree`、`session/list`）不产生日志；有修改的请求只产生一行 INFO 完成日志 |
| R08 | `crates/acp/tests/tracing.rs` | prompt 完成时机 | `session/prompt` 的完成行在 "reported prompt insertion" 之后、"settled Kernel Run" 之前写出，前缀为 `[-]`。排队路径的响应时机由现有插入语义决定，本次不改动 |
| R09 | kernel tests | 前台子代理 | 子 Run 的事件带父 trace 与子 Run id |
| R10 | kernel tests | 后台子代理 | 子会话空闲：子 Run 使用新 trace，起始日志写明父 trace；子会话忙：消息排进子会话当前 Run，Reporter 的投递日志写明父 trace 与子会话 |
| R11 | kernel tests | 全部入口 | 扩展触发、`Kernel::run`、用户命令、直接斜杠命令、独立压缩、Tree 摘要发起的 Run 都有带新 trace 的顶层 span；循环内的自动压缩打开只带 `run_id` 的嵌套 span，事件带循环的 trace 与压缩的 Run id；`/compact` 不另开 span |
| R12 | kernel tests | 模型调用日志 | 完成行包含耗时、响应 id 与提供方报告的 token 数；未报告的不出现；失败行在错误带有时包含提供方请求 id |

`Kernel::run_traced` 改为 `run_with_insertion` 后需迁移其测试调用：`crates/kernel/tests/turn.rs`、`crates/kernel/tests/mcp_host.rs`、`crates/kernel/tests/protocol_alignment/insertion.rs`、`protocol_alignment/title.rs`、`protocol_alignment/queue.rs`。

手动验收：重跑同一条真实运行（2 次工具调用、4 次模型调用），对比改动前后的日志，确认第 1 节的成功标准。本次改动不涉及 WebUI，不需要 WebUI e2e。

## 9. 风险与已知限制

- 准入阶段不在任何 trace 内：prompt 进入 Run 之前的扩展输入钩子、登录检查与入队都发生在 Run 之外，前缀为 `[-]`。慢的输入钩子不会出现在 Run 的 trace 里。
- span 在 `run_with_source` 中、进入 `run_until_settled` 之前打开，等待会话执行锁的时间计入新 Run 的 span，此时 RunStart 尚未发出；在等锁时被取消的 prompt，其取消日志挂在一个从未开始的 Run id 下。
- 后台子代理的回报投递给发起方时启动的新 Run 使用新 trace，不会自动关联到子代理的 trace；只能通过 Reporter 投递日志中的会话与 trace 关联。
- 成功的模型调用日志不再包含提供方请求 id（见 6.1）。
- 只有 `run` 一种 span，Turn 和工具调用没有自己的 span；以后接入 OpenTelemetry 时可再增加。
- 所有在 Run 的 future 内启动、替 Run 执行工作的 spawn 都必须带着当前 span；新增的 spawn 若遗漏，日志会退化为 `[-]`。R11 覆盖现有位置。
- 权限请求与 elicitation 的发出与回复日志不在 Run 的 trace 内：请求由投影/watcher 路径（以及 MCP 客户端自己的任务）发出，启动处没有 Run span。要纳入 trace，需要在 Kernel 发出请求事件处捕获 `Span::current()`，并经投影与 watcher 传到发送与回复处理处；这是独立的改动。目前这些日志写明会话与请求 id，可与 Run 的起始行人工关联。

## 10. 不在范围

- OpenTelemetry 导出与 W3C trace id 格式。
- 持久化 trace id。
- 调整会话 watcher 与投影日志的级别。
- 把后台回报引发的 Run 并入原 trace（即"整条因果链一个 trace"）。

# TUI 通过 ACP Server 解耦 Kernel 设计方案

**日期**: 2026-05-16  
**状态**: 待用户审核  
**替代**: `docs/superpowers/specs/2026-05-16-tui-kernel-design.md`

## 1. 背景

当前 TUI 第一版实现方向是直接依赖 `kernel`，由 `crates/tui` 构造 `Kernel`，调用 `protocol::AgentKernel`，并直接消费 `protocol::Event`。

最新决策是反转这个方向：TUI 的 UI 交互协议不再绑定 kernel event，而是作为 ACP client 消费 ACP 的 `SessionNotification` / `SessionUpdate` / `RequestPermissionRequest`。TUI 可以在启动 ACP server 时依赖 `acp`、`kernel`、`config`、`provider`、`tools` 等 crate；真正需要隔离的是 UI reducer 与 app 主交互路径不能直接消费 kernel 的内部事件流。

新的边界是：

```text
TUI
  <-> Agent Client Protocol
  <-> ACP server (`crates/acp`, in-process by default)
  <-> kernel
```

这会让 UI 的交互协议与 kernel 内部事件解耦。ACP server 成为 TUI 的后端协议边界。

## 2. 目标

1. TUI 的 UI 状态和渲染逻辑不再消费 `kernel` 的内部事件流，尤其不再以 `protocol::Event` 作为 UI reducer 的输入。
2. TUI 以 ACP request/notification 作为交互协议，依赖 ACP `SessionNotification` / `SessionUpdate` / `RequestPermissionRequest` 实现 UI。
3. `crates/tui` 在必要时允许依赖 `config`、`provider`、`tools`、`protocol`，甚至 `kernel`；但这些依赖不能重新把 UI reducer 绑定到 kernel event，也不能绕过 ACP 作为 TUI 的主要交互协议。
4. TUI 继续保留当前已有 UI 能力：
   - prompt 输入。
   - session new/load/list。
   - streaming assistant/thought/tool/usage 渲染。
   - approval overlay。
   - transcript 手动滚动和 tail follow。
   - tool call 折叠/展开。
   - model/cwd/token 底部状态栏。
5. TUI 可以直接依赖 `acp` 模块，在进程内启动 ACP server，并通过内存 transport 与其交互；外部 ACP server 进程是 P2 需求，P1 不实现。
6. `crates/acp` 继续是 kernel 到 ACP 的 adapter，TUI 不直接消费 adapter 之前的 kernel event。

## 3. 非目标

1. 不重写 kernel runtime。
2. 不让 UI reducer 直接消费 `ClawcodeAgent` 之前的 kernel event；即使使用 in-process ACP server，UI 输入也必须是 ACP update。
3. 不在 TUI crate 中保留任何 `protocol::Event` reducer。
4. 不实现完整 terminal emulator。shell output 仍做文本级归一化。
5. 不改变 ACP schema。
6. 不在这一阶段实现远程 ACP 配置发现或多 server 管理 UI。

## 4. 选定架构

TUI 作为 ACP client，通过 `agent-client-protocol` 与 ACP server 通信。

```text
crates/tui
  -> agent-client-protocol
  -> in-process duplex transport by default
  -> optional process/stdio transport in P2
  -> ratatui/crossterm

crates/acp
  -> protocol
  -> kernel
  -> config/provider/tools
```

`crates/tui` 的 UI reducer 只允许把 ACP schema 类型或本地 UI 类型作为渲染输入。Cargo 依赖不是本设计的硬边界；硬边界是 TUI 不能直接用 `protocol::Event` 驱动 UI，也不能通过直接调用 `AgentKernel::prompt()` 跳过 ACP 交互路径。

## 5. ACP Server 启动与 Transport

第一版优先使用 in-process ACP server。这与 `crates/acp/src/bin/client.rs` 的现有做法一致：

1. TUI 构造后端运行所需对象，例如 `config::load()`、`ToolRegistry::register_builtins()`、`Kernel::new(...)`、`kernel.register_agent_tools()`。
2. TUI 创建 `acp::agent::ClawcodeAgent::new(Arc::new(kernel))`。
3. TUI 创建两组 `tokio::io::duplex(64 * 1024)`：
   - client outgoing -> agent incoming
   - agent outgoing -> client incoming
4. TUI 用 `agent_client_protocol::ByteStreams` 包装两侧 IO：
   - `client_io = ByteStreams::new(client_outgoing.compat_write(), client_incoming.compat())`
   - `agent_io = ByteStreams::new(agent_outgoing.compat_write(), agent_incoming.compat())`
5. TUI `tokio::spawn` agent task：
   - `agent.serve(agent_io).await`
6. TUI 侧通过 `Client::builder().connect_with(client_io, ...)` 连接 ACP agent。

这个模式允许 TUI 依赖 `acp`、`kernel`、`config`、`provider`、`tools` 等模块来启动 server，但 UI 层仍只消费 ACP request/notification。

P1 不实现外部 ACP server 进程 transport，也不提供 `--acp-command` 或 `CLAW_ACP_COMMAND`。后续 P2 如果需要支持外部 server，可以在 `acp_client` 后面增加 process/stdio transport，但不能改变 `app.rs` 只通过 ACP connection 交互的边界。

## 6. 文件结构

调整后的 `crates/tui/src`：

```text
crates/tui/src/
├── acp_client.rs       # ACP client connection, requests, notifications, permission responders
├── acp_server.rs       # in-process ACP server bootstrap and duplex transport
├── app.rs              # TUI event loop and ACP orchestration
├── event.rs            # crossterm event normalization
├── lib.rs
├── main.rs             # CLI
├── terminal.rs         # raw mode / alternate screen restore
└── ui/
    ├── mod.rs
    ├── approval.rs     # renderable ACP approval state and key mapping
    ├── composer.rs
    ├── render.rs
    ├── state.rs        # reduce ACP SessionUpdate into UI state
    └── view.rs         # scroll/follow-tail/tool-fold state
```

保留或重写：

```text
crates/tui/src/bootstrap.rs
```

它不再作为“UI 直连 kernel”的 bootstrap，而是可以改为 `acp_server.rs` 或 `bootstrap.rs` 中的 in-process ACP server bootstrap。无论命名如何，这层只能负责启动 ACP server，不能把 `protocol::Event` 暴露给 UI。

## 7. ACP Client 层

新增 `acp_client.rs`。

职责：

1. 连接 ACP server transport。
2. 建立 ACP client connection。
3. 发送 session/prompt/cancel/list requests。
4. 接收 ACP notifications。
5. 接收 permission request，并把 responder 暂存给 app loop。
6. 把 ACP callback 转成 TUI app 内部事件。

新增或重写 `acp_server.rs` / `bootstrap.rs`。

职责：

1. 构造 in-process ACP server 需要的 kernel/config/provider/tools。
2. 构造 `acp::agent::ClawcodeAgent`。
3. 建立 duplex `ByteStreams`。
4. 启动 `agent.serve(agent_io)` 后台任务。
5. 返回 client 侧 `ByteStreams` 和 agent task handle，供 `acp_client` 连接与生命周期管理。

建议的内部事件：

```rust
pub enum AppEvent {
    Terminal(TuiEvent),
    SessionUpdate {
        session_id: agent_client_protocol::schema::SessionId,
        update: agent_client_protocol::schema::SessionUpdate,
    },
    PermissionRequested(PendingPermissionRequest),
    PromptFinished(agent_client_protocol::schema::StopReason),
    PromptFailed(String),
    AcpError(String),
}
```

`PendingPermissionRequest` 只保存可渲染字段和一个本地 request id。真实 responder 不放进 UI state，而由 `app.rs` 或 `acp_client.rs` 的 pending map 持有。

## 8. App Loop

`app.rs` 从“kernel event loop”改为“ACP client event loop”。

启动流程：

```text
parse CLI
connect ACP server
initialize
list sessions OR open session
enter terminal
run event loop
```

打开 session：

```text
--resume <SESSION_ID> -> LoadSessionRequest
otherwise             -> NewSessionRequest
```

发送 prompt：

```text
Composer submit
  -> append local user message
  -> PromptRequest(session_id, [ContentBlock::Text])
  -> ACP server streams SessionNotification during request
  -> PromptResponse returns StopReason
```

取消：

```text
Ctrl+C while running
  -> CancelNotification(session_id)
```

退出：

```text
Ctrl+C or Esc while idle
  -> restore terminal
  -> terminate child ACP process if this TUI spawned it
```

## 9. UI State Reducer

`ui::state::AppState` 从：

```rust
apply_event(protocol::Event)
```

改为：

```rust
apply_session_update(agent_client_protocol::schema::SessionUpdate)
```

映射规则：

| ACP update | UI state |
| --- | --- |
| `AgentMessageChunk` | append/coalesce `TranscriptCell::Assistant` |
| `AgentThoughtChunk` | append/coalesce `TranscriptCell::Reasoning` |
| `ToolCall` | create/update `ToolCallView` |
| `ToolCallUpdate` | update title/status/content/output |
| `Plan` | first pass 可忽略或 render system row |
| `UsageUpdate` | update token usage total |

`ToolCallView` 不再使用 `protocol::ToolCallStatus`。建议新增 UI 本地 enum：

```rust
pub enum UiToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
```

这样 UI 不绑定 kernel internal status，也不把 ACP schema 类型扩散到 render 细节里。

## 10. Approval 处理

direct-kernel 版本：

```text
protocol::Event::ExecApprovalRequested
-> kernel.resolve_approval(...)
```

ACP-client 版本：

```text
ACP server -> RequestPermissionRequest
TUI shows approval overlay
user presses allow/reject
TUI responds RequestPermissionResponse
ACP server maps response back to kernel.resolve_approval(...)
```

UI state 只保存：

```rust
PendingApproval {
    request_id: PermissionRequestId,
    title: String,
    body: String,
}
```

Responder map 由非 UI 层持有：

```text
request_id -> Responder<RequestPermissionResponse>
```

这样 render state 不持有 async responder，也不会把 transport 生命周期放进 UI data model。

## 11. Tool Call 折叠与滚动

现有 `ui::view::ViewState` 保留：

```rust
transcript_scroll
follow_tail
tool_calls_collapsed
```

交互保持：

| Key | Behavior |
| --- | --- |
| `PageUp` | manual scroll up |
| `PageDown` | manual scroll down |
| `Home` | top |
| `End` | bottom + follow tail |
| `Ctrl+T` | toggle tool collapse |

Tool call 默认折叠。展开时显示 ACP `ToolCallUpdate` content 中的文本块。shell output 继续做：

1. `\r` 当前行覆盖归一化。
2. ANSI escape sequence stripping。
3. control char filtering。

不实现完整 VT100 terminal state。

## 12. Cargo 依赖策略

`crates/tui/Cargo.toml` 必须包含 ACP client 所需依赖：

```toml
agent-client-protocol = { workspace = true, features = ["unstable"] }
tokio-util = { version = "0.7", features = ["compat"] }
```

为了支持默认 in-process ACP server，`crates/tui` 可以直接依赖：

```toml
acp = { path = "../acp" }
kernel = { path = "../kernel" }
config = { path = "../config" }
provider = { path = "../provider" }
tools = { path = "../tools" }
```

`protocol` 也不是禁止依赖项。是否保留取决于实现是否确实需要：

1. 默认 in-process server 需要 `acp/kernel/config/provider/tools` 依赖，这是允许的。
2. 外部-only transport 是 P2 扩展，不进入 P1；若未来拆成 feature，可以用 feature 控制。
3. 即使保留 `protocol` 或 `kernel` 依赖，UI reducer 也不能消费 `protocol::Event`，prompt/session/approval 交互仍必须走 ACP。

保留：

```toml
anyhow
clap
crossterm
futures
ratatui
serde_json
tokio
tokio-stream
tracing
tracing-subscriber
typed-builder
unicode-width
```

如果 workspace root 尚未把 `agent-client-protocol` 的 feature 暴露给 TUI，可按 crate-local dependency 写 features。

## 13. 测试策略

UI reducer 测试改用 ACP schema 构造输入：

1. `AgentMessageChunk` 合并 assistant 文本。
2. `AgentThoughtChunk` 合并 reasoning 文本。
3. `ToolCall` 创建 tool view。
4. `ToolCallUpdate` 追加 output/status。
5. `UsageUpdate` 更新 token 总量。
6. `RequestPermissionRequest` 生成 `PendingApproval` 可渲染数据。

Render 测试保留：

1. 小终端 smoke。
2. input/status 不重叠。
3. resize 后核心 UI 可见。
4. transcript 默认 follow bottom。
5. manual scroll 显示历史。
6. tool call 默认折叠。
7. shell `\r` output 归一化。

ACP client/server 测试：

1. request/notification 转 `AppEvent` 的纯转换测试。
2. CLI 参数解析测试。
3. in-process duplex transport 构造测试。
4. P1 不做真实 child process e2e，避免把 P2 的外部进程 transport 带入首版验收。

## 14. 迁移步骤

1. 调整 `crates/tui/Cargo.toml` 依赖：新增 ACP client 依赖，并允许 `acp/kernel/config/provider/tools` 用于默认 in-process ACP server。
2. 改造 `bootstrap.rs` 或新增 `acp_server.rs`，实现与 `crates/acp/src/bin/client.rs` 一致的 in-process ACP server 启动。
3. 新增 `acp_client.rs`，实现 in-process duplex connect、ACP request、notification/request callback 到 app event channel。
4. 改 `main.rs` CLI：
   - `--list-sessions` 通过 ACP request。
5. 改 `app.rs`：
   - 删除 direct `AgentKernel` 调用。
   - prompt/cancel/load/new/list 走 `AcpClient`。
   - approval 走 responder。
6. 改 `ui::state`：
   - `apply_event` -> `apply_session_update`。
   - UI reducer 输入全部替换成本地 UI 类型或 ACP schema 输入。
7. 修正 `ui::approval`，从 ACP permission request 生成 overlay。
8. 保留并修正 `ui::render` / `ui::view` 已有滚动折叠能力。
9. 运行验证：
   - `cargo test -p tui`
   - `cargo clippy -p tui --all-targets -- -D warnings`
   - `cargo run -p tui -- --list-sessions`

## 15. 验收标准

1. `ui::state` 和 `ui::render` 不消费 `protocol::Event`。
2. prompt/session/approval 主路径不直接调用 `AgentKernel`，而是通过 ACP request/notification 完成；即使 server 是 in-process 启动，也必须经过 ACP client/server 连接。
3. TUI 能通过 ACP `NewSessionRequest` 创建 session。
4. TUI 能通过 ACP `LoadSessionRequest` 恢复 session，并显示 replay history。
5. TUI 能通过 ACP `ListSessionsRequest` 列出 session。
6. TUI prompt 输出完全来自 ACP `SessionNotification`。
7. Approval 通过 ACP `RequestPermissionRequest`/`RequestPermissionResponse` 完成。
8. 当前 UI 能力不回退：滚动、折叠、状态栏、shell output 归一化继续可用。
9. `cargo test -p tui` 通过。
10. `cargo clippy -p tui --all-targets -- -D warnings` 通过。

## 16. 风险与处理

### 16.1 ACP notification 与 prompt response 生命周期

`PromptRequest` 在 ACP server 端会在 streaming 结束后才返回 `PromptResponse`。TUI 需要同时处理请求进行中的 notifications 和最终 response。

处理方式：`acp_client` 将 prompt request 放到 async task，notification handler 通过 channel 发 `AppEvent`，最终 response 也通过 channel 发 `PromptFinished`。

### 16.2 Permission responder 生命周期

如果 user 在 approval overlay 出现时退出 TUI，responder 不能悬挂。

处理方式：退出前对 pending permission 统一响应 reject，或者 drop 时由 ACP client 关闭 connection，让 server 侧 request 失败并转 reject。第一版优先显式 reject。

### 16.3 in-process server 与 UI 边界混淆

允许 TUI 依赖 `kernel` 来启动 in-process ACP server 后，最容易退回 direct-kernel UI。

处理方式：把 in-process server bootstrap 放在 `acp_server.rs` / `bootstrap.rs`，只返回 ACP client transport 或 connection；`ui::state`、`ui::render`、`app.rs` 的 prompt/session/approval 主路径仍然只处理 ACP schema 和 ACP requests。

### 16.4 P2 外部 ACP transport 扩展

外部 ACP server 进程 transport 不是 P1 需求，不应增加首版实现和测试范围。

处理方式：P1 只实现 in-process server。P2 如需外部 transport，再新增 `--acp-command` / `CLAW_ACP_COMMAND` 或等价配置，并补充 child process e2e。

### 16.5 ACP schema 信息损失

ACP `UsageUpdate` 当前是 total/subtotal 形式，内部 input/output split 可能不可见。

处理方式：TUI 底部状态栏第一版只显示 total tokens。如果后续需要 input/output split，应扩展 ACP adapter 输出，而不是让 TUI 读 internal event。

## 17. 自检

1. 没有把 Cargo 依赖误写成硬隔离边界。
2. 没有要求 TUI 消费 `protocol::Event`。
3. 没有允许 UI reducer 绕过 ACP 直接消费 kernel event。
4. 允许 TUI 直接依赖 `acp` 模块并 in-process 启动 ACP server，但不允许 UI 绕过 ACP。
5. 迁移步骤先建立 ACP UI 输入模型再改 app loop，避免半解耦。
6. 保留当前 UI 交互成果，不把滚动/折叠回退掉。

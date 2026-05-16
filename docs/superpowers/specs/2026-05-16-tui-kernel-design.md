# TUI 直连 Kernel 设计方案

**日期**: 2026-05-16
**状态**: 待用户审核

参考实现:

- `/home/isbest/Documents/WorkSpace/codex/codex-rs/tui`
- `/home/isbest/Documents/WorkSpace/codex/codex-rs/tui/src/app.rs`
- `/home/isbest/Documents/WorkSpace/codex/codex-rs/tui/src/tui.rs`
- `/home/isbest/Documents/WorkSpace/codex/codex-rs/tui/src/tui/event_stream.rs`

---

## 1. 背景

当前 `clawcode` 已经有适合作为 TUI 后端的内部协议边界：

1. `protocol`: 定义 `AgentKernel`、`Event`、`Op`、`SessionCreated`、`SessionInfo`、`ReviewDecision` 等前后端通信类型。
2. `kernel`: 实现 `protocol::AgentKernel`，负责 session、LLM、tool、approval、subagent、persistence。
3. `acp`: 作为外部协议 adapter，将内部 `protocol::Event` 转换成 ACP `SessionUpdate`，用于 Zed 或其他 ACP client。

最新设计方向是：TUI 直接与 `kernel` 交互，不再通过 ACP。

```text
TUI <-> protocol::AgentKernel <-> Kernel

ACP client <-> acp::ClawcodeAgent <-> Kernel
```

因此，TUI 与 ACP 是两个并行 frontend。它们共享 `kernel` 和 `protocol`，但 TUI 不消费 ACP schema，ACP 也不承担 TUI 的中间层职责。

Codex 的 TUI 参考实现使用 `ratatui + crossterm`：

- `ratatui`: 布局、buffer、widgets、文本渲染。
- `crossterm`: raw mode、alternate screen、bracketed paste、keyboard/resize/focus event stream。
- Codex 当前还通过 `[patch.crates-io]` 使用 fork 版 `ratatui` 与 `crossterm`；本项目第一版不跟随 fork。

---

## 2. 目标

1. 新增一个可交互 TUI，直接调用 `protocol::AgentKernel`。
2. TUI 消费内部 `protocol::Event`，不通过 ACP `SessionNotification`。
3. TUI 支持新建 session、恢复 session、列出 session、发送 prompt、取消 turn、关闭 session。
4. TUI 支持显示 assistant message、reasoning、tool call、tool output、plan、usage、subagent 状态。
5. TUI 支持处理 kernel 发出的 approval request，并通过 `resolve_approval()` 回传决策。
6. 第一版使用 `ratatui + crossterm`，实现稳定的 terminal restore 与 redraw loop。
7. 保留现有 ACP adapter，不破坏 Zed 或其他 ACP client 对接。

---

## 3. 非目标

1. 不重写 `kernel` 的 session runtime。
2. 不让 ACP 成为 TUI 的中间层。
3. 不在第一版实现 Codex 完整 TUI 功能，例如复杂 resume picker、agent tree 可视化、外部编辑器、图片预览、语音输入、插件市场 UI。
4. 不引入 Codex fork 版 `ratatui` / `crossterm`。
5. 不改变 session persistence 文件格式。
6. 不在第一版设计 remote TUI 或网络 transport。

---

## 4. 方案比较

### 4.1 方案 A: TUI 通过 ACP 交互

TUI 作为 ACP client，通过 `ClawcodeAgent` 间接访问 kernel。

优点：

- TUI 与外部 ACP client 使用同一协议表面。
- 不直接依赖 kernel/provider/tools/config。

缺点：

- TUI 会被 ACP schema 限制，内部事件信息需要先压到 ACP，再从 ACP 还原为 UI state。
- 本项目已经有 `protocol::Event`，TUI 绕 ACP 会增加一层不必要转换。
- Approval、tool、plan、usage 的 UI 语义更接近内部事件，ACP 更适合外部 adapter。

结论：不采用。

### 4.2 方案 B: TUI 直接调用 `AgentKernel`

TUI 构造 `Kernel`，调用 `new_session()` / `load_session()` / `prompt()` / `resolve_approval()`，直接消费 `protocol::EventStream`。

优点：

- 路径短，事件语义完整。
- 与现有 `protocol` crate 的设计目标一致：`AgentKernel` 被 frontend protocol adapters 消费。
- ACP 与 TUI 可以独立演进，ACP 专注外部协议，TUI 专注本地交互体验。
- TUI 可以直接利用内部 `Event` 的 tool delta、agent status、permission request。

缺点：

- TUI crate 需要依赖 `kernel`，并承担 kernel bootstrap 逻辑。
- 若 ACP 和 TUI 都各自构造 kernel，可能出现启动代码重复。

结论：第一阶段采用。

### 4.3 方案 C: 抽出 shared app bootstrap crate

新增一个 shared runtime/bootstrap crate，由 ACP 和 TUI 共用 kernel 构造。

优点：

- 避免 ACP main 与 TUI main 重复构造 config/provider/tools/kernel。
- 将应用启动边界集中管理。

缺点：

- 第一版会引入额外 crate 边界。
- 现有启动逻辑很短，提前抽象可能增加实现面。

结论：暂不作为 Phase 1 必需项。若实现时发现重复扩散，再抽出 shared bootstrap。

---

## 5. 选定设计

第一阶段采用方案 B：TUI 直接调用 `AgentKernel`。

整体数据流：

```text
User key input
  -> TUI composer
  -> App submits prompt
  -> Kernel::prompt(session_id, text)
  -> protocol::EventStream
  -> TUI state update
  -> ratatui render

Approval overlay
  -> user selects allow/reject
  -> Kernel::resolve_approval(session_id, call_id, decision)
```

crate 关系：

```text
crates/tui
  -> protocol
  -> kernel
  -> config
  -> provider
  -> tools
  -> ratatui
  -> crossterm
  -> tokio

crates/acp
  -> protocol
  -> kernel
  -> provider
  -> config
  -> tools
  -> agent-client-protocol
```

TUI 与 ACP 并行依赖 `kernel`。`protocol` 继续作为两者共同的内部 event/op/type 层。

---

## 6. Kernel 启动边界

当前 `crates/acp/src/main.rs` 和 `crates/acp/src/bin/client.rs` 都包含类似启动逻辑：

```text
config::load()
LlmFactory::new(config)
ToolRegistry::new()
tools.register_builtins()
Kernel::new(...)
kernel.register_agent_tools()
```

Phase 1 可以在 `crates/tui` 内部实现同样的 bootstrap，保持变更局部：

```rust
/// Build the kernel used by the local TUI session.
async fn build_kernel() -> anyhow::Result<Arc<Kernel>>;
```

该函数只存在于 TUI crate 内部，职责：

1. 加载 config。
2. 构造 `LlmFactory`。
3. 构造 `ToolRegistry` 并注册 builtin tools。
4. 构造 `Kernel`。
5. 调用 `kernel.register_agent_tools()`。

如果后续 ACP 与 TUI 的启动逻辑继续重复，可将该函数上移到一个 shared crate 或 `kernel` 提供的 app bootstrap helper。第一版不提前抽象。

---

## 7. TUI crate 结构

建议新增：

```text
crates/tui/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── lib.rs
    ├── app.rs
    ├── bootstrap.rs
    ├── event.rs
    ├── terminal.rs
    ├── render.rs
    ├── state.rs
    ├── composer.rs
    └── approval.rs
```

### 7.1 `main.rs`

职责：

- 解析 CLI 参数。
- 初始化 tracing。
- 处理 `--list-sessions` 的非 TUI 输出路径。
- 初始化 terminal guard。
- 构造 kernel。
- 调用 `App::run()`。

第一版 CLI 参数：

```text
claw-tui [--resume <SESSION_ID>] [--list-sessions] [--no-alt-screen]
```

`--list-sessions` 直接调用 `kernel.list_sessions()` 并在普通 stdout 输出，不进入 raw mode。

### 7.2 `bootstrap.rs`

职责：

- 构造 `Arc<Kernel>`。
- 隐藏 config/provider/tools/bootstrap 细节。
- 保持 `main.rs` 简短。

所有新增函数必须有英文函数级注释。

### 7.3 `terminal.rs`

职责：

- `enable_raw_mode()` / `disable_raw_mode()`。
- `EnterAlternateScreen` / `LeaveAlternateScreen`。
- `EnableBracketedPaste` / `DisableBracketedPaste`。
- panic/drop 时恢复 terminal。
- 提供 `TerminalGuard`。

第一版使用 `ratatui::Terminal<CrosstermBackend<Stdout>>`，不复制 Codex 的 custom terminal。

### 7.4 `event.rs`

定义 TUI 内部事件：

```rust
pub enum TuiEvent {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Tick,
}

pub enum AppEvent {
    Terminal(TuiEvent),
    Kernel(Event),
    PromptFinished(StopReason),
    PromptFailed(String),
    Error(String),
}
```

事件来源：

- `crossterm::event::EventStream`
- `protocol::EventStream`
- prompt task result
- periodic tick

第一版可以不实现 Codex 的 `EventBroker` pause/resume。后续如果支持外部编辑器或交互式子进程，再引入 pause/resume 机制。

### 7.5 `app.rs`

职责：

- 持有 `Arc<dyn AgentKernel>` 或 `Arc<Kernel>`。
- 启动或恢复 session。
- 管理 app event loop。
- 将用户输入提交为 `kernel.prompt()`。
- 将 kernel events 写入 `AppState`。
- 调用 render。

主循环形态：

```text
tokio::select!
  terminal_event = terminal_events.next()
  app_event = app_event_rx.recv()
  tick = interval.tick()
```

提交 prompt 时：

1. 从 composer 取出文本。
2. 创建 user transcript cell。
3. 调用 `kernel.prompt(&session_id, text)`。
4. spawn task 消费返回的 `EventStream`。
5. 每个 event 发送到 app event channel。
6. 收到 `TurnComplete` 后清理 running 状态。

### 7.6 `state.rs`

保存可渲染状态：

```text
AppState
├── session_id
├── cwd
├── model_label
├── transcript
├── composer_text
├── running_prompt
├── tool_calls
├── pending_approval
├── plan
├── usage
├── top_status_line
├── bottom_status_line
└── error_banner
```

`transcript` 是 TUI 展示状态，不是持久化 source of truth。session 持久化仍由 kernel/store 负责。

### 7.7 `composer.rs`

第一版实现轻量 composer：

- 普通字符插入。
- Backspace/Delete。
- Left/Right/Home/End。
- Enter 提交。
- `Ctrl+J` 插入换行。
- Bracketed paste 直接插入文本。

不引入 Codex 的自研 textarea。第一版优先保持输入可用、代码可控。

### 7.8 `approval.rs`

职责：

- 保存 pending approval request。
- 渲染 approval overlay。
- 将用户按键映射为 `ReviewDecision`。

支持事件：

- `Event::ExecApprovalRequested`
- `Event::PermissionRequested`

第一版可以先统一映射为：

- `a` / `y`: `ReviewDecision::AllowOnce`
- `r` / `n` / `Esc`: `ReviewDecision::RejectOnce`

如果后续需要 `AllowAlways` / `RejectAlways`，再扩展快捷键。

### 7.9 `render.rs`

使用 `ratatui` 渲染：

```text
┌──────────────── transcript ────────────────┐
│ user / assistant / reasoning / tools        │
│ plan / usage / errors                       │
├──────────────── status ────────────────────┤
│ turn state / approval state / error summary │
├──────────────── composer ──────────────────┤
│ > user input                                │
├────────────── runtime status ───────────────┤
│ model: deepseek/deepseek-v4 | cwd: ... | tok│
└──────────────── footer ────────────────────┘
```

第一版布局：

- transcript 占剩余高度。
- top status line 1 行，展示 turn/approval/error 等当前交互状态。
- composer 1 到 6 行。
- bottom runtime status line 1 行，展示模型、目录和 token 用量。
- footer 1 行快捷键提示。

bottom runtime status line 是固定区域，位于 input 下方。它的内容包括：

- `model`: 当前 provider/model，例如 `deepseek/deepseek-v4-flash`。
- `cwd`: 当前 session 工作目录，宽度不足时中间省略。
- `tokens`: 最近一次 `UsageUpdate` 的输入、输出和总 token。

模型信息优先来自 `SessionCreated.models` 与当前 config 的 active model。若第一版无法稳定取得 display name，则先显示 provider/model id。目录来自 session cwd。token 用量来自 `Event::UsageUpdate`，没有数据时显示 `tokens: -`。

approval overlay：

```text
Approve tool execution?
tool: shell
args: {...}

[a] allow once   [r] reject
```

---

## 8. Kernel Event 映射规则

### 8.1 Session 生命周期

启动：

- 无 `--resume`: 调用 `kernel.new_session(cwd)`。
- 有 `--resume`: 调用 `kernel.load_session(&session_id)`。
- `--list-sessions`: 调用 `kernel.list_sessions(Some(cwd), None)`，输出后退出。

退出：

- 正常退出时调用 `kernel.close_session(&session_id)`。
- 如果当前 prompt running，先调用 `kernel.cancel(&session_id)`，再 close。

### 8.2 Prompt

用户按 Enter：

1. composer 文本追加为 user transcript cell。
2. 调用 `kernel.prompt(&session_id, text)`。
3. 设置 `running_prompt = true`。
4. prompt task 消费 `EventStream`。
5. 收到 `Event::TurnComplete` 后设置 `running_prompt = false`。

第一版不支持图片和多 block 输入。

### 8.3 Event 映射

TUI 消费以下 `protocol::Event`：

- `AgentMessageChunk`: append 到当前 assistant streaming cell。
- `AgentThoughtChunk`: append 到 reasoning cell，可默认 dim 显示。
- `ToolCallDelta`: 更新 pending tool call name/arguments prefix。
- `ToolCall`: 创建或替换 tool call entry。
- `ToolCallUpdate`: 追加 output/status。
- `PlanUpdate`: 替换当前 plan 展示。
- `UsageUpdate`: 更新 token usage。
- `PermissionRequested`: 打开 permission overlay。
- `ExecApprovalRequested`: 打开 approval overlay。
- `AgentStatusChange`: 更新 agent 状态提示。
- `AgentSpawned`: 追加 agent spawned system cell。
- `TurnComplete`: 结束 running 状态。

未知或后续新增 event：

- 记录 debug log。
- 不使 TUI 崩溃。

### 8.4 Approval

收到 `ExecApprovalRequested` 或 `PermissionRequested`：

1. 保存 pending approval。
2. 打开 overlay。
3. 用户选择后调用 `kernel.resolve_approval(&session_id, &call_id, decision)`。
4. 关闭 overlay。

注意：当前 `PermissionRequested` 与 `ExecApprovalRequested` 的结构不同。实现时需要统一出 TUI 内部 `PendingApproval`，以便 overlay 不关心来源 event。

### 8.5 Cancel

Ctrl+C 行为：

- 如果有 pending approval：调用 `resolve_approval(..., RejectOnce)` 并关闭 overlay。
- 如果 prompt running：调用 `kernel.cancel(&session_id)`。
- 如果 composer 有文本：清空 composer。
- 否则退出 TUI。

---

## 9. 错误处理

### 9.1 Terminal 错误

- raw mode 初始化失败：返回错误，不进入 TUI。
- render 失败：恢复 terminal 后返回错误。
- panic：通过 guard 尽力恢复 raw mode 与 alternate screen。

### 9.2 Kernel 错误

- session 创建失败：恢复 terminal 后返回错误。
- prompt 返回错误：显示 error banner，`running_prompt = false`。
- event stream 返回 `KernelError::Cancelled`：显示 cancelled 状态。
- `resolve_approval()` 失败：显示 error banner，清理 pending overlay。

### 9.3 Channel 错误

- prompt task 向 app event channel 发送失败：记录 tracing，结束 task。
- terminal event stream 关闭：恢复 terminal 并退出。

---

## 10. 测试策略

### 10.1 单元测试

覆盖：

- composer 编辑行为。
- `protocol::Event` 到 `AppState` 的映射。
- approval event 到 `PendingApproval` 的归一化。
- Ctrl+C 状态机。
- render 在小终端尺寸下不 panic。

### 10.2 集成测试

新增 fake kernel 测试，不调用真实 LLM：

- fake `AgentKernel::new_session()` 返回固定 session。
- fake `AgentKernel::prompt()` 返回预置 `EventStream`。
- 注入 `AgentMessageChunk`，验证 transcript 更新。
- 注入 `ToolCall` + `ToolCallUpdate`，验证 tool 状态。
- 注入 `ExecApprovalRequested`，模拟用户选择，验证 `resolve_approval()` 被调用。

### 10.3 手工验证

第一阶段手工验证命令：

```bash
cargo run -p tui
cargo run -p tui -- --list-sessions
cargo run -p tui -- --resume <SESSION_ID>
```

验证点：

- terminal 退出后 raw mode 恢复。
- 流式文本不会覆盖 composer。
- tool call 状态可见。
- approval overlay 可响应。
- Ctrl+C 不会留下悬挂 turn。

---

## 11. 分阶段实现

### Phase 1: MVP TUI

1. 新建 `crates/tui`。
2. 在 workspace 注册 `tui` crate。
3. 实现 TUI 内部 kernel bootstrap。
4. 实现 terminal guard。
5. 实现 app event loop。
6. 实现基础 composer 与 transcript render。
7. 接通 `new_session`、`load_session`、`list_sessions`、`prompt`、`cancel`、`close_session`。
8. 接通 kernel event 映射与 approval overlay。

验收标准：

- 能启动 TUI 并完成一轮 prompt。
- 能显示 assistant 流式输出。
- 能显示 tool call 和 approval。
- 退出后终端状态正常。

### Phase 2: UX 完善

1. 增加 resume picker。
2. 增加 model/mode 切换交互。
3. 增加 transcript scrollback。
4. 增加更完整的 multiline composer。
5. 增加更完整的 tool output folding。

### Phase 3: Bootstrap 收敛

1. 如果 ACP 与 TUI 的 kernel 构造重复变多，抽出 shared bootstrap。
2. 保持 ACP 与 TUI 是并行 frontend，而不是互相依赖。

---

## 12. 依赖选择

第一版 workspace dependencies 增加：

```toml
ratatui = "0.29"
crossterm = "0.28"
tokio-stream = "0.1"
unicode-width = "0.2"
```

`crates/tui/Cargo.toml` 使用：

```toml
protocol = { path = "../protocol" }
kernel = { path = "../kernel" }
config = { path = "../config" }
provider = { path = "../provider" }
tools = { path = "../tools" }

crossterm = { workspace = true, features = ["bracketed-paste", "event-stream"] }
ratatui = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std", "signal"] }
tokio-stream = { workspace = true }
```

暂不引入：

- `ratatui-macros`
- `syntect`
- `arboard`
- image rendering crates
- Codex fork dependencies

---

## 13. 设计约束

1. TUI 直接依赖 `kernel` 并调用 `protocol::AgentKernel`。
2. TUI 直接消费 `protocol::Event`。
3. ACP 保持外部协议 adapter，不作为 TUI 中间层。
4. TUI 的展示状态不作为持久化 source of truth。
5. 新增或修改的 Rust 函数必须有英文函数级注释。
6. 新增或修改的非平凡 Rust 逻辑必须有英文注释。
7. 超过 3 个字段的 struct 必须使用 `typed-builder`。
8. 克隆 `Arc<T>` 字段时使用 `Arc::clone(&field)`。
9. 不经用户明确许可不提交 commit。

---

## 14. 未纳入第一版的明确后续项

1. 外部编辑器集成。
2. Codex 风格 inline viewport。
3. 完整 vim/emacs composer keymap。
4. 图片/附件输入。
5. 多 agent tree UI。
6. remote TUI / ACP stdio bridge 模式。
7. 高保真 terminal snapshot 测试。

这些后续项不阻塞第一版 TUI 直连 Kernel。

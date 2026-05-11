# 协议层设计方案

## 目标

为 clawcode 项目设计内部协议层（`clawcode-protocol` crate），并新建 ACP 桥接层（`clawcode-acp` crate），实现类似 Codex 的 agent，最终支持 ACP 协议对接 Zed 编辑器 UI 渲染。

## 架构

```
clawcode-protocol   ← 最底层，纯数据类型，无外部重依赖
clawcode-provider   ← 依赖 protocol（Message 等共享类型上移至此）
clawcode-kernel     ← 依赖 protocol + provider
clawcode-acp        ← 依赖 protocol + kernel + agent-client-protocol
clawcode-config     ← 不变
```

依赖方向：都是单向向下，上层依赖下层，无循环依赖。

### Crate 层级

```
crates/
├── protocol/          # clawcode-protocol — 内部协议，纯数据类型
│   └── src/
│       ├── lib.rs     # 模块声明 + 统一导出 + provider 类型 re-export
│       ├── session.rs # SessionId, SessionInfo, SessionListPage, SessionCreated
│       ├── tool.rs    # ToolDefinition, ToolCallStatus
│       ├── plan.rs    # Plan, PlanEntry, PlanPriority, PlanStatus
│       ├── permission.rs # PermissionRequest, PermissionOption, PermissionOptionKind
│       ├── agent.rs   # AgentPath, AgentStatus, InterAgentMessage
│       ├── config.rs  # SessionMode, ModelInfo, SessionConfigOption
│       ├── event.rs   # 内核→前端 流式事件 Event 枚举
│       ├── op.rs      # 前端→内核 操作指令 Op 枚举
│       ├── kernel.rs  # AgentKernel trait, KernelError, EventStream
│       └── message.rs # Message 等共享类型（从 provider 上移）
│
├── kernel/            # clawcode-kernel — agent 循环、工具执行
│   └── src/
│       └── lib.rs     # Kernel 结构体，实现 AgentKernel trait
│
├── acp/               # clawcode-acp（新建）— ACP 桥接层
│   └── src/
│       ├── lib.rs     # 入口，run() 函数，stdio 传输
│       ├── agent.rs   # ClawcodeAgent 结构体 + ACP handler 注册
│       └── translate.rs # 内部类型 → ACP 类型的 From 实现
│
├── config/            # 已有，保持不变
└── provider/          # 已有，调整为依赖 protocol
```

## 核心类型定义

### 消息类型（位于 protocol crate，从 provider 上移）

这些类型原在 `provider::completion::message`，现上移至 protocol，使 protocol 无需依赖 provider。

```rust
// Message, UserContent, AssistantContent, ToolCall, ToolFunction,
// ToolResult, ToolResultContent, Text, Image, Reasoning, Role, ImageMediaType, ...
```

provider crate 改为依赖 protocol 并 re-export 这些类型，保持向后兼容。

### session.rs — 会话

```rust
/// Unique session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

/// Summary info for a session in listing results.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    #[builder(default)]
    pub title: Option<String>,
    #[builder(default)]
    pub updated_at: Option<String>,
}

/// Data returned after creating or loading a session.
#[derive(Debug, Clone, TypedBuilder)]
pub struct SessionCreated {
    pub session_id: SessionId,
    pub modes: Vec<SessionMode>,
    pub models: Vec<ModelInfo>,
}

/// Paginated session list result.
#[derive(Debug, Clone, TypedBuilder)]
pub struct SessionListPage {
    pub sessions: Vec<SessionInfo>,
    #[builder(default)]
    pub next_cursor: Option<String>,
}
```

### tool.rs — 工具定义与状态

```rust
/// Tool definition registered with the agent.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Execution status of a tool call within the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
```

### plan.rs — 计划/进度

```rust
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct PlanEntry {
    pub name: String,
    pub priority: PlanPriority,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority { Low, Medium, High }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus { Pending, InProgress, Completed }
```

### permission.rs — 权限请求

```rust
/// Permission request sent from kernel to frontend.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct PermissionRequest {
    pub call_id: String,
    pub message: String,
    pub options: Vec<PermissionOption>,
}

/// A single permission option the user can choose.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}
```

### agent.rs — 多 Agent 身份

```rust
/// Hierarchical agent path, e.g. `/root/explorer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentPath(pub String);

/// Runtime status of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Interrupted,
    Completed { message: Option<String> },
    Errored { reason: String },
    Shutdown,
}

/// Message sent between agents.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct InterAgentMessage {
    pub from: AgentPath,
    pub to: AgentPath,
    pub content: String,
    #[builder(default)]
    pub trigger_turn: bool,
}
```

### config.rs — 会话配置

```rust
/// A session mode preset (e.g. read-only, auto, full-access).
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[builder(default)]
    pub description: Option<String>,
}

/// Model info exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    #[builder(default)]
    pub description: Option<String>,
    #[builder(default)]
    pub context_tokens: Option<u64>,
    #[builder(default)]
    pub max_output_tokens: Option<u64>,
}

/// A configurable option for a session.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[builder(default)]
    pub description: Option<String>,
    pub values: Vec<SessionConfigValue>,
    #[builder(default)]
    pub current_value: Option<String>,
}

/// A possible value for a config option.
#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct SessionConfigValue {
    pub id: String,
    pub label: String,
}
```

## 事件系统

### op.rs — 操作指令（前端/客户端 → 内核）

```rust
/// Operation submitted from the frontend / client to the kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    NewSession {
        cwd: PathBuf,
    },
    LoadSession {
        session_id: SessionId,
    },
    Prompt {
        session_id: SessionId,
        message: Message,
    },
    Cancel {
        session_id: SessionId,
    },
    SetMode {
        session_id: SessionId,
        mode: String,
    },
    SetModel {
        session_id: SessionId,
        provider_id: String,
        model_id: String,
    },
    CloseSession {
        session_id: SessionId,
    },
    SpawnAgent {
        parent_session: SessionId,
        agent_path: AgentPath,
        role: String,
        prompt: String,
    },
    InterAgentMessage {
        from: AgentPath,
        to: AgentPath,
        content: String,
    },
}
```

### event.rs — 流式事件（内核 → 前端）

```rust
/// Streaming event emitted from the kernel to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Text delta from the assistant.
    AgentMessageChunk {
        session_id: SessionId,
        text: String,
    },
    /// Reasoning / thinking delta from the assistant.
    AgentThoughtChunk {
        session_id: SessionId,
        text: String,
    },
    /// A tool call was initiated or updated.
    ToolCall {
        session_id: SessionId,
        agent_path: AgentPath,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
        status: ToolCallStatus,
    },
    /// Incremental update to an active tool call.
    ToolCallUpdate {
        session_id: SessionId,
        call_id: String,
        output_delta: Option<String>,
        status: Option<ToolCallStatus>,
    },
    /// The plan / task list was updated.
    PlanUpdate {
        session_id: SessionId,
        entries: Vec<PlanEntry>,
    },
    /// Token usage information.
    UsageUpdate {
        session_id: SessionId,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The kernel requests user permission.
    PermissionRequested {
        session_id: SessionId,
        request: PermissionRequest,
    },
    /// A sub-agent's status changed.
    AgentStatusChange {
        session_id: SessionId,
        agent_path: AgentPath,
        status: AgentStatus,
    },
    /// The current turn has completed.
    TurnComplete {
        session_id: SessionId,
        stop_reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    Cancelled,
    Error,
}
```

## Kernel Trait 接口

### kernel.rs

```rust
/// Boxed, pinned stream of kernel events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>;

/// Central agent kernel trait.
/// Implemented by the kernel crate, consumed by ACP and other frontend adapters.
#[async_trait]
pub trait AgentKernel: Send + Sync {
    /// Create a new session and return its ID plus available config.
    async fn new_session(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerConfig>,
    ) -> Result<SessionCreated, KernelError>;

    /// Load a previously persisted session.
    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionCreated, KernelError>;

    /// List persisted sessions with optional cwd filter and cursor pagination.
    async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError>;

    /// Submit a user prompt, returning a stream of events.
    async fn prompt(
        &self,
        session_id: &SessionId,
        message: Message,
    ) -> Result<EventStream, KernelError>;

    /// Cancel the currently running turn.
    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Set the session approval/sandboxing mode.
    async fn set_mode(
        &self,
        session_id: &SessionId,
        mode: &str,
    ) -> Result<(), KernelError>;

    /// Switch the model for the session.
    async fn set_model(
        &self,
        session_id: &SessionId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), KernelError>;

    /// Close a session and release its resources.
    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Spawn a sub-agent.
    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        agent_path: AgentPath,
        role: &str,
        prompt: &str,
    ) -> Result<(), KernelError>;
}

/// Error type for kernel operations.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    #[error("agent not found: {0}")]
    AgentNotFound(AgentPath),

    #[error("authentication required")]
    AuthRequired,

    #[error("operation cancelled")]
    Cancelled,

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}
```

## ACP 桥接层

### agent.rs — ClawcodeAgent

```rust
/// ACP Agent implementation that bridges ACP protocol with the clawcode kernel.
pub struct ClawcodeAgent {
    kernel: Arc<dyn AgentKernel>,
    llm_factory: Arc<LlmFactory>,
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
    active_sessions: Arc<Mutex<HashMap<SessionId, SessionHandle>>>,
}
```

Handler 注册使用 `Agent.builder()` 模式，涵盖以下 ACP 方法：

| ACP 方法 | 行为 |
|---|---|
| `initialize` | 返回 agent 能力、auth 方法、版本信息 |
| `authenticate` | 验证 API key |
| `new_session` | 调用 `kernel.new_session()`，返回 modes/models/config_options |
| `load_session` | 调用 `kernel.load_session()`，回放历史事件 |
| `list_sessions` | 调用 `kernel.list_sessions()`，游标分页 |
| `prompt` | 调用 `kernel.prompt()`，将 EventStream 翻译为 ACP SessionNotification |
| `cancel` | 调用 `kernel.cancel()` |
| `set_session_mode` | 调用 `kernel.set_mode()` |
| `set_session_model` | 调用 `kernel.set_model()` |
| `set_session_config_option` | 处理推理深度等配置项 |
| `close_session` | 调用 `kernel.close_session()` |
| `cancel` (notification) | 中断当前回合 |

### translate.rs — 类型转换

内部类型到 ACP 类型的转换使用 `From` trait，通过 move 语义传递，避免内存拷贝：

```rust
// ── StopReason ──
impl From<protocol::StopReason> for acp::schema::StopReason {
    fn from(r: protocol::StopReason) -> Self {
        match r {
            protocol::StopReason::EndTurn => Self::EndTurn,
            protocol::StopReason::Cancelled => Self::Cancelled,
            protocol::StopReason::Error => Self::Error,
        }
    }
}

// ── ToolCallStatus ──
impl From<protocol::ToolCallStatus> for acp::schema::ToolCallStatus {
    fn from(s: protocol::ToolCallStatus) -> Self {
        match s {
            protocol::ToolCallStatus::Pending => Self::Pending,
            protocol::ToolCallStatus::InProgress => Self::InProgress,
            protocol::ToolCallStatus::Completed => Self::Completed,
            protocol::ToolCallStatus::Failed => Self::Failed,
        }
    }
}

// ── PlanPriority ──
impl From<protocol::PlanPriority> for acp::schema::PlanEntryPriority {
    fn from(p: protocol::PlanPriority) -> Self {
        match p {
            protocol::PlanPriority::Low => Self::Low,
            protocol::PlanPriority::Medium => Self::Medium,
            protocol::PlanPriority::High => Self::High,
        }
    }
}

// ── PlanStatus ──
impl From<protocol::PlanStatus> for acp::schema::PlanEntryStatus {
    fn from(s: protocol::PlanStatus) -> Self {
        match s {
            protocol::PlanStatus::Pending => Self::Pending,
            protocol::PlanStatus::InProgress => Self::InProgress,
            protocol::PlanStatus::Completed => Self::Completed,
        }
    }
}

// ── PermissionOptionKind ──
impl From<protocol::PermissionOptionKind> for acp::schema::PermissionOptionKind {
    fn from(k: protocol::PermissionOptionKind) -> Self {
        match k {
            protocol::PermissionOptionKind::AllowOnce => Self::AllowOnce,
            protocol::PermissionOptionKind::AllowAlways => Self::AllowAlways,
            protocol::PermissionOptionKind::RejectOnce => Self::RejectOnce,
            protocol::PermissionOptionKind::RejectAlways => Self::RejectAlways,
        }
    }
}
```

### 事件翻译循环

```rust
/// Translate the internal event stream into ACP notifications,
/// sending them to the client until the turn completes.
async fn translate_loop(
    session_id: SessionId,
    mut events: EventStream,
    client: Arc<dyn ClientSender>,
) -> Result<StopReason, Error> {
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| Error::internal_error().data(e.to_string()))?;
        match event {
            Event::AgentMessageChunk { text, .. } => {
                let chunk = ContentChunk::text_block().text(text);
                client.send_notification(SessionUpdate::AgentMessageChunk(chunk))?;
            }
            Event::AgentThoughtChunk { text, .. } => {
                let chunk = ContentChunk::text_block().text(text);
                client.send_notification(SessionUpdate::AgentThoughtChunk(chunk))?;
            }
            Event::ToolCall { call_id, name, arguments, status, agent_path, .. } => {
                let tool_call = ToolCall::new(
                    ToolCallId::new(call_id),
                    name,
                    arguments,
                )
                .status(status.into())
                .location(ToolCallLocation {
                    path: PathBuf::from(agent_path.0),
                });
                client.send_notification(SessionUpdate::ToolCall(tool_call))?;
            }
            Event::ToolCallUpdate { call_id, output_delta, status, .. } => {
                let mut update = ToolCallUpdate::new(ToolCallId::new(call_id));
                if let Some(delta) = output_delta {
                    update = update.content(ToolCallContent::Terminal(Terminal));
                }
                if let Some(s) = status {
                    update = update.status(s.into());
                }
                client.send_notification(SessionUpdate::ToolCallUpdate(update))?;
            }
            Event::PlanUpdate { entries, .. } => {
                let plan_entries: Vec<PlanEntry> = entries
                    .into_iter()
                    .map(|e| PlanEntry::new(e.name)
                        .priority(e.priority.into())
                        .status(e.status.into()))
                    .collect();
                client.send_notification(SessionUpdate::Plan(Plan::new(plan_entries)))?;
            }
            Event::UsageUpdate { input_tokens, output_tokens, .. } => {
                let usage = UsageUpdate::new()
                    .used(input_tokens + output_tokens);
                client.send_notification(SessionUpdate::UsageUpdate(usage))?;
            }
            Event::PermissionRequested { request, .. } => {
                let acp_req = client.request_permission(request.into()).await?;
                // Permission outcome is handled by the kernel via a response channel
            }
            Event::TurnComplete { stop_reason, .. } => {
                return Ok(stop_reason.into());
            }
            _ => {}
        }
    }
    Ok(StopReason::EndTurn)
}
```

### 传输层

```rust
/// Start the ACP agent over stdio transport.
pub async fn run(kernel: Arc<dyn AgentKernel>, llm_factory: Arc<LlmFactory>) -> io::Result<()> {
    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(ClawcodeAgent::new(kernel, llm_factory));
    agent.serve(ByteStreams::new(stdout, stdin)).await
}
```

## 类型化构建器规则

使用 `typed-builder` crate。结构体字段超过 3 个时（即 ≥4），必须使用 `TypedBuilder` derive 宏，通过 builder 模式构造实例。`Option` 字段使用 `#[builder(default)]`。

```rust
// 3 个字段 — 不需要 TypedBuilder
#[derive(Debug, Clone)]
pub struct SessionId(pub String);

// 4 个字段 — 必须使用 TypedBuilder
#[derive(Debug, Clone, TypedBuilder)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    #[builder(default)]
    pub title: Option<String>,
}
```

## ACP 事件映射表

| 内部 Event | ACP 输出 |
|---|---|
| `AgentMessageChunk` | `SessionUpdate::AgentMessageChunk(ContentChunk)` |
| `AgentThoughtChunk` | `SessionUpdate::AgentThoughtChunk(ContentChunk)` |
| `ToolCall` | `SessionUpdate::ToolCall(ToolCall)` |
| `ToolCallUpdate` | `SessionUpdate::ToolCallUpdate(ToolCallUpdate)` |
| `PlanUpdate` | `SessionUpdate::Plan(Plan)` |
| `UsageUpdate` | `SessionUpdate::UsageUpdate(UsageUpdate)` |
| `PermissionRequested` | `RequestPermissionRequest`（发送至客户端并等待响应） |
| `AgentStatusChange` | `SessionUpdate`（自定义扩展或 `AgentMessageChunk` 包装） |
| `TurnComplete` | `PromptResponse.stop_reason` |

## 实施计划概述

1. **将 Message 类型从 provider 上移至 protocol**
   - 在 protocol 新建 `message.rs`，移动 `Message`、`UserContent`、`AssistantContent`、`ToolCall`、`ToolFunction`、`ToolResult` 等类型
   - provider 改为依赖 protocol 并 re-export
2. **实现 protocol crate 核心类型**
   - 按 `session.rs` → `tool.rs` → `plan.rs` → `permission.rs` → `agent.rs` → `config.rs` → `event.rs` → `op.rs` → `kernel.rs` 顺序实现
3. **实现 kernel crate**
   - 实现 `AgentKernel` trait
   - 集成 `LlmFactory` / `ArcLlm` 进行 LLM 调用
   - 实现工具执行循环
4. **新建 acp crate**
   - 依赖 `agent-client-protocol`
   - 实现 `ClawcodeAgent` 和 `translate.rs`
   - stdio 传输入口

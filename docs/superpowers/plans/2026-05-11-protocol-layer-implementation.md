# 协议层实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 实现 clawcode 内部协议层（protocol crate），将共享 Message 类型从 provider 上移至 protocol，实现 kernel 的 AgentKernel trait 基础实现，创建 ACP 桥接 crate。

**架构：** 四阶段。Phase 1 创建 protocol crate 骨架及独立数据类型。Phase 2 将 Message + OneOrMany 从 provider 上移至 protocol，provider 通过 re-export 保持向后兼容。Phase 3 实现 Event、Op、Kernel trait。Phase 4 创建 ACP 桥接 crate 及类型转换。

**技术栈：** Rust 2024 edition, tokio, serde, async-trait, typed-builder, agent-client-protocol, uuid, strum, thiserror

**规格文档：** `docs/superpowers/specs/2026-05-11-protocol-layer-design.md`

---

## 文件清单

| 文件 | 用途 |
|---|---|
| `crates/protocol/Cargo.toml` | 新建 — crate 清单 |
| `crates/protocol/src/lib.rs` | 新建 — 模块声明 + 统一导出 |
| `crates/protocol/src/one_or_many.rs` | 新建 — 从 provider 复制 |
| `crates/protocol/src/message.rs` | 新建 — 从 provider 移动（去 provider 内部依赖） |
| `crates/protocol/src/session.rs` | 新建 — session 类型 |
| `crates/protocol/src/tool.rs` | 新建 — tool 类型 |
| `crates/protocol/src/plan.rs` | 新建 — plan 类型 |
| `crates/protocol/src/permission.rs` | 新建 — permission 类型 |
| `crates/protocol/src/agent.rs` | 新建 — agent 多 Agent 类型 |
| `crates/protocol/src/config.rs` | 新建 — config 配置类型 |
| `crates/protocol/src/event.rs` | 新建 — Event 流式事件 |
| `crates/protocol/src/op.rs` | 新建 — Op 操作指令 |
| `crates/protocol/src/kernel.rs` | 新建 — AgentKernel trait |
| `Cargo.toml`（workspace 根） | 修改 — 添加 typed-builder、uuid、strum、agent-client-protocol、clawcode-protocol |
| `crates/provider/Cargo.toml` | 修改 — 添加 clawcode-protocol 依赖 |
| `crates/provider/src/completion/message.rs` | 修改 — 替换为 re-export |
| `crates/provider/src/one_or_many.rs` | 修改 — 替换为 re-export |
| `crates/provider/src/lib.rs` | 修改 — 更新 re-export 路径，保持 `crate::message` 和 `crate::OneOrMany` 兼容 |
| `crates/kernel/Cargo.toml` | 修改 — 添加依赖 |
| `crates/kernel/src/lib.rs` | 修改 — 替换 stub 为 Kernel struct |
| `crates/acp/Cargo.toml` | 新建 — ACP bridge crate 清单 |
| `crates/acp/src/lib.rs` | 新建 — run() 入口 |
| `crates/acp/src/agent.rs` | 新建 — ClawcodeAgent 结构 + handler 注册 |
| `crates/acp/src/translate.rs` | 新建 — From trait 类型转换 |
| `crates/acp/src/main.rs` | 新建 — 二进制入口 |

---

### 任务 1：添加 workspace 依赖并创建 protocol crate 骨架

**文件：**
- 修改：`Cargo.toml`（workspace 根）
- 创建：`crates/protocol/Cargo.toml`
- 创建：`crates/protocol/src/lib.rs`

- [ ] **步骤1：在 workspace Cargo.toml 添加依赖**

```toml
# builder — 添加到 serde 区域下方
typed-builder = "0.21"

# misc 区域追加
uuid = { version = "1", features = ["v4", "serde"] }
strum = { version = "0.27", features = ["derive"] }
agent-client-protocol = "0.11.1"

# 最后添加 workspace member 依赖
clawcode-protocol = { path = "crates/protocol" }
```

- [ ] **步骤2：创建 protocol crate 的 Cargo.toml**

```toml
[package]
name = "clawcode-protocol"
edition.workspace = true
version.workspace = true
description = "Internal protocol types for clawcode agent-core / frontend communication"

[lib]
name = "protocol"
path = "src/lib.rs"
doctest = false

[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
uuid = { workspace = true }
strum = { workspace = true }
thiserror = { workspace = true }
typed-builder = { workspace = true }
async-trait = { workspace = true }
futures = { workspace = true }
```

- [ ] **步骤3：创建 lib.rs 骨架**

```rust
//! Internal protocol types for clawcode agent-core / frontend communication.
//!
//! Uses a Submission Queue (SQ) / Event Queue (EQ) pattern:
//! - The frontend sends [`Op`] submissions to the kernel.
//! - The kernel streams [`Event`]s back through an event channel.
//!
//! These types are designed so they can be bridged to ACP
//! (`agent-client-protocol`) for IDE-native UI rendering.

pub mod agent;
pub mod config;
pub mod event;
pub mod kernel;
pub mod message;
pub mod one_or_many;
pub mod op;
pub mod permission;
pub mod plan;
pub mod session;
pub mod tool;

pub use agent::*;
pub use config::*;
pub use event::*;
pub use kernel::*;
pub use message::*;
pub use one_or_many::*;
pub use op::*;
pub use permission::*;
pub use plan::*;
pub use session::*;
pub use tool::*;
```

- [ ] **步骤4：构建验证**

```bash
cargo check -p clawcode-protocol
```

预期：报 missing module 错误（后续任务逐个补上），但 Cargo.toml 和 workspace 配置本身正确。

- [ ] **步骤5：提交**

```bash
git add Cargo.toml crates/protocol/
git commit -m "$(cat <<'EOF'
chore(protocol): add protocol crate skeleton and workspace dependencies
EOF
)"
```

---

### 任务 2：实现 session 类型

**文件：**
- 创建：`crates/protocol/src/session.rs`

- [ ] **步骤1：编写 session.rs**

```rust
//! Session identifier and metadata types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique session identifier generated when a new session is created.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Summary info for a session in listing results.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    #[builder(default)]
    pub title: Option<String>,
    #[builder(default)]
    pub updated_at: Option<String>,
}

/// Data returned to the frontend after creating or loading a session.
#[derive(Debug, Clone)]
pub struct SessionCreated {
    pub session_id: SessionId,
    pub modes: Vec<super::config::SessionMode>,
    pub models: Vec<super::config::ModelInfo>,
}

/// Paginated session list result.
#[derive(Debug, Clone)]
pub struct SessionListPage {
    pub sessions: Vec<SessionInfo>,
    pub next_cursor: Option<String>,
}
```

> **注：** `SessionCreated` 只有 3 个字段，不需要 TypedBuilder。`SessionListPage` 只有 2 个字段，也不需要用 TypedBuilder。`SessionInfo` 有 4 个字段（≥4），必须使用 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

预期：session 模块编译通过，报错仅限于其他尚未创建的模块。

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/session.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add session identifier and metadata types
EOF
)"
```

---

### 任务 3：实现 tool 类型

**文件：**
- 创建：`crates/protocol/src/tool.rs`

- [ ] **步骤1：编写 tool.rs**

```rust
//! Tool definition and execution status types.

use serde::{Deserialize, Serialize};

/// Tool definition registered with the agent kernel.
///
/// Describes a callable tool the LLM can invoke via function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name exposed to the model.
    pub name: String,
    /// Human-readable description sent to the model.
    pub description: String,
    /// JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// Execution status of a tool call within the agent kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Tool call requested but execution has not started.
    Pending,
    /// Tool is currently executing.
    InProgress,
    /// Tool execution completed successfully.
    Completed,
    /// Tool execution failed with an error.
    Failed,
}
```

> **注：** `ToolDefinition` 只有 3 个字段，不需要 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/tool.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add tool definition and execution status types
EOF
)"
```

---

### 任务 4：实现 plan 类型

**文件：**
- 创建：`crates/protocol/src/plan.rs`

- [ ] **步骤1：编写 plan.rs**

```rust
//! Plan and task-progress types for structured agent output.

use serde::{Deserialize, Serialize};

/// A single entry in the agent's execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Human-readable task name.
    pub name: String,
    /// Priority level for this task.
    pub priority: PlanPriority,
    /// Current execution status.
    pub status: PlanStatus,
}

/// Priority level for a plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority { Low, Medium, High }

/// Execution status of a plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus { Pending, InProgress, Completed }
```

> **注：** `PlanEntry` 只有 3 个字段，不需要 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add plan and task-progress types
EOF
)"
```

---

### 任务 5：实现 permission 类型

**文件：**
- 创建：`crates/protocol/src/permission.rs`

- [ ] **步骤1：编写 permission.rs**

```rust
//! Permission request types for tool execution approval.

use serde::{Deserialize, Serialize};

/// Permission request sent from the kernel to the frontend
/// when a tool execution needs user approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Identifies the tool call this permission is for.
    pub call_id: String,
    /// Human-readable message explaining what needs approval.
    pub message: String,
    /// Available permission choices for the user.
    pub options: Vec<PermissionOption>,
}

/// A single permission option the user can choose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Unique option identifier (e.g. "allow_once").
    pub id: String,
    /// Human-readable label (e.g. "Allow Once").
    pub label: String,
    /// The kind of this option determining its scope.
    pub kind: PermissionOptionKind,
}

/// Classification of a permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}
```

> **注：** `PermissionRequest` 和 `PermissionOption` 都只有 3 个字段，不需要 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/permission.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add permission request types
EOF
)"
```

---

### 任务 6：实现 agent 多 Agent 类型

**文件：**
- 创建：`crates/protocol/src/agent.rs`

- [ ] **步骤1：编写 agent.rs**

```rust
//! Multi-agent identity, status, and inter-agent messaging types.

use serde::{Deserialize, Serialize};

/// Hierarchical agent path, e.g. `/root/explorer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentPath(pub String);

impl std::fmt::Display for AgentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AgentPath {
    /// Create the root agent path.
    #[must_use]
    pub fn root() -> Self {
        Self("/root".to_string())
    }

    /// Create a child agent path under this parent.
    #[must_use]
    pub fn join(&self, name: &str) -> Self {
        Self(format!("{}/{}", self.0, name))
    }

    /// Extract the last segment (the agent's name).
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

/// Runtime status of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Interrupted,
    Completed {
        /// Optional final assistant message content.
        message: Option<String>,
    },
    Errored {
        /// Human-readable error description.
        reason: String,
    },
    Shutdown,
}

/// Message sent between agents in a multi-agent session.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct InterAgentMessage {
    pub from: AgentPath,
    pub to: AgentPath,
    pub content: String,
    #[builder(default)]
    pub trigger_turn: bool,
}
```

> **注：** `InterAgentMessage` 有 4 个字段（≥4），必须使用 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add multi-agent identity, status, and inter-agent message types
EOF
)"
```

---

### 任务 7：实现 config 类型

**文件：**
- 创建：`crates/protocol/src/config.rs`

- [ ] **步骤1：编写 config.rs**

```rust
//! Session configuration types: modes, models, and configurable options.

use serde::{Deserialize, Serialize};

/// A session mode preset (e.g. read-only, auto, full-access).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// Model info exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
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

/// A configurable option for a session (e.g. reasoning effort level).
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[builder(default)]
    pub description: Option<String>,
    pub values: Vec<SessionConfigValue>,
    #[builder(default)]
    pub current_value: Option<String>,
}

/// A selectable value within a session config option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigValue {
    pub id: String,
    pub label: String,
}
```

> **注：** `SessionMode` 3 个字段，不需要 TypedBuilder。`SessionConfigValue` 2 个字段，不需要 TypedBuilder。`ModelInfo` 5 个字段（≥4），`SessionConfigOption` 5 个字段（≥4），两者必须使用 TypedBuilder。

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/config.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add session configuration types
EOF
)"
```

---

### 任务 8：将 Message + OneOrMany 从 provider 上移至 protocol

这是最关键的一步。策略：复制源码到 protocol，去掉 provider 内部依赖（`use crate::OneOrMany`、`use super::CompletionError`），然后在 provider 中通过 re-export 保持所有现有 `use crate::message::*` 和 `use crate::OneOrMany` 路径不变。

**文件：**
- 创建：`crates/protocol/src/one_or_many.rs`
- 创建：`crates/protocol/src/message.rs`
- 修改：`crates/provider/Cargo.toml`
- 修改：`crates/provider/src/one_or_many.rs`
- 修改：`crates/provider/src/completion/message.rs`
- 修改：`crates/provider/src/lib.rs`

- [ ] **步骤1：将 one_or_many.rs 复制到 protocol**

`one_or_many.rs` 没有 crate 内部依赖（仅用 serde、thiserror），可直接完整复制。

```bash
cp crates/provider/src/one_or_many.rs crates/protocol/src/one_or_many.rs
```

- [ ] **步骤2：将 message.rs 复制到 protocol 并修改**

复制文件：

```bash
cp crates/provider/src/completion/message.rs crates/protocol/src/message.rs
```

然后编辑 `crates/protocol/src/message.rs`，做以下 3 处修改：

**修改1 — 第3行**：替换 `use crate::OneOrMany;` 为：

```rust
use crate::one_or_many::OneOrMany;
```

**修改2 — 第7行**：删除 `use super::CompletionError;`

**修改3 — 第1171-1175行**：删除 `impl From<MessageError> for CompletionError` 整个块：

```rust
// 删除这整段，CompletionError 留在 provider 中
impl From<MessageError> for CompletionError {
    fn from(error: MessageError) -> Self {
        CompletionError::RequestError(error.into())
    }
}
```

- [ ] **步骤3：更新 provider 的 Cargo.toml，添加 protocol 依赖**

在 `[dependencies]` 最上方添加：

```toml
clawcode-protocol = { path = "../protocol" }
```

- [ ] **步骤4：替换 provider 的 one_or_many.rs 为 re-export**

```rust
//! OneOrMany container type, re-exported from protocol.
pub use protocol::one_or_many::*;
```

- [ ] **步骤5：替换 provider 的 completion/message.rs 为 re-export**

```rust
//! Provider-agnostic chat message types.
//!
//! These types are defined in `clawcode-protocol` and re-exported here
//! for backward compatibility.

pub use protocol::message::*;

// Re-add the From impl that depends on CompletionError (provider-only type)
use crate::completion::CompletionError;

impl From<MessageError> for CompletionError {
    fn from(error: MessageError) -> Self {
        CompletionError::RequestError(error.into())
    }
}
```

- [ ] **步骤6：确认 provider lib.rs 无需修改**

现有 `lib.rs` 的模块声明和 re-export 保持不变：

```rust
// 这些都不需要改
pub mod completion;                                // completion/message.rs 内部已 re-export
pub mod one_or_many;                               // one_or_many.rs 内部已 re-export
pub use completion::message;                       // crate::message 路径继续有效
pub use one_or_many::{EmptyListError, OneOrMany};  // crate::OneOrMany 路径继续有效
```

因为 `completion::message` 和 `one_or_many` 模块内部已改为 `pub use protocol::...`, 
provider 内部所有 `use crate::message::X` 和 `use crate::OneOrMany` 路径自动跟随，无需逐个文件修改。

- [ ] **步骤7：构建全 workspace 验证**

```bash
cargo check
```

修复任何编译错误。如果有 provider 内部文件报了类型找不到的错误，检查对应的 `use` 路径是否需要从 `use crate::completion::message::X` 改为 `use crate::message::X`（后者更短，且因为 lib.rs 的 `pub use completion::message` 而有效）。

- [ ] **步骤8：运行 provider 测试**

```bash
cargo test -p provider
```

- [ ] **步骤9：提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor(protocol): move Message and OneOrMany types from provider to protocol
EOF
)"
```

---

### 任务 9：实现 Event 流式事件类型

**文件：**
- 创建：`crates/protocol/src/event.rs`

- [ ] **步骤1：编写 event.rs**

```rust
//! Streaming event types emitted from the kernel to the frontend.

use serde::{Deserialize, Serialize};

use crate::agent::{AgentPath, AgentStatus};
use crate::permission::PermissionRequest;
use crate::plan::PlanEntry;
use crate::session::SessionId;
use crate::tool::ToolCallStatus;

/// Streaming event emitted from the kernel to the frontend.
///
/// Each event carries a `session_id` and represents a discrete update
/// the frontend should render: text deltas, tool calls, plan changes, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Text delta from the assistant message.
    AgentMessageChunk {
        session_id: SessionId,
        /// Incremental text to append.
        text: String,
    },
    /// Reasoning / thinking delta from the assistant.
    AgentThoughtChunk {
        session_id: SessionId,
        /// Incremental thinking text to append.
        text: String,
    },
    /// A tool call was initiated by the assistant.
    ToolCall {
        session_id: SessionId,
        /// Agent that made the tool call.
        agent_path: AgentPath,
        /// Unique call identifier within the turn.
        call_id: String,
        /// Tool/function name.
        name: String,
        /// JSON-encoded arguments.
        arguments: serde_json::Value,
        /// Current execution status.
        status: ToolCallStatus,
    },
    /// Incremental update to an active tool call.
    ToolCallUpdate {
        session_id: SessionId,
        /// The tool call being updated.
        call_id: String,
        /// New output delta to append.
        output_delta: Option<String>,
        /// Updated status, if changed.
        status: Option<ToolCallStatus>,
    },
    /// The agent's execution plan was created or updated.
    PlanUpdate {
        session_id: SessionId,
        /// Complete list of plan entries (replaces previous plan).
        entries: Vec<PlanEntry>,
    },
    /// Token usage information for the current turn.
    UsageUpdate {
        session_id: SessionId,
        /// Number of input (prompt) tokens consumed.
        input_tokens: u64,
        /// Number of output (completion) tokens produced.
        output_tokens: u64,
    },
    /// The kernel is requesting user permission for a tool execution.
    PermissionRequested {
        session_id: SessionId,
        /// The permission request details.
        request: PermissionRequest,
    },
    /// A sub-agent's runtime status changed.
    AgentStatusChange {
        session_id: SessionId,
        /// The agent whose status changed.
        agent_path: AgentPath,
        /// New status.
        status: AgentStatus,
    },
    /// The current turn has completed.
    TurnComplete {
        session_id: SessionId,
        /// Reason the turn stopped.
        stop_reason: StopReason,
    },
}

/// Reason a turn completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Turn finished normally.
    EndTurn,
    /// Turn was cancelled by the user.
    Cancelled,
    /// Turn terminated due to an error.
    Error,
}
```

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/event.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add streaming event types
EOF
)"
```

---

### 任务 10：实现 Op 操作指令类型

**文件：**
- 创建：`crates/protocol/src/op.rs`

- [ ] **步骤1：编写 op.rs**

```rust
//! Operation types submitted from the frontend / client to the kernel.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::AgentPath;
use crate::message::Message;
use crate::session::SessionId;

/// Operation submitted from the frontend / client to the kernel.
///
/// Each variant represents a command the kernel should execute.
/// Responses come as streaming [`Event`](crate::event::Event)s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Create a new session at the given working directory.
    NewSession { cwd: PathBuf },
    /// Load a previously persisted session.
    LoadSession { session_id: SessionId },
    /// Submit a user prompt to an active session.
    Prompt {
        session_id: SessionId,
        message: Message,
    },
    /// Cancel the currently running turn in a session.
    Cancel { session_id: SessionId },
    /// Change the session's approval/sandboxing mode.
    SetMode {
        session_id: SessionId,
        mode: String,
    },
    /// Switch the model for a session.
    SetModel {
        session_id: SessionId,
        provider_id: String,
        model_id: String,
    },
    /// Close a session and release its resources.
    CloseSession { session_id: SessionId },
    /// Spawn a sub-agent from a parent session.
    SpawnAgent {
        parent_session: SessionId,
        agent_path: AgentPath,
        role: String,
        prompt: String,
    },
    /// Deliver a message between agents.
    InterAgentMessage {
        from: AgentPath,
        to: AgentPath,
        content: String,
    },
}
```

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/op.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add operation command types
EOF
)"
```

---

### 任务 11：实现 AgentKernel trait

**文件：**
- 创建：`crates/protocol/src/kernel.rs`

- [ ] **步骤1：编写 kernel.rs**

```rust
//! Agent kernel trait and associated error/stream types.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::agent::AgentPath;
use crate::config::SessionMode;
use crate::config::ModelInfo;
use crate::event::Event;
use crate::message::Message;
use crate::session::{SessionCreated, SessionId, SessionInfo, SessionListPage};

/// Boxed, pinned stream of kernel events.
///
/// Returned by [`AgentKernel::prompt`]; the frontend consumes this
/// to receive real-time updates during a turn.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>;

/// Central agent kernel trait.
///
/// Implemented by the kernel crate, consumed by ACP and other
/// frontend protocol adapters. All session management, LLM
/// interaction, and tool execution flows through this interface.
#[async_trait]
pub trait AgentKernel: Send + Sync {
    /// Create a new session and return its ID plus available config.
    async fn new_session(
        &self,
        cwd: PathBuf,
    ) -> Result<SessionCreated, KernelError>;

    /// Load a previously persisted session.
    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionCreated, KernelError>;

    /// List persisted sessions with optional cwd filter and cursor-based pagination.
    async fn list_sessions(
        &self,
        cwd: Option<&Path>,
        cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError>;

    /// Submit a user prompt, returning a stream of events.
    ///
    /// The stream yields events until the turn completes, then terminates.
    async fn prompt(
        &self,
        session_id: &SessionId,
        message: Message,
    ) -> Result<EventStream, KernelError>;

    /// Cancel the currently running turn in a session.
    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Set the session's approval/sandboxing mode.
    async fn set_mode(
        &self,
        session_id: &SessionId,
        mode: &str,
    ) -> Result<(), KernelError>;

    /// Switch the model for a session.
    async fn set_model(
        &self,
        session_id: &SessionId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), KernelError>;

    /// Close a session and release its resources.
    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Spawn a sub-agent in a parent session.
    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        agent_path: AgentPath,
        role: &str,
        prompt: &str,
    ) -> Result<(), KernelError>;

    /// Return the available approval/sandboxing mode presets.
    fn available_modes(&self) -> Vec<SessionMode>;

    /// Return the available models from configured providers.
    fn available_models(&self) -> Vec<ModelInfo>;
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

- [ ] **步骤2：构建验证**

```bash
cargo check -p clawcode-protocol
```

预期：protocol crate 完全编译通过。

- [ ] **步骤3：提交**

```bash
git add crates/protocol/src/kernel.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add AgentKernel trait and kernel error types
EOF
)"
```

---

### 任务 12：实现 kernel crate

**文件：**
- 修改：`crates/kernel/Cargo.toml`
- 修改：`crates/kernel/src/lib.rs`

- [ ] **步骤1：更新 kernel Cargo.toml**

```toml
[package]
name = "kernel"
edition.workspace = true
version.workspace = true
description = "Clawcode agent kernel - session management, LLM orchestration, tool execution"

[dependencies]
clawcode-protocol = { path = "../protocol" }
clawcode-provider = { path = "../provider" }
config = { path = "../config" }

tokio = { workspace = true, features = ["rt", "sync", "macros"] }
async-trait = { workspace = true }
futures = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }
anyhow = { workspace = true }
typed-builder = { workspace = true }
```

- [ ] **步骤2：编写 kernel lib.rs**

```rust
//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via [`provider::LlmFactory`] and managing session state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use futures::Stream;
use tokio::sync::Mutex;

use protocol::{
    AgentKernel, AgentPath, AgentStatus, Event, KernelError,
    Message, ModelInfo, SessionCreated, SessionId, SessionInfo,
    SessionListPage, SessionMode, StopReason,
};
use provider::factory::{ArcLlm, LlmFactory};
use config::ConfigHandle;

/// Central kernel struct implementing [`AgentKernel`].
pub struct Kernel {
    llm_factory: Arc<LlmFactory>,
    config: ConfigHandle,
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
}

/// Per-session runtime handle.
struct SessionHandle {
    cwd: PathBuf,
    cancel_token: tokio::sync::watch::Sender<bool>,
}

impl Kernel {
    /// Create a new kernel instance with the given LLM factory and config.
    #[must_use]
    pub fn new(llm_factory: Arc<LlmFactory>, config: ConfigHandle) -> Self {
        Self {
            llm_factory,
            config,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Build available session modes.
    fn build_modes(&self) -> Vec<SessionMode> {
        vec![
            SessionMode {
                id: "read-only".to_string(),
                name: "Read Only".to_string(),
                description: Some("Agent cannot modify files".to_string()),
            },
            SessionMode {
                id: "auto".to_string(),
                name: "Auto".to_string(),
                description: Some("Agent asks for approval before making changes".to_string()),
            },
            SessionMode {
                id: "full-access".to_string(),
                name: "Full Access".to_string(),
                description: Some("Agent can modify files without approval".to_string()),
            },
        ]
    }

    /// Build available models from LLM configuration.
    fn build_models(&self) -> Vec<ModelInfo> {
        let cfg = self.config.current();
        cfg.providers
            .iter()
            .flat_map(|p| {
                p.models.iter().map(|m| {
                    ModelInfo::builder()
                        .id(format!("{}/{}", p.id.as_str(), m.id))
                        .display_name(m.display_name.clone())
                        .context_tokens(Some(m.context_tokens))
                        .max_output_tokens(Some(m.max_output_tokens))
                        .build()
                })
            })
            .collect()
    }
}

#[async_trait]
impl AgentKernel for Kernel {
    async fn new_session(&self, cwd: PathBuf) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let (cancel_tx, _) = tokio::sync::watch::channel(false);

        self.sessions.lock().await.insert(
            session_id.clone(),
            SessionHandle {
                cwd: cwd.clone(),
                cancel_token: cancel_tx,
            },
        );

        Ok(SessionCreated {
            session_id,
            modes: self.build_modes(),
            models: self.build_models(),
        })
    }

    async fn load_session(&self, session_id: &SessionId) -> Result<SessionCreated, KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(SessionCreated {
            session_id: session_id.clone(),
            modes: self.build_modes(),
            models: self.build_models(),
        })
    }

    async fn list_sessions(
        &self,
        _cwd: Option<&Path>,
        _cursor: Option<&str>,
    ) -> Result<SessionListPage, KernelError> {
        let sessions: Vec<SessionInfo> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, handle)| SessionInfo::builder()
                .session_id(id.clone())
                .cwd(handle.cwd.clone())
                .build())
            .collect();

        Ok(SessionListPage {
            sessions,
            next_cursor: None,
        })
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        message: Message,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>,
        KernelError,
    > {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }

        // Minimal stub: echoes input and completes.
        // Full LLM integration will be added in a subsequent plan.
        let sid = session_id.clone();
        let events: Vec<Result<Event, KernelError>> = vec![
            Ok(Event::AgentMessageChunk {
                session_id: sid.clone(),
                text: format!("Received: {:?}", message),
            }),
            Ok(Event::TurnComplete {
                session_id: sid,
                stop_reason: StopReason::EndTurn,
            }),
        ];

        Ok(Box::pin(stream::iter(events)))
    }

    async fn cancel(&self, session_id: &SessionId) -> Result<(), KernelError> {
        match self.sessions.lock().await.get(session_id) {
            Some(handle) => {
                let _ = handle.cancel_token.send(true);
                Ok(())
            }
            None => Err(KernelError::SessionNotFound(session_id.clone())),
        }
    }

    async fn set_mode(&self, session_id: &SessionId, _mode: &str) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn set_model(
        &self,
        session_id: &SessionId,
        _provider_id: &str,
        _model_id: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError> {
        if self.sessions.lock().await.remove(session_id).is_none() {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        Ok(())
    }

    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        _agent_path: AgentPath,
        _role: &str,
        _prompt: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(parent_session) {
            return Err(KernelError::SessionNotFound(parent_session.clone()));
        }
        // Sub-agent spawning will be implemented in a subsequent plan.
        Ok(())
    }

    fn available_modes(&self) -> Vec<SessionMode> {
        self.build_modes()
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        self.build_models()
    }
}
```

- [ ] **步骤3：构建验证**

```bash
cargo check -p kernel
```

- [ ] **步骤4：提交**

```bash
git add crates/kernel/
git commit -m "$(cat <<'EOF'
feat(kernel): implement AgentKernel trait with session management
EOF
)"
```

---

### 任务 13：创建 ACP 桥接 crate

**文件：**
- 创建：`crates/acp/Cargo.toml`
- 创建：`crates/acp/src/lib.rs`
- 创建：`crates/acp/src/translate.rs`
- 创建：`crates/acp/src/agent.rs`
- 创建：`crates/acp/src/main.rs`

- [ ] **步骤1：创建 Cargo.toml**

```toml
[package]
name = "clawcode-acp"
edition.workspace = true
version.workspace = true
description = "ACP bridge for clawcode - translates between internal protocol and Agent Client Protocol"

[[bin]]
name = "clawcode-acp"
path = "src/main.rs"

[dependencies]
clawcode-protocol = { path = "../protocol" }
clawcode-kernel = { path = "../kernel" }
clawcode-provider = { path = "../provider" }
config = { path = "../config" }

agent-client-protocol = { workspace = true, features = ["unstable"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std"] }
tokio-util = { version = "0.7", features = ["compat"] }
async-trait = { workspace = true }
futures = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true, features = ["env-filter"] }
anyhow = { workspace = true }
uuid = { workspace = true }
thiserror = { workspace = true }
typed-builder = { workspace = true }
```

- [ ] **步骤2：创建 translate.rs**

```rust
//! Type translation from clawcode internal types to ACP schema types.
//!
//! All conversions use the `From` trait with move semantics
//! to avoid unnecessary memory copies.

use agent_client_protocol as acp;
use acp::schema;
use protocol as proto;

// ── StopReason ──

impl From<proto::StopReason> for schema::StopReason {
    fn from(r: proto::StopReason) -> Self {
        match r {
            proto::StopReason::EndTurn => Self::EndTurn,
            proto::StopReason::Cancelled => Self::Cancelled,
            proto::StopReason::Error => Self::Error,
        }
    }
}

// ── ToolCallStatus ──

impl From<proto::ToolCallStatus> for schema::ToolCallStatus {
    fn from(s: proto::ToolCallStatus) -> Self {
        match s {
            proto::ToolCallStatus::Pending => Self::Pending,
            proto::ToolCallStatus::InProgress => Self::InProgress,
            proto::ToolCallStatus::Completed => Self::Completed,
            proto::ToolCallStatus::Failed => Self::Failed,
        }
    }
}

// ── PlanPriority ──

impl From<proto::PlanPriority> for schema::PlanEntryPriority {
    fn from(p: proto::PlanPriority) -> Self {
        match p {
            proto::PlanPriority::Low => Self::Low,
            proto::PlanPriority::Medium => Self::Medium,
            proto::PlanPriority::High => Self::High,
        }
    }
}

// ── PlanStatus ──

impl From<proto::PlanStatus> for schema::PlanEntryStatus {
    fn from(s: proto::PlanStatus) -> Self {
        match s {
            proto::PlanStatus::Pending => Self::Pending,
            proto::PlanStatus::InProgress => Self::InProgress,
            proto::PlanStatus::Completed => Self::Completed,
        }
    }
}

// ── PermissionOptionKind ──

impl From<proto::PermissionOptionKind> for schema::PermissionOptionKind {
    fn from(k: proto::PermissionOptionKind) -> Self {
        match k {
            proto::PermissionOptionKind::AllowOnce => Self::AllowOnce,
            proto::PermissionOptionKind::AllowAlways => Self::AllowAlways,
            proto::PermissionOptionKind::RejectOnce => Self::RejectOnce,
            proto::PermissionOptionKind::RejectAlways => Self::RejectAlways,
        }
    }
}
```

- [ ] **步骤3：创建 agent.rs**

```rust
//! ACP Agent bridging the clawcode kernel to the ACP protocol.

use std::sync::{Arc, Mutex};

use acp::schema::{
    AgentAuthCapabilities, AgentCapabilities, AuthenticateRequest,
    AuthenticateResponse, CancelNotification, ClientCapabilities,
    CloseSessionRequest, CloseSessionResponse, Implementation,
    InitializeRequest, InitializeResponse, LogoutCapabilities,
    McpCapabilities, NewSessionRequest, NewSessionResponse,
    PromptCapabilities, PromptRequest, PromptResponse,
    SessionCapabilities, SessionCloseCapabilities,
    SessionListCapabilities, SessionId as AcpSessionId,
    SessionMode as AcpSessionMode, ModelInfo as AcpModelInfo,
    SetSessionModeRequest, SetSessionModeResponse,
    SetSessionModelRequest, SetSessionModelResponse,
};
use acp::{Agent, Client, ConnectTo, ConnectionTo, Error};
use agent_client_protocol as acp;

use protocol::{AgentKernel, SessionId};
use provider::factory::LlmFactory;

/// ACP Agent bridging the clawcode kernel to the ACP protocol.
pub struct ClawcodeAgent {
    kernel: Arc<dyn AgentKernel>,
    #[allow(dead_code)]
    llm_factory: Arc<LlmFactory>,
    #[allow(dead_code)]
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
}

impl ClawcodeAgent {
    /// Create a new ACP agent with the given kernel and LLM factory.
    #[must_use]
    pub fn new(
        kernel: Arc<dyn AgentKernel>,
        llm_factory: Arc<LlmFactory>,
    ) -> Self {
        Self {
            kernel,
            llm_factory,
            client_capabilities: Arc::default(),
        }
    }

    /// Convert an internal SessionId to an ACP SessionId.
    fn to_acp_session_id(id: &SessionId) -> AcpSessionId {
        AcpSessionId::new(id.0.clone())
    }

    /// Build and serve the ACP agent over the given transport.
    pub async fn serve(
        self: Arc<Self>,
        transport: impl ConnectTo<Agent> + 'static,
    ) -> acp::Result<()> {
        let agent = self;
        Agent
            .builder()
            .name("clawcode-acp")
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: InitializeRequest, responder, _cx| {
                        responder.respond_with_result(
                            agent.handle_initialize(request).await,
                        )
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: AuthenticateRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_authenticate(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: NewSessionRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_new_session(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: PromptRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_prompt(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                {
                    let agent = agent.clone();
                    async move |notification: CancelNotification,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            if let Err(e) = agent.handle_cancel(notification).await
                            {
                                tracing::error!(
                                    "Error handling cancel: {:?}", e
                                );
                            }
                            Ok(())
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: SetSessionModeRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_set_mode(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: SetSessionModelRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_set_model(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: CloseSessionRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_close_session(request).await,
                            )
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .connect_to(transport)
            .await
    }

    // ── Handler implementations ──

    async fn handle_initialize(
        &self,
        _request: InitializeRequest,
    ) -> Result<InitializeResponse, Error> {
        let protocol_version = acp::schema::ProtocolVersion::V1;

        let mut caps = AgentCapabilities::new()
            .prompt_capabilities(
                PromptCapabilities::new()
                    .embedded_context(true)
                    .image(true),
            )
            .mcp_capabilities(McpCapabilities::new().http(true))
            .load_session(true)
            .auth(
                AgentAuthCapabilities::new()
                    .logout(LogoutCapabilities::new()),
            );

        caps.session_capabilities = SessionCapabilities::new()
            .close(SessionCloseCapabilities::new())
            .list(SessionListCapabilities::new());

        Ok(InitializeResponse::new(protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                Implementation::new(
                    "clawcode-acp",
                    env!("CARGO_PKG_VERSION"),
                )
                .title("Clawcode"),
            ))
    }

    async fn handle_authenticate(
        &self,
        _request: AuthenticateRequest,
    ) -> Result<AuthenticateResponse, Error> {
        Ok(AuthenticateResponse::new())
    }

    async fn handle_new_session(
        &self,
        request: NewSessionRequest,
    ) -> Result<NewSessionResponse, Error> {
        let NewSessionRequest { cwd, .. } = request;

        let created = self
            .kernel
            .new_session(cwd)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        let acp_session_id = Self::to_acp_session_id(&created.session_id);

        let modes: Vec<AcpSessionMode> = created
            .modes
            .into_iter()
            .map(|m| {
                let mut acp_mode = AcpSessionMode::new(
                    acp::schema::SessionModeId::new(m.id),
                    m.name,
                );
                if let Some(desc) = m.description {
                    acp_mode = acp_mode.description(desc);
                }
                acp_mode
            })
            .collect();

        let models: Vec<AcpModelInfo> = created
            .models
            .into_iter()
            .map(|m| {
                let mut info = AcpModelInfo::new(
                    acp::schema::ModelId::new(m.id),
                    m.display_name,
                );
                if let Some(desc) = m.description {
                    info = info.description(desc);
                }
                info
            })
            .collect();

        Ok(NewSessionResponse::new(acp_session_id)
            .modes(modes)
            .models(models))
    }

    async fn handle_prompt(
        &self,
        request: PromptRequest,
    ) -> Result<PromptResponse, Error> {
        let _session_id = SessionId(request.session_id.0.clone());

        // Minimal stub: echoes back via a user message.
        // Full event translation loop will be implemented in a subsequent plan.
        let stop_reason =
            acp::schema::StopReason::EndTurn;

        Ok(PromptResponse::new(stop_reason))
    }

    async fn handle_cancel(
        &self,
        notification: CancelNotification,
    ) -> Result<(), Error> {
        let session_id = SessionId(notification.session_id.0.clone());
        self.kernel
            .cancel(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))
    }

    async fn handle_set_mode(
        &self,
        request: SetSessionModeRequest,
    ) -> Result<SetSessionModeResponse, Error> {
        let session_id = SessionId(request.session_id.0.clone());
        self.kernel
            .set_mode(&session_id, &request.mode_id.0)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(SetSessionModeResponse::default())
    }

    async fn handle_set_model(
        &self,
        request: SetSessionModelRequest,
    ) -> Result<SetSessionModelResponse, Error> {
        let session_id = SessionId(request.session_id.0.clone());
        // model_id format: "provider_id/model_id"
        let parts: Vec<&str> = request.model_id.0.splitn(2, '/').collect();
        let (provider_id, model_id) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            ("", parts[0])
        };
        self.kernel
            .set_model(&session_id, provider_id, model_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(SetSessionModelResponse::default())
    }

    async fn handle_close_session(
        &self,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, Error> {
        let session_id = SessionId(request.session_id.0.clone());
        self.kernel
            .close_session(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(CloseSessionResponse::new())
    }
}
```

- [ ] **步骤4：创建 lib.rs**

```rust
//! Clawcode ACP bridge.
//!
//! Implements the Agent Client Protocol (ACP) over stdio,
//! translating between the clawcode internal protocol and
//! the ACP schema types for Zed editor integration.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;
pub mod translate;

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use protocol::AgentKernel;
use provider::factory::LlmFactory;

/// Start the ACP agent over stdio transport.
///
/// # Errors
///
/// Returns an error if the ACP transport fails or the kernel
/// encounters an unrecoverable error.
pub async fn run(
    kernel: Arc<dyn AgentKernel>,
    llm_factory: Arc<LlmFactory>,
) -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env(),
        )
        .init();

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(agent::ClawcodeAgent::new(kernel, llm_factory));
    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))
}
```

- [ ] **步骤5：创建 main.rs**

```rust
//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use config::ConfigHandle;
use kernel::Kernel;
use provider::factory::LlmFactory;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = ConfigHandle::default();
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let kernel = Arc::new(Kernel::new(llm_factory.clone(), config));

    acp::run(kernel, llm_factory).await
}
```

- [ ] **步骤6：构建全 workspace**

```bash
cargo check
```

- [ ] **步骤7：提交**

```bash
git add crates/acp/
git commit -m "$(cat <<'EOF'
feat(acp): add ACP bridge crate with handler registration and type translations
EOF
)"
```

---

### 任务 14：最终验证

- [ ] **步骤1：构建全 workspace**

```bash
cargo build
```

- [ ] **步骤2：运行全部测试**

```bash
cargo test
```

- [ ] **步骤3：运行 clippy**

```bash
cargo clippy -- -D warnings
```

- [ ] **步骤4：修复并提交**

```bash
git add -A && git commit -m "$(cat <<'EOF'
chore: fix clippy warnings and test failures after protocol implementation
EOF
)"
```

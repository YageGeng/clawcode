# Protocol Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the clawcode internal protocol layer, migrate shared Message types from provider to protocol, build the kernel's AgentKernel trait implementation, and create the ACP bridge crate.

**Architecture:** Four-phase implementation. Phase 1 creates the protocol crate with pure data types. Phase 2 moves `Message` and related types from provider up to protocol. Phase 3 implements kernel with the AgentKernel trait backed by LlmFactory. Phase 4 creates the ACP bridge crate with `From` trait translations.

**Tech Stack:** Rust 2024 edition, tokio, serde, async-trait, typed-builder, agent-client-protocol, uuid (SessionId generation), strum (enum display), thiserror (kernel errors)

**Spec:** `docs/superpowers/specs/2026-05-11-protocol-layer-design.md`

---

## File Map

| File | Purpose |
|---|---|
| `crates/protocol/Cargo.toml` | New crate manifest |
| `crates/protocol/src/lib.rs` | Module declarations + unified re-exports |
| `crates/protocol/src/message.rs` | Message, UserContent, AssistantContent, ToolCall, ToolFunction, ToolResult, ToolResultContent, Text, Image, Reasoning, Role (migrated from provider) |
| `crates/protocol/src/session.rs` | SessionId, SessionInfo, SessionCreated, SessionListPage |
| `crates/protocol/src/tool.rs` | ToolDefinition, ToolCallStatus |
| `crates/protocol/src/plan.rs` | PlanEntry, PlanPriority, PlanStatus |
| `crates/protocol/src/permission.rs` | PermissionRequest, PermissionOption, PermissionOptionKind |
| `crates/protocol/src/agent.rs` | AgentPath, AgentStatus, InterAgentMessage |
| `crates/protocol/src/config.rs` | SessionMode, ModelInfo, SessionConfigOption, SessionConfigValue |
| `crates/protocol/src/event.rs` | Event enum (all streaming event variants), StopReason |
| `crates/protocol/src/op.rs` | Op enum (all operation variants) |
| `crates/protocol/src/kernel.rs` | AgentKernel trait, KernelError, EventStream |
| `Cargo.toml` (workspace root) | Add typed-builder, uuid, strum to workspace dependencies; add protocol back as workspace member |
| `crates/provider/Cargo.toml` | Add dependency on clawcode-protocol |
| `crates/provider/src/completion/message.rs` | Delete (types moved to protocol) |
| `crates/provider/src/completion/mod.rs` | Remove message module, update imports |
| `crates/provider/src/lib.rs` | Re-export message types from protocol |
| `crates/provider/src/factory/event.rs` | Update imports to use protocol types |
| `crates/provider/src/providers/anthropic/completion.rs` | Update imports |
| `crates/provider/src/providers/anthropic/streaming.rs` | Update imports |
| `crates/provider/src/providers/openai/completion/mod.rs` | Update imports |
| `crates/provider/src/providers/openai/responses_api/mod.rs` | Update imports |
| `crates/provider/src/providers/openai/responses_api/streaming.rs` | Update imports |
| `crates/provider/src/providers/chatgpt/mod.rs` | Update imports |
| `crates/provider/src/providers/deepseek.rs` | Update imports |
| `crates/provider/src/providers/moonshot.rs` | Update imports |
| `crates/provider/src/providers/minimax.rs` | Update imports |
| `crates/provider/src/providers/xiaomimimo.rs` | Update imports |
| `crates/provider/src/providers/internal/buffered.rs` | Update imports |
| `crates/provider/src/providers/internal/openai_chat_completions_compatible.rs` | Update imports |
| `crates/provider/src/streaming.rs` | Update imports |
| `crates/provider/examples/*.rs` | Update imports |
| `crates/kernel/Cargo.toml` | Add dependencies: clawcode-protocol, clawcode-provider, clawcode-config, tokio, async-trait, anyhow |
| `crates/kernel/src/lib.rs` | Replace stub with Kernel struct implementing AgentKernel |
| `crates/acp/Cargo.toml` | New crate manifest with ACP dependencies |
| `crates/acp/src/lib.rs` | Entry point: run() with stdio transport |
| `crates/acp/src/agent.rs` | ClawcodeAgent struct + ACP handler registration |
| `crates/acp/src/translate.rs` | From impls: internal types → ACP types |

---

### Task 1: Add workspace dependencies and create protocol crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/protocol/Cargo.toml`
- Create: `crates/protocol/src/lib.rs`

- [ ] **Step 1: Add typed-builder, uuid, strum to workspace dependencies**

Open `Cargo.toml` (workspace root). Add under `# serde` area:

```toml
# builder
typed-builder = "0.21"
```

Add under `# misc` area:

```toml
uuid = { version = "1", features = ["v4", "serde"] }
strum = { version = "0.27", features = ["derive"] }
agent-client-protocol = "0.11.1"
```

Also add the protocol as a workspace member dependency at the bottom:

```toml
clawcode-protocol = { path = "crates/protocol" }
```

- [ ] **Step 2: Create protocol crate Cargo.toml**

Create `crates/protocol/Cargo.toml`:

```toml
[package]
name = "clawcode-protocol"
edition.workspace = true
version.workspace = true
description = "Internal protocol types for clawcode agent-core / frontend communication"

[lib]
name = "clawcode_protocol"
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

- [ ] **Step 3: Create protocol crate lib.rs skeleton**

Create `crates/protocol/src/lib.rs`:

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
pub mod op;
pub mod permission;
pub mod plan;
pub mod session;
pub mod tool;

// Re-export everything for convenience
pub use agent::*;
pub use config::*;
pub use event::*;
pub use kernel::*;
pub use message::*;
pub use op::*;
pub use permission::*;
pub use plan::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 4: Build to verify skeleton compiles**

```bash
cargo check -p clawcode-protocol
```

Expected: errors about missing modules (will resolve in subsequent tasks).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/protocol/
git commit -m "$(cat <<'EOF'
chore(protocol): add protocol crate skeleton and workspace dependencies
EOF
)"
```

---

### Task 2: Implement protocol session types

**Files:**
- Create: `crates/protocol/src/session.rs`

- [ ] **Step 1: Write session.rs**

```rust
//! Session identifier and metadata types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique session identifier.
///
/// Generated when a new session is created, used to reference
/// the session in all subsequent operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Summary info for a session shown in listing results.
///
/// Contains enough data to render a session picker row without
/// loading the full session state.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionInfo {
    /// Unique session identifier.
    pub session_id: SessionId,
    /// Working directory where the session runs.
    pub cwd: PathBuf,
    /// Human-readable title (usually the first user message).
    #[builder(default)]
    pub title: Option<String>,
    /// ISO-8601 timestamp of last activity.
    #[builder(default)]
    pub updated_at: Option<String>,
}

/// Data returned to the frontend after creating or loading a session.
///
/// Carries the session id plus the available modes and models
/// the frontend can present in its configuration UI.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct SessionCreated {
    /// The created or loaded session.
    pub session_id: SessionId,
    /// Available approval/sandboxing mode presets.
    pub modes: Vec<super::config::SessionMode>,
    /// Available model presets for this provider configuration.
    pub models: Vec<super::config::ModelInfo>,
}

/// Paginated session list result.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct SessionListPage {
    /// Sessions in the current page.
    pub sessions: Vec<SessionInfo>,
    /// Opaque cursor for the next page, `None` when on the last page.
    #[builder(default)]
    pub next_cursor: Option<String>,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

Expected: errors only for other missing modules (tool, plan, etc.); session module compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/session.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add session identifier and metadata types
EOF
)"
```

---

### Task 3: Implement protocol tool types

**Files:**
- Create: `crates/protocol/src/tool.rs`

- [ ] **Step 1: Write tool.rs**

```rust
//! Tool definition and execution status types.

use serde::{Deserialize, Serialize};

/// Tool definition registered with the agent kernel.
///
/// Describes a callable tool the LLM can invoke via function calling.
/// The `parameters` field uses JSON Schema to define the tool's arguments.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
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
    /// Tool call has been requested but execution has not started.
    Pending,
    /// Tool is currently executing.
    InProgress,
    /// Tool execution completed successfully.
    Completed,
    /// Tool execution failed with an error.
    Failed,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/tool.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add tool definition and execution status types
EOF
)"
```

---

### Task 4: Implement protocol plan types

**Files:**
- Create: `crates/protocol/src/plan.rs`

- [ ] **Step 1: Write plan.rs**

```rust
//! Plan and task-progress types for structured agent output.

use serde::{Deserialize, Serialize};

/// A single entry in the agent's execution plan.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
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
pub enum PlanPriority {
    Low,
    Medium,
    High,
}

/// Execution status of a plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add plan and task-progress types
EOF
)"
```

---

### Task 5: Implement protocol permission types

**Files:**
- Create: `crates/protocol/src/permission.rs`

- [ ] **Step 1: Write permission.rs**

```rust
//! Permission request types for tool execution approval.

use serde::{Deserialize, Serialize};

/// Permission request sent from the kernel to the frontend
/// when a tool execution needs user approval.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct PermissionRequest {
    /// Identifies the tool call this permission is for.
    pub call_id: String,
    /// Human-readable message explaining what needs approval.
    pub message: String,
    /// Available permission choices for the user.
    pub options: Vec<PermissionOption>,
}

/// A single permission option the user can choose.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct PermissionOption {
    /// Unique option identifier (e.g. "allow_once", "reject_always").
    pub id: String,
    /// Human-readable label (e.g. "Allow Once").
    pub label: String,
    /// The kind of this option determining its scope and persistence.
    pub kind: PermissionOptionKind,
}

/// Classification of a permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    /// Grant permission for this single execution only.
    AllowOnce,
    /// Grant permission and persist for future identical requests.
    AllowAlways,
    /// Deny this single execution.
    RejectOnce,
    /// Deny and persist rejection for future identical requests.
    RejectAlways,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/permission.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add permission request types
EOF
)"
```

---

### Task 6: Implement protocol agent types

**Files:**
- Create: `crates/protocol/src/agent.rs`

- [ ] **Step 1: Write agent.rs**

```rust
//! Multi-agent identity, status, and inter-agent messaging types.

use serde::{Deserialize, Serialize};

/// Hierarchical agent path, e.g. `/root/explorer`.
///
/// Paths use `/` as separator and start with `/root` for the
/// main agent. Sub-agents append their name: `/root/worker`.
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
///
/// Used to track the lifecycle of agents in a multi-agent session,
/// reported via `Event::AgentStatusChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent is actively processing a turn.
    Running,
    /// Agent's turn was interrupted, may receive further input.
    Interrupted,
    /// Agent completed its turn, with an optional final message.
    Completed {
        /// Optional final assistant message content.
        message: Option<String>,
    },
    /// Agent encountered an unrecoverable error.
    Errored {
        /// Human-readable error description.
        reason: String,
    },
    /// Agent has been shut down and released its resources.
    Shutdown,
}

/// Message sent between agents in a multi-agent session.
///
/// Carries the sender, recipient(s), and content. When `trigger_turn`
/// is true, the recipient should start a new turn upon delivery.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct InterAgentMessage {
    /// Sender agent path.
    pub from: AgentPath,
    /// Primary recipient agent path.
    pub to: AgentPath,
    /// Message content (plain text or structured data).
    pub content: String,
    /// If true, the recipient should start a new turn upon delivery.
    #[builder(default)]
    pub trigger_turn: bool,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add multi-agent identity, status, and inter-agent message types
EOF
)"
```

---

### Task 7: Implement protocol config types

**Files:**
- Create: `crates/protocol/src/config.rs`

- [ ] **Step 1: Write config.rs**

```rust
//! Session configuration types: modes, models, and configurable options.

use serde::{Deserialize, Serialize};

/// A session mode preset defining the agent's approval and sandboxing behavior.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionMode {
    /// Unique mode identifier (e.g. "read-only", "auto", "full-access").
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Optional description explaining what the mode enables.
    #[builder(default)]
    pub description: Option<String>,
}

/// Information about an available model exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct ModelInfo {
    /// Model identifier within the provider (e.g. "deepseek-v4-pro").
    pub id: String,
    /// Human-readable display name (e.g. "DeepSeek V4 Pro").
    pub display_name: String,
    /// Optional description of the model's capabilities.
    #[builder(default)]
    pub description: Option<String>,
    /// Maximum context window size in tokens, if known.
    #[builder(default)]
    pub context_tokens: Option<u64>,
    /// Maximum output tokens per response, if known.
    #[builder(default)]
    pub max_output_tokens: Option<u64>,
}

/// A configurable option exposed for a session (e.g. reasoning effort).
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionConfigOption {
    /// Unique config option identifier.
    pub id: String,
    /// Human-readable option name.
    pub name: String,
    /// Optional description of what this option controls.
    #[builder(default)]
    pub description: Option<String>,
    /// Available values for this option.
    pub values: Vec<SessionConfigValue>,
    /// Currently selected value id, if any.
    #[builder(default)]
    pub current_value: Option<String>,
}

/// A selectable value within a session config option.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionConfigValue {
    /// Unique value identifier.
    pub id: String,
    /// Human-readable label.
    pub label: String,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/config.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add session configuration types
EOF
)"
```

---

### Task 8: Move Message types from provider to protocol

**Files:**
- Create: `crates/protocol/src/message.rs`
- Modify: `crates/provider/Cargo.toml`
- Modify: `crates/provider/src/completion/message.rs` → delete content, re-export from protocol
- Modify: All provider files referencing `crate::completion::message::*`

- [ ] **Step 1: Copy message.rs content to protocol crate**

Read `crates/provider/src/completion/message.rs` and copy the ENTIRE content into `crates/protocol/src/message.rs`.

Then change the `use` imports at the top: replace `use crate::OneOrMany;` with `use crate::one_or_many::OneOrMany;` (we'll fix this in step 2 — actually we need to handle the `OneOrMany` type which is in provider but used in message types).

Actually, we need to move `OneOrMany` too, or re-export it. Let's move it to protocol as well.

Create a new file `crates/protocol/src/one_or_many.rs` by copying from `crates/provider/src/one_or_many.rs`.

Then in `crates/protocol/src/message.rs`, replace:
```rust
use crate::OneOrMany;
```
with:
```rust
use crate::one_or_many::OneOrMany;
```

And change completion error references to use protocol's own error type (keep `CompletionError` and `PromptError` in provider — Message only references them via `From` impls which stay in provider).

Wait — this is getting complex. Let me read the actual message.rs to understand all the intra-crate references.

- [ ] **Step 2: Read provider message.rs to understand dependencies**

Run:
```bash
cat crates/provider/src/completion/message.rs
```

Identify all `use crate::...` references and external deps needed.

Expected: `use crate::OneOrMany;`, `use super::CompletionError;`, `use crate::json_utils;`, `use crate::completion::CompletionError;`

- [ ] **Step 3: Move OneOrMany to protocol crate**

Create `crates/protocol/src/one_or_many.rs` by copying the full content from `crates/provider/src/one_or_many.rs`.

Update imports inside: remove any provider-specific references. The `OneOrMany` type has a dependency on `serde` and `thiserror` (for `EmptyListError`) — both are already protocol deps.

- [ ] **Step 4: Write protocol message.rs**

The message types need to be self-contained in protocol. They reference `CompletionError` and `PromptError` which stay in provider — those `From` impls should also stay in provider (they convert provider error types). The protocol message types themselves should NOT reference provider types.

Write `crates/protocol/src/message.rs` with the complete message type definitions from provider, but:
- Remove `use crate::OneOrMany;` → `use crate::one_or_many::OneOrMany;`
- Remove error `From` impls that reference provider types (`CompletionError`, `PromptError`, `StructuredOutputError`)
- Keep `MessageError` since it's defined within message.rs itself
- Keep all `From<String>`, `From<&str>` impls
- Keep all serde derives
- Keep `MimeType` trait

- [ ] **Step 5: Update protocol lib.rs**

Add before the module declarations:
```rust
pub mod one_or_many;
```

- [ ] **Step 6: Update provider Cargo.toml to depend on protocol**

Add to `crates/provider/Cargo.toml` under `[dependencies]`:
```toml
clawcode-protocol = { path = "../protocol" }
```

- [ ] **Step 7: Update provider message.rs to re-export from protocol**

Replace `crates/provider/src/completion/message.rs` content:

```rust
//! Provider-agnostic chat message types.
//!
//! These types are defined in `clawcode-protocol` and re-exported here
//! for backward compatibility.

pub use clawcode_protocol::message::*;
```

- [ ] **Step 8: Update provider one_or_many.rs to re-export from protocol**

Replace `crates/provider/src/one_or_many.rs` content:

```rust
//! OneOrMany container type, re-exported from protocol.
pub use clawcode_protocol::one_or_many::*;
```

- [ ] **Step 9: Move error From impls back to provider**

The `impl From<MessageError> for CompletionError` and similar impls that reference provider error types need to stay in provider. Create a new block in provider (or keep it near the re-exports):

In `crates/provider/src/completion/message.rs`, after the re-export:

```rust
// Provider-specific From impls that reference CompletionError.
use crate::completion::CompletionError;

impl From<MessageError> for CompletionError {
    fn from(error: MessageError) -> Self {
        CompletionError::RequestError(error.into())
    }
}
```

- [ ] **Step 10: Update all provider files that import from message**

In each of these files, replace `use crate::completion::message::*;` or individual imports with `use clawcode_protocol::message::*;` or the protocol path:

- `crates/provider/src/completion/request.rs`
- `crates/provider/src/completion/mod.rs`
- `crates/provider/src/factory/event.rs`
- `crates/provider/src/factory/mod.rs`
- `crates/provider/src/lib.rs`
- `crates/provider/src/streaming.rs`
- `crates/provider/src/providers/anthropic/completion.rs`
- `crates/provider/src/providers/anthropic/streaming.rs`
- `crates/provider/src/providers/openai/completion/mod.rs`
- `crates/provider/src/providers/openai/responses_api/mod.rs`
- `crates/provider/src/providers/openai/responses_api/streaming.rs`
- `crates/provider/src/providers/chatgpt/mod.rs`
- `crates/provider/src/providers/deepseek.rs`
- `crates/provider/src/providers/moonshot.rs`
- `crates/provider/src/providers/minimax.rs`
- `crates/provider/src/providers/xiaomimimo.rs`
- `crates/provider/src/providers/internal/buffered.rs`
- `crates/provider/src/providers/internal/openai_chat_completions_compatible.rs`
- `crates/provider/examples/deepseek.rs`
- `crates/provider/examples/deepseek_stream.rs`
- `crates/provider/examples/factory_say_hi.rs`
- `crates/provider/examples/factory_say_hi_stream.rs`

- [ ] **Step 11: Fix lib.rs self-re-export**

In provider's `lib.rs`, change:
```rust
pub use completion::message;
```
to:
```rust
// Re-export message types from protocol for backward compatibility
pub use clawcode_protocol::{message, one_or_many};
```

- [ ] **Step 12: Build entire workspace to verify**

```bash
cargo check
```

Fix any import errors.

- [ ] **Step 13: Run provider tests**

```bash
cargo test -p provider
```

- [ ] **Step 14: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor(protocol): move Message and OneOrMany types from provider to protocol
EOF
)"
```

---

### Task 9: Implement protocol event types

**Files:**
- Create: `crates/protocol/src/event.rs`

- [ ] **Step 1: Write event.rs**

```rust
//! Streaming event types emitted from the kernel to the frontend.

use std::path::PathBuf;

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
        /// Owning session.
        session_id: SessionId,
        /// Incremental text to append.
        text: String,
    },
    /// Reasoning / thinking delta from the assistant.
    AgentThoughtChunk {
        /// Owning session.
        session_id: SessionId,
        /// Incremental thinking text to append.
        text: String,
    },
    /// A tool call was initiated by the assistant.
    ToolCall {
        /// Owning session.
        session_id: SessionId,
        /// Agent that made the tool call (relevant for sub-agents).
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
        /// Owning session.
        session_id: SessionId,
        /// The tool call being updated.
        call_id: String,
        /// New output delta to append to the tool's terminal/log view.
        output_delta: Option<String>,
        /// Updated status, if changed.
        status: Option<ToolCallStatus>,
    },
    /// The agent's execution plan was created or updated.
    PlanUpdate {
        /// Owning session.
        session_id: SessionId,
        /// Complete list of plan entries (replaces previous plan).
        entries: Vec<PlanEntry>,
    },
    /// Token usage information for the current turn.
    UsageUpdate {
        /// Owning session.
        session_id: SessionId,
        /// Number of input (prompt) tokens consumed.
        input_tokens: u64,
        /// Number of output (completion) tokens produced.
        output_tokens: u64,
    },
    /// The kernel is requesting user permission for a tool execution.
    PermissionRequested {
        /// Owning session.
        session_id: SessionId,
        /// The permission request details.
        request: PermissionRequest,
    },
    /// A sub-agent's runtime status changed.
    AgentStatusChange {
        /// Owning (parent) session.
        session_id: SessionId,
        /// The agent whose status changed.
        agent_path: AgentPath,
        /// New status.
        status: AgentStatus,
    },
    /// The current turn has completed.
    TurnComplete {
        /// Owning session.
        session_id: SessionId,
        /// Reason the turn stopped.
        stop_reason: StopReason,
    },
}

/// Reason a turn completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Turn finished naturally (model returned a final response).
    EndTurn,
    /// Turn was cancelled by the user.
    Cancelled,
    /// Turn terminated due to an error.
    Error,
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/event.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add streaming event types
EOF
)"
```

---

### Task 10: Implement protocol op types

**Files:**
- Create: `crates/protocol/src/op.rs`

- [ ] **Step 1: Write op.rs**

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
/// Responses come back as streaming [`Event`](crate::event::Event)s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Create a new session at the given working directory.
    NewSession {
        /// Working directory for the session.
        cwd: PathBuf,
    },
    /// Load a previously persisted session.
    LoadSession {
        /// Session to load.
        session_id: SessionId,
    },
    /// Submit a user prompt to an active session.
    Prompt {
        /// Target session.
        session_id: SessionId,
        /// The user's message.
        message: Message,
    },
    /// Cancel the currently running turn in a session.
    Cancel {
        /// Target session.
        session_id: SessionId,
    },
    /// Change the session's approval/sandboxing mode.
    SetMode {
        /// Target session.
        session_id: SessionId,
        /// New mode identifier.
        mode: String,
    },
    /// Switch the model for a session.
    SetModel {
        /// Target session.
        session_id: SessionId,
        /// Provider identifier.
        provider_id: String,
        /// Model identifier within the provider.
        model_id: String,
    },
    /// Close a session and release its resources.
    CloseSession {
        /// Session to close.
        session_id: SessionId,
    },
    /// Spawn a sub-agent from a parent session.
    SpawnAgent {
        /// Parent session that owns the new agent.
        parent_session: SessionId,
        /// Hierarchical path for the new agent.
        agent_path: AgentPath,
        /// Role preset to apply ("explorer", "worker", etc.).
        role: String,
        /// Initial prompt for the sub-agent.
        prompt: String,
    },
    /// Deliver a message between agents.
    InterAgentMessage {
        /// Sender agent path.
        from: AgentPath,
        /// Recipient agent path.
        to: AgentPath,
        /// Message content.
        content: String,
    },
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/op.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add operation command types
EOF
)"
```

---

### Task 11: Implement kernel trait

**Files:**
- Create: `crates/protocol/src/kernel.rs`

- [ ] **Step 1: Write kernel.rs**

```rust
//! Agent kernel trait and associated error/stream types.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::agent::AgentPath;
use crate::config::{ModelInfo, SessionMode};
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
    ///
    /// The kernel allocates a session with the given working directory
    /// and optional MCP server configurations.
    async fn new_session(
        &self,
        cwd: PathBuf,
    ) -> Result<SessionCreated, KernelError>;

    /// Load a previously persisted session.
    ///
    /// Restores the session from its rollout history so the user can
    /// continue an earlier conversation.
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
    /// Callers should poll the stream until it returns `None`.
    async fn prompt(
        &self,
        session_id: &SessionId,
        message: Message,
    ) -> Result<EventStream, KernelError>;

    /// Cancel the currently running turn in a session.
    ///
    /// This interrupts the event stream returned by [`prompt`].
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
    ///
    /// After closing, the session ID is no longer valid.
    async fn close_session(&self, session_id: &SessionId) -> Result<(), KernelError>;

    /// Spawn a sub-agent in a parent session.
    ///
    /// The sub-agent runs independently and reports status changes
    /// via [`Event::AgentStatusChange`] on the parent's event stream.
    async fn spawn_agent(
        &self,
        parent_session: &SessionId,
        agent_path: AgentPath,
        role: &str,
        prompt: &str,
    ) -> Result<(), KernelError>;

    /// Get available modes for the kernel.
    ///
    /// Returns the list of approval/sandboxing mode presets the frontend
    /// can offer for session configuration.
    fn available_modes(&self) -> Vec<SessionMode>;

    /// Get available models from the configured providers.
    fn available_models(&self) -> Vec<ModelInfo>;
}

/// Error type for kernel operations.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    /// The requested session was not found (never existed or already closed).
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// The requested agent was not found.
    #[error("agent not found: {0}")]
    AgentNotFound(AgentPath),

    /// Authentication is required before this operation.
    #[error("authentication required")]
    AuthRequired,

    /// The operation was cancelled by user request.
    #[error("operation cancelled")]
    Cancelled,

    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}
```

- [ ] **Step 2: Build to verify**

```bash
cargo check -p clawcode-protocol
```

Expected: Protocol crate fully compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/protocol/src/kernel.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add AgentKernel trait and kernel error types
EOF
)"
```

---

### Task 12: Implement kernel crate

**Files:**
- Modify: `crates/kernel/Cargo.toml`
- Modify: `crates/kernel/src/lib.rs`

- [ ] **Step 1: Update kernel Cargo.toml**

Replace `crates/kernel/Cargo.toml`:

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

- [ ] **Step 2: Write kernel lib.rs**

Replace `crates/kernel/src/lib.rs`:

```rust
//! Clawcode agent kernel.
//!
//! Implements [`clawcode_protocol::AgentKernel`], orchestrating LLM
//! calls via [`clawcode_provider::LlmFactory`] and managing session state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, Stream};
use tokio::sync::Mutex;

use clawcode_protocol::{
    AgentKernel, AgentPath, AgentStatus, Event, KernelError, Message,
    ModelInfo, SessionCreated, SessionId, SessionInfo, SessionListPage,
    SessionMode, StopReason,
};
use clawcode_provider::factory::{ArcLlm, LlmFactory};
use config::ConfigHandle;

/// Central kernel struct that implements [`AgentKernel`].
///
/// Holds the LLM factory for model dispatch and a session registry
/// for tracking active sessions.
pub struct Kernel {
    /// Shared LLM factory for dispatching provider/model requests.
    llm_factory: Arc<LlmFactory>,
    /// Configuration handle for reading provider/model settings.
    config: ConfigHandle,
    /// Active sessions keyed by [`SessionId`].
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
}

/// Per-session runtime handle.
struct SessionHandle {
    /// Working directory for the session.
    cwd: PathBuf,
    /// Token used to signal cancellation.
    cancel_token: tokio::sync::watch::Sender<bool>,
}

impl Kernel {
    /// Create a new kernel instance.
    ///
    /// The kernel is initialized with the given LLM factory and configuration
    /// handle. No sessions are active on creation.
    #[must_use]
    pub fn new(llm_factory: Arc<LlmFactory>, config: ConfigHandle) -> Self {
        Self {
            llm_factory,
            config,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Build the list of session modes from configuration.
    fn build_modes(&self) -> Vec<SessionMode> {
        vec![
            SessionMode::builder()
                .id("read-only".to_string())
                .name("Read Only".to_string())
                .description(Some("Agent cannot modify files".to_string()))
                .build(),
            SessionMode::builder()
                .id("auto".to_string())
                .name("Auto".to_string())
                .description(Some("Agent asks for approval before making changes".to_string()))
                .build(),
            SessionMode::builder()
                .id("full-access".to_string())
                .name("Full Access".to_string())
                .description(Some("Agent can modify files without approval".to_string()))
                .build(),
        ]
    }

    /// Build the list of available models from the LLM configuration.
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
    async fn new_session(
        &self,
        cwd: PathBuf,
    ) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let (cancel_tx, _) = tokio::sync::watch::channel(false);

        let handle = SessionHandle {
            cwd: cwd.clone(),
            cancel_token: cancel_tx,
        };

        self.sessions.lock().await.insert(session_id.clone(), handle);

        Ok(SessionCreated::builder()
            .session_id(session_id)
            .modes(self.build_modes())
            .models(self.build_models())
            .build())
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionCreated, KernelError> {
        // Check if session exists in our registry
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }

        Ok(SessionCreated::builder()
            .session_id(session_id.clone())
            .modes(self.build_modes())
            .models(self.build_models())
            .build())
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
            .map(|(id, handle)| {
                SessionInfo::builder()
                    .session_id(id.clone())
                    .cwd(handle.cwd.clone())
                    .build()
            })
            .collect();

        Ok(SessionListPage::builder()
            .sessions(sessions)
            .build())
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        message: Message,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>,
        KernelError,
    > {
        // Verify session exists
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }

        // For now return a simple stream that echoes back and completes
        // Full LLM integration will be added in a subsequent plan
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
        if let Some(handle) = self.sessions.lock().await.get(session_id) {
            // Signal cancellation
            let _ = handle.cancel_token.send(true);
            Ok(())
        } else {
            Err(KernelError::SessionNotFound(session_id.clone()))
        }
    }

    async fn set_mode(
        &self,
        session_id: &SessionId,
        _mode: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        }
        // Mode changes will be wired in subsequent implementation
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
        // Model changes will be wired in subsequent implementation
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
        // Sub-agent spawning will be implemented in a subsequent plan
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

- [ ] **Step 3: Build to verify**

```bash
cargo check -p kernel
```

- [ ] **Step 4: Commit**

```bash
git add crates/kernel/
git commit -m "$(cat <<'EOF'
feat(kernel): implement AgentKernel trait with session management
EOF
)"
```

---

### Task 13: Create ACP bridge crate

**Files:**
- Create: `crates/acp/Cargo.toml`
- Create: `crates/acp/src/lib.rs`
- Create: `crates/acp/src/agent.rs`
- Create: `crates/acp/src/translate.rs`

- [ ] **Step 1: Create ACP crate Cargo.toml**

Create `crates/acp/Cargo.toml`:

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

- [ ] **Step 2: Create ACP lib.rs**

Create `crates/acp/src/lib.rs`:

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

use clawcode_kernel::Kernel;
use clawcode_protocol::AgentKernel;
use clawcode_provider::factory::LlmFactory;

/// Start the ACP agent over stdio transport.
///
/// This function initializes tracing, builds the ACP agent,
/// and blocks until the transport closes.
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

- [ ] **Step 3: Create ACP translate.rs**

Create `crates/acp/src/translate.rs`:

```rust
//! Type translation from clawcode internal types to ACP schema types.
//!
//! All conversions use the `From` trait with move semantics
//! to avoid unnecessary memory copies.

use agent_client_protocol as acp;
use acp::schema;

// ── StopReason ──

impl From<clawcode_protocol::StopReason> for schema::StopReason {
    fn from(r: clawcode_protocol::StopReason) -> Self {
        match r {
            clawcode_protocol::StopReason::EndTurn => Self::EndTurn,
            clawcode_protocol::StopReason::Cancelled => Self::Cancelled,
            clawcode_protocol::StopReason::Error => Self::Error,
        }
    }
}

// ── ToolCallStatus ──

impl From<clawcode_protocol::ToolCallStatus> for schema::ToolCallStatus {
    fn from(s: clawcode_protocol::ToolCallStatus) -> Self {
        match s {
            clawcode_protocol::ToolCallStatus::Pending => Self::Pending,
            clawcode_protocol::ToolCallStatus::InProgress => Self::InProgress,
            clawcode_protocol::ToolCallStatus::Completed => Self::Completed,
            clawcode_protocol::ToolCallStatus::Failed => Self::Failed,
        }
    }
}

// ── PlanPriority ──

impl From<clawcode_protocol::PlanPriority> for schema::PlanEntryPriority {
    fn from(p: clawcode_protocol::PlanPriority) -> Self {
        match p {
            clawcode_protocol::PlanPriority::Low => Self::Low,
            clawcode_protocol::PlanPriority::Medium => Self::Medium,
            clawcode_protocol::PlanPriority::High => Self::High,
        }
    }
}

// ── PlanStatus ──

impl From<clawcode_protocol::PlanStatus> for schema::PlanEntryStatus {
    fn from(s: clawcode_protocol::PlanStatus) -> Self {
        match s {
            clawcode_protocol::PlanStatus::Pending => Self::Pending,
            clawcode_protocol::PlanStatus::InProgress => Self::InProgress,
            clawcode_protocol::PlanStatus::Completed => Self::Completed,
        }
    }
}

// ── PermissionOptionKind ──

impl From<clawcode_protocol::PermissionOptionKind> for schema::PermissionOptionKind {
    fn from(k: clawcode_protocol::PermissionOptionKind) -> Self {
        match k {
            clawcode_protocol::PermissionOptionKind::AllowOnce => Self::AllowOnce,
            clawcode_protocol::PermissionOptionKind::AllowAlways => Self::AllowAlways,
            clawcode_protocol::PermissionOptionKind::RejectOnce => Self::RejectOnce,
            clawcode_protocol::PermissionOptionKind::RejectAlways => Self::RejectAlways,
        }
    }
}
```

- [ ] **Step 4: Create ACP agent.rs**

Create `crates/acp/src/agent.rs`:

```rust
//! ACP Agent implementation bridging the clawcode kernel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acp::schema::{
    AgentAuthCapabilities, AgentCapabilities, AuthMethod, AuthMethodEnvVar,
    AuthEnvVar, AuthMethodId, AuthenticateRequest, AuthenticateResponse,
    CancelNotification, ClientCapabilities, CloseSessionRequest,
    CloseSessionResponse, Implementation, InitializeRequest,
    InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    LogoutCapabilities, LogoutRequest, LogoutResponse, McpCapabilities,
    NewSessionRequest, NewSessionResponse, PromptCapabilities,
    PromptRequest, PromptResponse, SessionCapabilities, SessionCloseCapabilities,
    SessionListCapabilities, SessionId as AcpSessionId, SessionInfo as AcpSessionInfo,
    SessionMode as AcpSessionMode, ModelInfo as AcpModelInfo,
    SetSessionModeRequest, SetSessionModeResponse,
    SetSessionModelRequest, SetSessionModelResponse,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use acp::{Agent, Client, ConnectTo, ConnectionTo, Error};
use agent_client_protocol as acp;

use clawcode_protocol::{AgentKernel, SessionId};
use clawcode_provider::factory::LlmFactory;

use crate::translate::*;

/// ACP Agent bridging the clawcode kernel to the ACP protocol.
///
/// Wraps a kernel reference and LLM factory, registers handlers
/// for all ACP request/notification methods, and translates
/// between ACP schema types and internal protocol types.
pub struct ClawcodeAgent {
    kernel: Arc<dyn AgentKernel>,
    llm_factory: Arc<LlmFactory>,
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

    /// Convert an internal `SessionId` to an ACP `SessionId`.
    fn to_acp_session_id(id: &SessionId) -> AcpSessionId {
        AcpSessionId::new(id.0.clone())
    }

    /// Build and serve the ACP agent over the given transport.
    ///
    /// Registers handlers for all supported ACP methods and connects
    /// to the transport. Blocks until the transport closes.
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
                            if let Err(e) = agent.handle_cancel(notification).await {
                                tracing::error!("Error handling cancel: {:?}", e);
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

        let agent_capabilities = AgentCapabilities::new()
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

        let agent_capabilities = agent_capabilities;
        let mut caps = agent_capabilities;
        caps.session_capabilities = SessionCapabilities::new()
            .close(SessionCloseCapabilities::new())
            .list(SessionListCapabilities::new());

        Ok(InitializeResponse::new(protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                Implementation::new("clawcode-acp", env!("CARGO_PKG_VERSION"))
                    .title("Clawcode"),
            ))
    }

    async fn handle_authenticate(
        &self,
        _request: AuthenticateRequest,
    ) -> Result<AuthenticateResponse, Error> {
        // For now, authentication is a no-op
        // Future: integrate API key validation
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
        let session_id = SessionId(request.session_id.0.clone());

        let message = clawcode_protocol::Message::user("prompt");
        let stop_reason = self
            .kernel
            .prompt(&session_id, message)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        // TODO: Full event translation loop — consume EventStream
        // and convert each Event to ACP SessionUpdate notifications
        drop(stop_reason);

        Ok(PromptResponse::new(acp::schema::StopReason::EndTurn))
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
        // model_id format is "provider_id/model_id"
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

- [ ] **Step 5: Create ACP main.rs**

Create `crates/acp/src/main.rs`:

```rust
//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use config::ConfigHandle;
use clawcode_kernel::Kernel;
use clawcode_provider::factory::LlmFactory;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = ConfigHandle::default();
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let kernel = Arc::new(Kernel::new(llm_factory.clone(), config));

    clawcode_acp::run(kernel, llm_factory).await
}
```

- [ ] **Step 6: Build entire workspace**

```bash
cargo check
```

Fix any compilation errors.

- [ ] **Step 7: Commit**

```bash
git add crates/acp/ crates/kernel/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(acp): add ACP bridge crate with handler registration and type translations
EOF
)"
```

---

### Task 14: Final workspace verification

- [ ] **Step 1: Build entire workspace**

```bash
cargo build
```

- [ ] **Step 2: Run all tests**

```bash
cargo test
```

- [ ] **Step 3: Run clippy**

```bash
cargo clippy -- -D warnings
```

- [ ] **Step 4: Commit any fixes**

```bash
git add -A && git commit -m "$(cat <<'EOF'
chore: fix clippy warnings and test failures after protocol implementation
EOF
)"
```

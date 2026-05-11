# Session/Turn 执行层实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 在 kernel crate 中实现 Channel + 后台 Task 架构的 session 管理、turn 流式执行、对话历史管理和工具注册。

**架构：** 每个 session 一个 `tokio::spawn` 后台 task，通过 `mpsc::UnboundedSender/Receiver` 收发 `Op`/`Event`。`execute_turn()` 组装 `CompletionRequest` → `ArcLlm::stream()` → `LlmStreamEvent` → `protocol::Event`。工具调用在 stream 消费过程中内联执行。

**技术栈：** tokio (mpsc, watch, spawn, CancellationToken), async-trait, async-stream, futures, provider (LlmFactory, ArcLlm)

**规格文档：** `docs/superpowers/specs/2026-05-11-session-turn-design.md`

---

## 文件清单

| 文件 | 操作 | 用途 |
|---|---|---|
| `crates/kernel/Cargo.toml` | 修改 | 添加 async-stream、async-trait 依赖 |
| `crates/kernel/src/context.rs` | 新建 | ContextManager trait + InMemoryContext |
| `crates/kernel/src/tool.rs` | 新建 | Tool trait + ToolRegistry + MockEchoTool |
| `crates/kernel/src/translate.rs` | 新建 | LlmStreamEvent → protocol::Event |
| `crates/kernel/src/turn.rs` | 新建 | TurnContext + execute_turn() |
| `crates/kernel/src/session.rs` | 新建 | SessionHandle + SessionRuntime + event_stream |
| `crates/kernel/src/lib.rs` | 修改 | Kernel 改为使用新 session 系统 |

---

### 任务 1：添加 kernel 依赖 + 准备

**文件：**
- 修改：`crates/kernel/Cargo.toml`

- [ ] **步骤1：更新 Cargo.toml，添加 async-stream 和 async-trait**

`async-trait` 已在 workspace 中，只需添加 `async-stream`。

在 `[dependencies]` 末尾追加：

```toml
async-stream = { workspace = true }
```

当前完整的 Cargo.toml：

```toml
[package]
name = "kernel"
edition.workspace = true
version.workspace = true
description = "Clawcode agent kernel - session management, LLM orchestration, tool execution"

[dependencies]
protocol = { path = "../protocol" }
provider = { path = "../provider" }
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
async-stream = { workspace = true }
```

- [ ] **步骤2：构建验证**

```bash
cargo check -p kernel
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(kernel): add async-stream dependency for event streams
EOF
)"
```

---

### 任务 2：实现 ContextManager

**文件：**
- 创建：`crates/kernel/src/context.rs`

- [ ] **步骤1：编写 context.rs**

```rust
//! Conversation history management for sessions.

use std::pin::Pin;

use futures::future::BoxFuture;
use protocol::message::Message;
use provider::factory::Llm;

/// Manages conversation history for a session.
///
/// Implementations range from in-memory `Vec<Message>` to persistent storage
/// with automatic compaction. The compaction interface is reserved for
/// future implementation.
pub trait ContextManager: Send + Sync {
    /// Append a message to the conversation history.
    fn push(&mut self, msg: Message);

    /// Return all messages in the current history, oldest first.
    fn history(&self) -> Vec<Message>;

    /// Estimate total token count of the stored history.
    fn token_count(&self) -> usize;

    /// Clear all history.
    fn clear(&mut self);

    // ── Reserved for future compaction ──

    /// Returns `true` when compaction is recommended for this history.
    /// Default implementation returns `false`.
    fn should_compact(&self) -> bool {
        false
    }

    /// Compact the history by summarizing older messages.
    /// Default implementation is a no-op.
    fn compact(
        &mut self,
        _llm: &dyn Llm,
    ) -> BoxFuture<'_, Result<(), anyhow::Error>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// In-memory implementation of [`ContextManager`] backed by `Vec<Message>`.
pub struct InMemoryContext {
    messages: Vec<Message>,
}

impl InMemoryContext {
    /// Create a new empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }
}

impl Default for InMemoryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextManager for InMemoryContext {
    fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    fn history(&self) -> Vec<Message> {
        self.messages.clone()
    }

    fn token_count(&self) -> usize {
        // Rough estimate: ~4 characters per token
        self.messages
            .iter()
            .map(|m| format!("{m:?}").len() / 4)
            .sum()
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_context_push_and_history() {
        let mut ctx = InMemoryContext::new();
        assert!(ctx.history().is_empty());

        let msg = Message::user("hello");
        ctx.push(msg.clone());
        assert_eq!(ctx.history().len(), 1);
    }

    #[test]
    fn in_memory_context_clear() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("hello"));
        ctx.clear();
        assert!(ctx.history().is_empty());
    }

    #[test]
    fn in_memory_context_token_count_is_nonzero_after_push() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("hello world"));
        assert!(ctx.token_count() > 0);
    }

    #[test]
    fn default_should_compact_returns_false() {
        let ctx = InMemoryContext::new();
        assert!(!ctx.should_compact());
    }
}
```

- [ ] **步骤2：构建并运行测试**

```bash
cargo test -p kernel -- context
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/context.rs
git commit -m "$(cat <<'EOF'
feat(kernel): add ContextManager trait and InMemoryContext
EOF
)"
```

---

### 任务 3：实现 Tool trait + ToolRegistry + Mock

**文件：**
- 创建：`crates/kernel/src/tool.rs`

- [ ] **步骤1：编写 tool.rs**

```rust
//! Tool registration and execution for agent turns.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

/// A tool that can be invoked by the LLM during a turn.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name exposed to the model.
    fn name(&self) -> &str;

    /// Human-readable description sent to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON arguments.
    /// Returns the output string on success, or an error message on failure.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        cwd: &Path,
    ) -> Result<String, String>;
}

/// Registry of available tools, keyed by tool name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Build tool definitions for the LLM completion request.
    #[must_use]
    pub fn definitions(&self) -> Vec<protocol::ToolDefinition> {
        self.tools
            .values()
            .map(|t| protocol::ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    }

    /// Execute a tool call by name.
    pub async fn execute(
        &self,
        name: &str,
        arguments: serde_json::Value,
        cwd: &Path,
    ) -> Result<String, String> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(arguments, cwd).await,
            None => Err(format!("unknown tool: {name}")),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Mock tools for testing ──

/// A mock tool that echoes its arguments — useful for testing the tool pipeline.
pub struct MockEchoTool {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
}

#[async_trait]
impl Tool for MockEchoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cwd: &Path,
    ) -> Result<String, String> {
        let msg = arguments["message"]
            .as_str()
            .unwrap_or("(no message)");
        Ok(format!("echo: {msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_register_and_definitions() {
        let mut reg = ToolRegistry::new();
        let tool = Arc::new(MockEchoTool {
            name: "echo".to_string(),
            description: "Echoes a message".to_string(),
        });
        reg.register(tool);
        let defs = reg.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn registry_execute_unknown_tool() {
        let reg = ToolRegistry::new();
        let result = reg
            .execute("nonexistent", serde_json::json!({}), Path::new("."))
            .await;
        assert!(result.is_err());
    }
}
```

- [ ] **步骤2：构建并运行测试**

```bash
cargo test -p kernel -- tool
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/tool.rs
git commit -m "$(cat <<'EOF'
feat(kernel): add Tool trait, ToolRegistry, and MockEchoTool
EOF
)"
```

---

### 任务 4：实现 translate.rs

**文件：**
- 创建：`crates/kernel/src/translate.rs`

- [ ] **步骤1：编写 translate.rs**

```rust
//! Translation of provider stream events into protocol events.

use protocol::{Event, SessionId};
use provider::factory::LlmStreamEvent;

/// Translate an [`LlmStreamEvent`] into an optional protocol [`Event`].
///
/// Returns `None` for ToolCall/ToolCallDelta events — those are handled
/// inline in [`super::turn::execute_turn`] because they require tool
/// execution before forwarding.
pub(crate) fn translate_stream_event(
    session_id: &SessionId,
    event: LlmStreamEvent,
) -> Option<Event> {
    match event {
        LlmStreamEvent::Text(text) => Some(Event::AgentMessageChunk {
            session_id: session_id.clone(),
            text: text.text,
        }),
        LlmStreamEvent::Reasoning(reasoning) => Some(Event::AgentThoughtChunk {
            session_id: session_id.clone(),
            text: reasoning.display_text(),
        }),
        LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
            Some(Event::AgentThoughtChunk {
                session_id: session_id.clone(),
                text: reasoning,
            })
        }
        LlmStreamEvent::Final { usage, .. } => usage.map(|u| Event::UsageUpdate {
            session_id: session_id.clone(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }),
        LlmStreamEvent::ToolCall { .. } | LlmStreamEvent::ToolCallDelta { .. } => {
            // Tool calls are handled inline in execute_turn
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::completion::message::Text;

    #[test]
    fn translate_text_event() {
        let sid = SessionId("test".to_string());
        let event = LlmStreamEvent::Text(Text {
            text: "hello".to_string(),
        });
        let result = translate_stream_event(&sid, event);
        match result {
            Some(Event::AgentMessageChunk { session_id, text }) => {
                assert_eq!(session_id.0, "test");
                assert_eq!(text, "hello");
            }
            _ => panic!("expected AgentMessageChunk"),
        }
    }

    #[test]
    fn translate_tool_call_returns_none() {
        let sid = SessionId("test".to_string());
        let event = LlmStreamEvent::ToolCall {
            tool_call: protocol::message::ToolCall {
                id: "1".to_string(),
                call_id: None,
                function: protocol::message::ToolFunction {
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                signature: None,
                additional_params: None,
            },
            internal_call_id: "ic1".to_string(),
        };
        let result = translate_stream_event(&sid, event);
        assert!(result.is_none());
    }

    #[test]
    fn translate_final_with_usage() {
        let sid = SessionId("test".to_string());
        let event = LlmStreamEvent::Final {
            raw: serde_json::json!({}),
            usage: Some(provider::completion::Usage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
        };
        let result = translate_stream_event(&sid, event);
        match result {
            Some(Event::UsageUpdate {
                session_id,
                input_tokens,
                output_tokens,
            }) => {
                assert_eq!(session_id.0, "test");
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 50);
            }
            _ => panic!("expected UsageUpdate"),
        }
    }
}
```

- [ ] **步骤2：构建并运行测试**

```bash
cargo test -p kernel -- translate
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/translate.rs
git commit -m "$(cat <<'EOF'
feat(kernel): add LlmStreamEvent to protocol Event translator
EOF
)"
```

---

### 任务 5：实现 turn.rs

**文件：**
- 创建：`crates/kernel/src/turn.rs`

- [ ] **步骤1：编写 turn.rs**

```rust
//! Turn execution — processes a single user prompt through the LLM.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;

use protocol::{
    AgentPath, Event, KernelError, SessionId, StopReason, ToolCallStatus,
};
use protocol::message::{AssistantContent, Message};
use protocol::one_or_many::OneOrMany;
use provider::completion::request::CompletionRequest;
use provider::factory::{ArcLlm, LlmStreamEvent};

use crate::context::ContextManager;
use crate::tool::ToolRegistry;
use crate::translate::translate_stream_event;

/// Immutable snapshot of all context needed to execute a single turn.
pub(crate) struct TurnContext {
    pub session_id: SessionId,
    pub llm: ArcLlm,
    pub tools: Arc<ToolRegistry>,
    pub cwd: PathBuf,
}

/// Execute a single turn: build the request, call the LLM, emit events,
/// execute tool calls inline, and save the assistant message to context.
///
/// # Errors
///
/// Returns `KernelError::Internal` if the LLM stream fails.
pub(crate) async fn execute_turn(
    ctx: &TurnContext,
    user_text: String,
    context: &mut Box<dyn ContextManager>,
    tx_event: &mpsc::UnboundedSender<Event>,
) -> Result<(), KernelError> {
    // 1. Build user message and push to context
    let user_msg = Message::user(user_text);
    context.push(user_msg.clone());

    // 2. Build CompletionRequest
    let history = context.history();
    let history = OneOrMany::many(history)
        .map_err(|e| KernelError::Internal(e.into()))?;

    let tool_defs: Vec<provider::completion::request::ToolDefinition> = ctx
        .tools
        .definitions()
        .into_iter()
        .map(|d| provider::completion::request::ToolDefinition {
            name: d.name,
            description: d.description,
            parameters: d.parameters,
        })
        .collect();

    let request = CompletionRequest {
        model: Some(ctx.llm.model_id().to_string()),
        preamble: None,
        chat_history: history,
        documents: Vec::new(),
        tools: tool_defs,
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    };

    // 3. Call LLM streaming API
    let mut stream = ctx.llm.stream(request).await.map_err(|e| {
        KernelError::Internal(anyhow::anyhow!("LLM stream error: {e}"))
    })?;

    // 4. Consume stream, translate events, execute tools inline
    let mut assistant_content: Vec<AssistantContent> = Vec::new();

    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| {
            KernelError::Internal(anyhow::anyhow!("Stream event error: {e}"))
        })?;

        match event {
            LlmStreamEvent::Text(text) => {
                assistant_content.push(AssistantContent::Text(text.clone()));
                if let Some(ev) = translate_stream_event(
                    &ctx.session_id,
                    LlmStreamEvent::Text(text),
                ) {
                    let _ = tx_event.send(ev);
                }
            }
            LlmStreamEvent::ToolCall {
                tool_call,
                internal_call_id,
            } => {
                // Emit InProgress event
                let _ = tx_event.send(Event::ToolCall {
                    session_id: ctx.session_id.clone(),
                    agent_path: AgentPath::root(),
                    call_id: internal_call_id.clone(),
                    name: tool_call.function.name.clone(),
                    arguments: tool_call.function.arguments.clone(),
                    status: ToolCallStatus::InProgress,
                });

                // Execute the tool
                let output = ctx
                    .tools
                    .execute(
                        &tool_call.function.name,
                        tool_call.function.arguments.clone(),
                        &ctx.cwd,
                    )
                    .await;

                match output {
                    Ok(out) => {
                        assistant_content
                            .push(AssistantContent::ToolCall(tool_call));
                        let _ = tx_event.send(Event::ToolCallUpdate {
                            session_id: ctx.session_id.clone(),
                            call_id: internal_call_id,
                            output_delta: Some(out),
                            status: Some(ToolCallStatus::Completed),
                        });
                    }
                    Err(err) => {
                        let _ = tx_event.send(Event::ToolCallUpdate {
                            session_id: ctx.session_id.clone(),
                            call_id: internal_call_id,
                            output_delta: Some(err),
                            status: Some(ToolCallStatus::Failed),
                        });
                    }
                }
            }
            LlmStreamEvent::Reasoning(reasoning) => {
                let _ = tx_event.send(Event::AgentThoughtChunk {
                    session_id: ctx.session_id.clone(),
                    text: reasoning.display_text(),
                });
            }
            LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                let _ = tx_event.send(Event::AgentThoughtChunk {
                    session_id: ctx.session_id.clone(),
                    text: reasoning,
                });
            }
            LlmStreamEvent::Final { usage, .. } => {
                if let Some(u) = usage {
                    let _ = tx_event.send(Event::UsageUpdate {
                        session_id: ctx.session_id.clone(),
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                    });
                }
            }
            _ => {}
        }
    }

    // 5. Save assistant message to context
    if !assistant_content.is_empty() {
        let assistant_msg = Message::Assistant {
            id: None,
            content: OneOrMany::many(assistant_content)
                .unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(""))),
        };
        context.push(assistant_msg);
    }

    Ok(())
}
```

- [ ] **步骤2：构建验证（暂不运行测试——turn 依赖 session 层）**

```bash
cargo check -p kernel
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/turn.rs
git commit -m "$(cat <<'EOF'
feat(kernel): add TurnContext and execute_turn with LLM streaming, tool execution
EOF
)"
```

---

### 任务 6：实现 session.rs

**文件：**
- 创建：`crates/kernel/src/session.rs`

- [ ] **步骤1：编写 session.rs**

```rust
//! Session lifecycle: channel-backed handles, background task, and event stream.

use std::path::PathBuf;
use std::pin::Pin;

use futures::Stream;
use tokio::sync::{mpsc, watch};

use protocol::{Event, KernelError, Op, SessionId, StopReason};
use provider::factory::ArcLlm;

use crate::context::ContextManager;
use crate::tool::ToolRegistry;
use crate::turn::{TurnContext, execute_turn};

/// Frontend handle for a live session.
///
/// Created by the kernel, held by callers for submitting operations
/// and consuming streaming events. Uses `Arc<Mutex<>>` for the event
/// receiver because `UnboundedReceiver` is not `Clone`.
#[derive(Clone)]
pub struct SessionHandle {
    pub session_id: SessionId,
    /// Send operations to the background task. `UnboundedSender` is `Clone`.
    pub(crate) tx_op: mpsc::UnboundedSender<Op>,
    /// Receive streaming events, shared behind `Arc<Mutex<>>` for cloneability.
    pub(crate) rx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Event>>>,
    /// Signal cancellation to the background task. `watch::Sender` is `Clone`.
    pub(crate) cancel_tx: watch::Sender<bool>,
}

impl SessionHandle {
    /// Take the event receiver out of the handle.
    /// Called once per prompt to create the event stream.
    pub(crate) async fn take_rx(
        &self,
    ) -> mpsc::UnboundedReceiver<Event> {
        let mut guard = self.rx_event.lock().await;
        std::mem::replace(
            &mut *guard,
            mpsc::unbounded_channel().1,
        )
    }
}

/// Runtime state owned by the background task of a single session.
pub(crate) struct SessionRuntime {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub rx_op: mpsc::UnboundedReceiver<Op>,
    pub tx_event: mpsc::UnboundedSender<Event>,
    pub cancel_rx: watch::Receiver<bool>,
    pub context: Box<dyn ContextManager>,
    pub llm: ArcLlm,
    pub tools: Arc<ToolRegistry>,
}

/// Spawn the background task for a session and return the frontend handle.
pub(crate) fn spawn_session(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
) -> SessionHandle {
    let (tx_op, rx_op) = mpsc::unbounded_channel();
    let (tx_event, rx_event) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);

    let runtime = SessionRuntime {
        session_id: session_id.clone(),
        cwd,
        rx_op,
        tx_event,
        cancel_rx,
        context,
        llm,
        tools,
    };

    tokio::spawn(run_session_loop(runtime));

    SessionHandle {
        session_id,
        tx_op,
        rx_event: Arc::new(tokio::sync::Mutex::new(rx_event)),
        cancel_tx,
    }
}

/// Background task: receive ops, execute turns, emit events.
async fn run_session_loop(mut rt: SessionRuntime) {
    loop {
        let op = rt.rx_op.recv().await;
        match op {
            Some(Op::Prompt { text, .. }) => {
                let ctx = TurnContext {
                    session_id: rt.session_id.clone(),
                    llm: rt.llm.clone(),
                    tools: rt.tools.clone(),
                    cwd: rt.cwd.clone(),
                };

                if let Err(e) = execute_turn(&ctx, text, &mut rt.context, &rt.tx_event).await {
                    let _ = rt.tx_event.send(Event::TurnComplete {
                        session_id: rt.session_id.clone(),
                        stop_reason: StopReason::Error,
                    });
                    tracing::error!(
                        session_id = %rt.session_id,
                        error = %e,
                        "Turn execution failed"
                    );
                } else {
                    let _ = rt.tx_event.send(Event::TurnComplete {
                        session_id: rt.session_id.clone(),
                        stop_reason: StopReason::EndTurn,
                    });
                }
            }
            Some(Op::Cancel { .. }) => {
                    let _ = rt.cancel_rx.changed().await;
                    // Cancel signal handled via watch channel
            }
            Some(Op::Shutdown) | None => break,
            _ => {
                // Other ops (SetMode, SetModel, etc.) handled in future plans
            }
        }
    }
}

/// Build an [`EventStream`] from the session's event receiver and cancel watch.
///
/// The stream terminates when `TurnComplete` arrives or cancellation is signaled.
pub(crate) fn event_stream(
    mut rx_event: mpsc::UnboundedReceiver<Event>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>> {
    Box::pin(async_stream::stream! {
        loop {
            tokio::select! {
                event = rx_event.recv() => {
                    match event {
                        Some(Event::TurnComplete { .. }) => {
                            yield Ok(event);
                            break;
                        }
                        Some(e) => yield Ok(e),
                        None => break,
                    }
                }
                _ = cancel_rx.changed() => {
                    yield Err(KernelError::Cancelled);
                    break;
                }
            }
        }
    })
}
```

- [ ] **步骤2：构建验证**

```bash
cargo check -p kernel
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/session.rs
git commit -m "$(cat <<'EOF'
feat(kernel): add SessionHandle, SessionRuntime, and background event loop
EOF
)"
```

---

### 任务 7：重写 kernel lib.rs

**文件：**
- 修改：`crates/kernel/src/lib.rs`

- [ ] **步骤1：重写 lib.rs**

```rust
//! Clawcode agent kernel.
//!
//! Implements [`protocol::AgentKernel`], orchestrating LLM
//! calls via the provider factory and managing session state.

mod context;
mod session;
mod tool;
mod translate;
mod turn;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use protocol::{
    AgentKernel, AgentPath, Event, KernelError, ModelInfo,
    SessionCreated, SessionId, SessionInfo, SessionListPage,
    SessionMode, StopReason,
};
use provider::factory::LlmFactory;
use config::ConfigHandle;

use crate::context::InMemoryContext;
use crate::session::{SessionHandle, event_stream, spawn_session};
use crate::tool::ToolRegistry;

/// Central kernel struct implementing [`AgentKernel`].
pub struct Kernel {
    /// Shared LLM factory for dispatching provider/model requests.
    llm_factory: Arc<LlmFactory>,
    /// Configuration handle for reading provider/model settings.
    config: ConfigHandle,
    /// Registered tools available to every session.
    tools: Arc<ToolRegistry>,
    /// Active sessions keyed by [`SessionId`].
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
}

impl Kernel {
    /// Create a new kernel instance with the given LLM factory,
    /// config, and tool registry.
    #[must_use]
    pub fn new(
        llm_factory: Arc<LlmFactory>,
        config: ConfigHandle,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            llm_factory,
            config,
            tools,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the default LLM handle from configuration.
    fn default_llm(&self) -> Option<Arc<provider::factory::Llm>> {
        let cfg = self.config.current();
        let provider = cfg.providers.first()?;
        let model = provider.models.first()?;
        self.llm_factory.get(provider.id.as_str(), &model.id)
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
                description: Some(
                    "Agent asks for approval before making changes"
                        .to_string(),
                ),
            },
            SessionMode {
                id: "full-access".to_string(),
                name: "Full Access".to_string(),
                description: Some(
                    "Agent can modify files without approval"
                        .to_string(),
                ),
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
                        .display_name(
                            m.display_name
                                .clone()
                                .unwrap_or_else(|| m.id.clone()),
                        )
                        .description(None)
                        .context_tokens(m.context_tokens)
                        .max_output_tokens(m.max_output_tokens)
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
        let llm = self
            .default_llm()
            .ok_or_else(|| {
                KernelError::Internal(anyhow::anyhow!(
                    "no LLM configured"
                ))
            })?;

        let handle = spawn_session(
            session_id.clone(),
            cwd.clone(),
            llm,
            self.tools.clone(),
            Box::new(InMemoryContext::new()),
        );

        let modes = self.build_modes();
        let models = self.build_models();

        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), handle);

        Ok(SessionCreated {
            session_id,
            modes,
            models,
        })
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionCreated, KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(
                session_id.clone(),
            ));
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
            .map(|(id, _handle)| {
                SessionInfo::builder()
                    .session_id(id.clone())
                    .cwd(PathBuf::from("."))
                    .build()
            })
            .collect();

        Ok(SessionListPage {
            sessions,
            next_cursor: None,
        })
    }

    async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>>,
        KernelError,
    > {
        let handle = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .ok_or_else(|| {
                KernelError::SessionNotFound(session_id.clone())
            })?
            .clone();

        // Send the prompt to the background task
        let _ = handle.tx_op.send(protocol::Op::Prompt {
            session_id: session_id.clone(),
            text,
        });

        // Take the receiver from the handle and build stream
        let rx_event = handle.take_rx().await;
        Ok(event_stream(rx_event, handle.cancel_rx))
    }

    async fn cancel(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        match self.sessions.lock().await.get(session_id) {
            Some(handle) => {
                let _ = handle.cancel_tx.send(true);
                Ok(())
            }
            None => Err(KernelError::SessionNotFound(
                session_id.clone(),
            )),
        }
    }

    async fn set_mode(
        &self,
        session_id: &SessionId,
        _mode: &str,
    ) -> Result<(), KernelError> {
        if !self.sessions.lock().await.contains_key(session_id) {
            return Err(KernelError::SessionNotFound(
                session_id.clone(),
            ));
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
            return Err(KernelError::SessionNotFound(
                session_id.clone(),
            ));
        }
        Ok(())
    }

    async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let handle = self
            .sessions
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| {
                KernelError::SessionNotFound(session_id.clone())
            })?;
        // Signal shutdown to the background task
        let _ = handle.tx_op.send(protocol::Op::Shutdown);
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
            return Err(KernelError::SessionNotFound(
                parent_session.clone(),
            ));
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

- [ ] **步骤2：构建验证**

```bash
cargo check -p kernel
```

- [ ] **步骤3：提交**

```bash
git add crates/kernel/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(kernel): rewrite Kernel with channel-based session management
EOF
)"
```

---

### 任务 8：更新 acp main.rs 适配新 Kernel 构造

**文件：**
- 修改：`crates/acp/src/main.rs`

- [ ] **步骤1：更新 main.rs**

Kernel 构造函数新增了 `tools: Arc<ToolRegistry>` 参数。

修改 `crates/acp/src/main.rs`：

```rust
//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use config::{AppConfig, ConfigHandle};
use kernel::Kernel;
use provider::factory::LlmFactory;
use kernel::tool::ToolRegistry;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = ConfigHandle::from_config(AppConfig::default());
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let tools = Arc::new(ToolRegistry::new());
    let kernel = Arc::new(Kernel::new(llm_factory.clone(), config, tools));

    acp::run(kernel, llm_factory).await
}
```

- [ ] **步骤2：构建全 workspace**

```bash
cargo build
```

- [ ] **步骤3：运行全部测试**

```bash
cargo test
```

- [ ] **步骤4：运行 clippy**

```bash
cargo clippy -- -D warnings
```

- [ ] **步骤5：提交修复**

```bash
git add crates/acp/src/main.rs
git commit -m "$(cat <<'EOF'
chore(acp): update main.rs for new Kernel constructor signature
EOF
)"
```

# Session/Turn 执行层设计方案

## 目标

以 Codex 的 Channel + 后台 Task 架构为参考，在 clawcode kernel crate 中实现 session 生命周期管理、turn 流式执行、对话历史管理、工具注册与执行。消息类型已在 protocol crate 中，LLM 调用通过 provider crate 的 `LlmFactory` / `ArcLlm`。

## 架构概览

```
┌─────────────────────────────────────┐
│              Kernel                   │
│  sessions: HashMap<SessionId,        │
│                    SessionHandle>     │
│  llm_factory: Arc<LlmFactory>        │
│  tools: Arc<ToolRegistry>            │
└──────────────┬──────────────────────┘
               │ new_session() 创建 SessionRuntime + spawn 后台 task
               ▼
┌──────────────────────────────────────┐
│          SessionHandle                │
│                                       │
│  session_id: SessionId                │
│  tx_op: UnboundedSender<Op>     ──→  │
│  rx_event: UnboundedReceiver<Event> ←── │
│  cancel_tx: watch::Sender<bool>       │
└──────────────┬──────────────────────┘
               │
               ▼
┌──────────────────────────────────────┐
│         SessionRuntime                │  后台 tokio task
│                                       │
│  rx_op, tx_event, cancel_rx          │
│  context: Box<dyn ContextManager>    │
│  llm: ArcLlm                         │
│  tools: Arc<ToolRegistry>            │
└──────────────┬──────────────────────┘
               │ Op::Prompt
               ▼
┌──────────────────────────────────────┐
│          execute_turn()               │
│                                       │
│  1. 构建 TurnContext 快照              │
│  2. 组装 CompletionRequest            │
│  3. llm.stream() → LlmStreamEvent    │
│  4. 翻译 → protocol::Event            │
│  5. 工具调用拦截执行                   │
│  6. 保存历史到 ContextManager          │
└──────────────────────────────────────┘
```

## 文件结构

```
crates/kernel/src/
├── lib.rs              # Kernel struct + AgentKernel impl（修改）
├── session.rs          # SessionHandle, SessionRuntime, 后台 event loop（新增）
├── turn.rs             # TurnContext, execute_turn()（新增）
├── context.rs          # ContextManager trait + InMemoryContext（新增）
├── tool.rs             # Tool trait + ToolRegistry + MockEchoTool（新增）
└── translate.rs        # LlmStreamEvent → protocol::Event（新增）
```

| 文件 | 职责 | 公开 API |
|---|---|---|
| `session.rs` | session 生命周期、channel 收发 | `SessionHandle`, `SessionRuntime` |
| `turn.rs` | 单轮 turn 执行 | `TurnContext`, `execute_turn()` |
| `context.rs` | 对话历史管理 | `ContextManager` trait, `InMemoryContext` |
| `tool.rs` | 工具注册/执行 | `Tool` trait, `ToolRegistry`, `MockEchoTool` |
| `translate.rs` | 事件翻译 | `translate_stream_event()` |

## 核心类型

### session.rs

```rust
/// Frontend handle for a live session.
/// Created by Kernel::new_session(), held by callers for Op submission and Event consumption.
pub struct SessionHandle {
    pub session_id: SessionId,
    /// Send operations to the background task.
    pub(crate) tx_op: mpsc::UnboundedSender<Op>,
    /// Receive streaming events from the background task.
    pub(crate) rx_event: mpsc::UnboundedReceiver<Event>,
    /// Signal cancellation to the background task.
    pub(crate) cancel_tx: watch::Sender<bool>,
}

/// Runtime state owned by the background task of a session.
pub(crate) struct SessionRuntime {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    /// Receive operations from the frontend.
    pub rx_op: mpsc::UnboundedReceiver<Op>,
    /// Send events to the frontend.
    pub tx_event: mpsc::UnboundedSender<Event>,
    /// Listen for cancellation signals.
    pub cancel_rx: watch::Receiver<bool>,
    /// Conversation history for this session.
    pub context: Box<dyn ContextManager>,
    /// LLM handle for model dispatch.
    pub llm: ArcLlm,
    /// Available tools for this session.
    pub tools: Arc<ToolRegistry>,
}
```

### turn.rs

```rust
/// Immutable snapshot of all context needed to execute a single turn.
pub(crate) struct TurnContext {
    pub session_id: SessionId,
    pub llm: ArcLlm,
    pub tools: Arc<ToolRegistry>,
    pub cwd: PathBuf,
    /// Cancellation token scoped to this turn.
    pub cancel: CancellationToken,
}
```

## Channel 架构与事件循环

### Kernel::prompt() 流程

```
Kernel::prompt(session_id, text)
  │
  ├─ handle = sessions.get(&session_id)
  ├─ handle.tx_op.send(Op::Prompt { text })
  │
  └─ return EventStream(handle.rx_event)
       // EventStream 消费 rx_event 直到 TurnComplete / Cancelled
```

### 后台 task 事件循环

```
loop {
    match rx_op.recv().await {
        Op::Prompt { text } => {
            let ctx = TurnContext { llm, tools, cwd, cancel, ... };
            execute_turn(ctx, text, &mut runtime.context, &tx_event).await;
            tx_event.send(Event::TurnComplete { session_id, stop_reason: EndTurn });
        }
        Op::Cancel => { cancel_tx.send(true); }
        Op::SetMode { mode } => { /* 更新 session 本地 mode */ }
        Op::SetModel { provider_id, model_id } => {
            // 从 LlmFactory 查找新模型
            runtime.llm = factory.get(&provider_id, &model_id).ok_or(...);
        }
        Op::Shutdown => break,
    }
}
```

### EventStream 实现

```rust
/// Convert mpsc receiver + cancel watch into a pinned EventStream.
/// Terminates when TurnComplete arrives or cancellation is signaled.
fn event_stream(
    mut rx_event: mpsc::UnboundedReceiver<Event>,
    mut cancel_rx: watch::Receiver<bool>,
) -> EventStream {
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

## ContextManager

```rust
/// Manages conversation history for a session.
///
/// Implementations range from in-memory Vec to persistent storage
/// with automatic compaction. The compaction interface is reserved
/// for future implementation.
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

    /// Compact the history, retaining recent messages and summarizing older ones.
    /// Default implementation is a no-op.
    fn compact(
        &mut self,
        _llm: &dyn Llm,
    ) -> BoxFuture<'_, Result<(), anyhow::Error>> {
        Box::pin(std::future::ready(Ok(())))
    }
}
```

### InMemoryContext

```rust
/// In-memory implementation of ContextManager backed by Vec<Message>.
pub struct InMemoryContext {
    messages: Vec<Message>,
}

impl InMemoryContext {
    pub fn new() -> Self {
        Self { messages: Vec::new() }
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
        // Rough estimate: 4 chars per token
        self.messages
            .iter()
            .map(|m| format!("{m:?}").len() / 4)
            .sum()
    }
    fn clear(&mut self) {
        self.messages.clear();
    }
}
```

## 工具执行接口

```rust
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
```

### ToolRegistry

```rust
/// Registry of available tools, keyed by tool name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
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
```

### Mock 工具

```rust
/// A mock tool that echoes its arguments.
pub struct MockEchoTool {
    pub name: String,
    pub description: String,
}

#[async_trait]
impl Tool for MockEchoTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "The message to echo" }
            }
        })
    }
    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        Ok(format!("echo: {}", arguments["message"]))
    }
}
```

## execute_turn 流程

```
async fn execute_turn(
    ctx: &TurnContext,
    user_text: String,
    context: &mut Box<dyn ContextManager>,
    tx_event: &mpsc::UnboundedSender<Event>,
) -> Result<(), KernelError>
```

### 步骤

1. **构建 user Message，写入历史**
   ```
   let user_msg = Message::user(user_text);
   context.push(user_msg.clone());
   ```

2. **组装 CompletionRequest**
   ```
   let request = ctx.llm
       .completion_request(user_msg)
       .messages(context.history())
       .tools(ctx.tools.definitions())
       .build();
   ```

3. **调用 LLM 流式接口**
   ```
   let mut stream = ctx.llm.stream(request).await?;
   ```

4. **消费 LlmStreamEvent，逐个翻译为 protocol::Event**
   ```
   let mut assistant_content = Vec::new();

   while let Some(event) = stream.next().await {
       let event = event?;
       match event {
           LlmStreamEvent::Text(text) => {
               assistant_content.push(AssistantContent::Text(text.clone()));
               tx_event.send(Event::AgentMessageChunk {
                   session_id: ctx.session_id.clone(),
                   text: text.text,
               });
           }
           LlmStreamEvent::ToolCall { tool_call, internal_call_id } => {
               tx_event.send(Event::ToolCall {
                   session_id: ctx.session_id.clone(),
                   agent_path: AgentPath::root(),
                   call_id: internal_call_id.clone(),
                   name: tool_call.function.name.clone(),
                   arguments: tool_call.function.arguments.clone(),
                   status: ToolCallStatus::InProgress,
               });

               match ctx.tools.execute(
                   &tool_call.function.name,
                   tool_call.function.arguments.clone(),
                   &ctx.cwd,
               ).await {
                   Ok(output) => {
                       assistant_content.push(AssistantContent::ToolCall(tool_call.clone()));
                       tx_event.send(Event::ToolCallUpdate {
                           session_id: ctx.session_id.clone(),
                           call_id: internal_call_id,
                           output_delta: Some(output),
                           status: Some(ToolCallStatus::Completed),
                       });
                   }
                   Err(err) => {
                       tx_event.send(Event::ToolCallUpdate {
                           session_id: ctx.session_id.clone(),
                           call_id: internal_call_id,
                           output_delta: Some(err),
                           status: Some(ToolCallStatus::Failed),
                       });
                   }
               }
           }
           LlmStreamEvent::Reasoning(reasoning) => {
               tx_event.send(Event::AgentThoughtChunk {
                   session_id: ctx.session_id.clone(),
                   text: reasoning.display_text(),
               });
           }
           LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
               tx_event.send(Event::AgentThoughtChunk {
                   session_id: ctx.session_id.clone(),
                   text: reasoning,
               });
           }
           LlmStreamEvent::Final { usage, .. } => {
               if let Some(u) = usage {
                   tx_event.send(Event::UsageUpdate {
                       session_id: ctx.session_id.clone(),
                       input_tokens: u.input_tokens,
                       output_tokens: u.output_tokens,
                   });
               }
           }
           _ => {}
       }
   }
   ```

5. **保存 assistant message 到 context**
   ```
   let assistant_msg = Message::Assistant {
       id: None,
       content: OneOrMany::many(assistant_content)?,
   };
   context.push(assistant_msg);
   ```

6. **TurnComplete 由外层 session loop 统一发送**，不在 execute_turn 内发送。

## 事件翻译器

```rust
// translate.rs

/// Translate an LlmStreamEvent into an optional protocol Event.
/// Returns None for events that don't need to be forwarded.
pub(crate) fn translate_stream_event(
    session_id: &SessionId,
    event: LlmStreamEvent,
) -> Option<protocol::Event> {
    match event {
        LlmStreamEvent::Text(text) => Some(Event::AgentMessageChunk {
            session_id: session_id.clone(),
            text: text.text,
        }),
        LlmStreamEvent::Reasoning(reasoning) => Some(Event::AgentThoughtChunk {
            session_id: session_id.clone(),
            text: reasoning.display_text(),
        }),
        LlmStreamEvent::ReasoningDelta { reasoning, .. } => Some(Event::AgentThoughtChunk {
            session_id: session_id.clone(),
            text: reasoning,
        }),
        LlmStreamEvent::Final { usage, .. } => usage.map(|u| Event::UsageUpdate {
            session_id: session_id.clone(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }),
        LlmStreamEvent::ToolCall { .. } | LlmStreamEvent::ToolCallDelta { .. } => {
            // Tool calls are handled inline in execute_turn, not forwarded directly
            None
        }
    }
}
```

## 依赖关系

```
kernel ──► protocol    (SessionId, Event, Op, StopReason, ToolDefinition, ToolCallStatus, AgentPath)
kernel ──► provider    (LlmFactory, ArcLlm, LlmStreamEvent, Message, CompletionRequest, Llm)
kernel ──► config      (ConfigHandle)
kernel ──► tokio       (mpsc, watch, spawn, select, CancellationToken)
kernel ──► async-trait (Tool trait)
kernel ──► async-stream (EventStream)
kernel ──► futures     (Stream, StreamExt)
kernel ──► uuid        (SessionId generation)
kernel ──► anyhow      (error handling)
```

## 实施计划概述

1. **context.rs** — ContextManager trait + InMemoryContext
2. **tool.rs** — Tool trait + ToolRegistry + MockEchoTool
3. **translate.rs** — LlmStreamEvent → protocol::Event
4. **turn.rs** — TurnContext + execute_turn()
5. **session.rs** — SessionHandle + SessionRuntime + event_stream() + 后台 task
6. **lib.rs** — 修改 Kernel，将 stub prompt() 替换为真实 session 管理

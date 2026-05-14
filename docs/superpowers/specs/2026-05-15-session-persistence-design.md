# Session 持久化设计方案

**日期**: 2026-05-15
**状态**: 已审核，Phase 1 root session 持久化已实现

参考实现: `/home/isbest/Documents/WorkSpace/codex`

---

## 1. 背景

当前 `clawcode` 的 session/thread 状态只存在于内存中：

- `Kernel.sessions` 是 `HashMap<SessionId, Thread>`，进程退出后所有 session 丢失。
- `Kernel::load_session()` 只检查内存 map，不会从磁盘恢复。
- 每个 `Session` 通过 `Box<dyn ContextManager>` 持有对话历史，当前默认实现为 `InMemoryContext`。
- subagent 由 `AgentControl` 创建独立 child session，并在内存中的 registry/mailbox 中维护路由关系。

Codex 的参考模型是：每个 thread 一个 append-only `rollout-*.jsonl` 作为 transcript source of truth；SQLite 只做索引、metadata、父子 edge 辅助发现。本项目第一版只实现文件持久化，不引入 SQLite 索引。

---

## 2. 目标

1. 将 root session 和 subagent session 都持久化到本地文件。
2. 支持通过 `SessionId` 恢复已完成历史，并继续 append 后续 turn。
3. 持久化足够的 system prompt 输入，使恢复后请求上下文可解释、可复现。
4. 持久化 subagent 父子关系，使 parent session 恢复时可以重新发现 child session。
5. 保持第一版实现简单：append-only JSONL + 小型 manifest，不引入 SQLite。

---

## 3. 非目标

1. 不实现 SQLite / Tantivy / 复杂索引。
2. 不实现跨机器同步或远程 thread store。
3. 不保证恢复崩溃时仍在流式生成中的半个 assistant/tool 调用。
4. 不持久化 pending approval channel、Tokio task、mpsc receiver、watch channel 等运行时对象。
5. 不持久化尚未进入 turn 的 volatile mailbox 消息；如果需要，可作为后续增强加入 mailbox journal。

---

## 4. 文件布局

```text
<data_home>/
├── sessions/
│   └── YYYY/
│       └── MM/
│           └── DD/
│               └── session-<timestamp>-<session_id>.jsonl
├── session_manifest.jsonl
└── archived_sessions/
    └── ...
```

### 4.1 data_home

新增配置项：

```toml
[session_persistence]
enabled = true
data_home = "~/.local/share/clawcode"
```

默认值：

- `enabled = true`
- `data_home = ~/.local/share/clawcode`

目录选择说明：

- MCP OAuth credential 当前保存在 `~/.config/clawcode/auth/mcp`，属于配置/密钥目录。
- Session transcript 属于用户数据，因此使用 data home：`~/.local/share/clawcode`。
- 两者不要混放，避免 session 历史与 auth secret 共享同一目录权限语义。

### 4.2 session 文件

每个 session 一个 append-only JSONL 文件：

```text
sessions/2026/05/15/session-2026-05-15T21-35-12-<session_id>.jsonl
```

路径只在创建 session 时确定，后续恢复继续 append 同一个文件。

### 4.3 manifest 文件

`session_manifest.jsonl` 是轻量 append-only manifest，用于通过 `SessionId` 快速定位文件路径。

它不是 source of truth。若 manifest 缺失或损坏，后续可以扫描 `sessions/**/session-*.jsonl` 重建。

---

## 5. 持久化记录模型

所有 JSONL 行统一格式：

```json
{"timestamp":"2026-05-15T13:35:12.123Z","type":"session_meta","payload":{}}
```

Rust record types 放在新模块：

```text
crates/kernel/src/persistence/
├── mod.rs
├── recorder.rs
├── store.rs
├── record.rs
├── replay.rs
└── manifest.rs
```

### 5.1 PersistedRecord

```rust
/// A timestamped JSONL record stored in a session rollout file.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct PersistedRecord {
    /// UTC timestamp when the record was written.
    pub timestamp: String,
    /// Schema version for forward-compatible replay.
    pub schema_version: u32,
    /// Typed payload for session replay.
    pub payload: PersistedPayload,
}
```

`PersistedRecord` 超过 3 个字段，必须使用 `typed-builder` 构造。

### 5.2 PersistedPayload

```rust
/// Replayable payloads written to the session JSONL file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PersistedPayload {
    SessionMeta(SessionMetaRecord),
    TurnContext(TurnContextRecord),
    Message(MessageRecord),
    TurnComplete(TurnCompleteRecord),
    TurnAborted(TurnAbortedRecord),
    AgentEdge(AgentEdgeRecord),
}
```

第一版只保存 replay 必需的 canonical 记录，不保存所有 UI streaming delta。

---

## 6. SessionMetaRecord

`SessionMetaRecord` 必须是每个 session 文件的第一条有效记录。

```rust
/// Immutable metadata captured when a session rollout is created.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionMetaRecord {
    /// Session id used by protocol and kernel APIs.
    pub session_id: SessionId,
    /// Optional parent session id when this session is a subagent.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Agent path for root or subagent routing.
    pub agent_path: AgentPath,
    /// Optional agent role name for subagent sessions.
    #[builder(default, setter(strip_option))]
    pub agent_role: Option<String>,
    /// Optional human-friendly nickname for subagent display.
    #[builder(default, setter(strip_option))]
    pub agent_nickname: Option<String>,
    /// Working directory used to create the session.
    pub cwd: PathBuf,
    /// Provider id selected at session creation time.
    pub provider_id: String,
    /// Model id selected at session creation time.
    pub model_id: String,
    /// Rendered base system prompt used by the first turn.
    pub base_system_prompt: String,
    /// Timestamp in UTC when the session was created.
    pub created_at: String,
}
```

### 6.1 是否保存 system prompt

第一版保存 `base_system_prompt`，并在每个 turn 保存 `rendered_preamble`。

原因：当前 `turn.rs` 每次 turn 动态 render system prompt，输入包含 cwd、AGENTS.md、skills、config、用户临时 system prompt 等。如果只保存消息历史，恢复后同一路径下的 AGENTS.md 或 skill 配置变化会导致上下文漂移。

### 6.2 恢复时如何使用 system prompt

- `base_system_prompt` 用于审计、展示和 fallback。
- 正常继续对话时，可以重新 render 当前 prompt；但 replay 历史中的旧 turn 以 `rendered_preamble` 为准。
- 如果未来需要“严格复现历史请求”，可以按 turn 的 `rendered_preamble` 回放。

---

## 7. TurnContextRecord

每次开始执行 turn 前写入一条 `TurnContextRecord`。

```rust
/// Durable snapshot of prompt/runtime settings for one turn.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct TurnContextRecord {
    /// Stable id for this turn inside the session.
    pub turn_id: String,
    /// User-visible operation kind, such as prompt or inter_agent_message.
    pub kind: TurnKindRecord,
    /// Current working directory for this turn.
    pub cwd: PathBuf,
    /// Provider id used by this turn.
    pub provider_id: String,
    /// Model id used by this turn.
    pub model_id: String,
    /// Fully rendered preamble passed to CompletionRequest.
    pub rendered_preamble: String,
    /// Optional ad-hoc system prompt from Op::Prompt.
    #[builder(default, setter(strip_option))]
    pub user_system_prompt: Option<String>,
}
```

`TurnKindRecord`：

```rust
/// Durable turn source classification.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKindRecord {
    Prompt,
    InterAgentMessage,
}
```

---

## 8. MessageRecord

`MessageRecord` 持久化对话历史中的 canonical `Message`。

```rust
/// A replayable conversation message appended to ContextManager.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct MessageRecord {
    /// Turn id that produced or consumed this message.
    pub turn_id: String,
    /// Message persisted after it is accepted into ContextManager.
    pub message: protocol::message::Message,
    /// Optional source metadata for diagnostics.
    #[builder(default, setter(strip_option))]
    pub source: Option<MessageSourceRecord>,
}
```

第一版的写入点：

1. `execute_turn()` 接受用户输入后，写入 user message。
2. assistant 完整响应收集完成并 push 到 context 后，写入 assistant message。
3. tool result push 到 context 后，写入 tool result message。

不写入 streaming delta，避免恢复时重放出半截 UI 状态。

---

## 9. TurnCompleteRecord 与 TurnAbortedRecord

```rust
/// Marks a turn as durably completed.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct TurnCompleteRecord {
    /// Completed turn id.
    pub turn_id: String,
    /// Stop reason emitted to protocol clients.
    pub stop_reason: StopReason,
}

/// Marks a turn as interrupted before normal completion.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct TurnAbortedRecord {
    /// Aborted turn id.
    pub turn_id: String,
    /// Human-readable abort reason.
    pub reason: String,
}
```

恢复策略：

- 只 replay 已写入的 `MessageRecord`。
- 如果最后一个 turn 没有 `TurnCompleteRecord` 或 `TurnAbortedRecord`，标记为 interrupted，并不恢复半成品 assistant/tool 状态。
- 后续可以追加一条 synthetic `TurnAbortedRecord`，方便 UI 展示“上次运行中断”。

---

## 10. AgentEdgeRecord

Codex 使用 SQLite `thread_spawn_edges` 记录 parent-child edge。第一版用 JSONL event + manifest 承担同样职责。

```rust
/// Durable parent-child edge for subagent discovery and resume.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct AgentEdgeRecord {
    /// Parent session id.
    pub parent_session_id: SessionId,
    /// Child session id.
    pub child_session_id: SessionId,
    /// Child agent path under the parent tree.
    pub child_agent_path: AgentPath,
    /// Child role name.
    pub child_role: String,
    /// Edge lifecycle status.
    pub status: AgentEdgeStatusRecord,
}

/// Durable lifecycle status for an agent edge.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEdgeStatusRecord {
    Open,
    Closed,
}
```

写入规则：

1. root session spawn child 成功后，父 session 文件追加 `AgentEdge(Open)`。
2. child session 文件的 `SessionMetaRecord.parent_session_id` 也保存 parent id。
3. close child/subtree 时，父 session 文件追加 `AgentEdge(Closed)`。
4. 恢复 parent 时扫描父文件中的 edge 最新状态，递归恢复 `Open` 的 child session。

---

## 11. SessionManifestRecord

```rust
/// Append-only manifest entry for locating session rollout files.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionManifestRecord {
    /// Session id mapped by this entry.
    pub session_id: SessionId,
    /// Path to the session JSONL file, relative to data_home when possible.
    pub path: PathBuf,
    /// Optional parent session id for subagent discovery.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Agent path for display and routing after restore.
    pub agent_path: AgentPath,
    /// Working directory used for fast session listing.
    pub cwd: PathBuf,
    /// Current lifecycle status.
    pub status: SessionManifestStatus,
    /// Last update time in UTC.
    pub updated_at: String,
}

/// Manifest lifecycle status.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionManifestStatus {
    Active,
    Closed,
    Archived,
}
```

manifest 同样 append-only，读取时“同一个 `session_id` 的最后一条记录胜出”。`list_sessions()` 默认展示 `Active` 与 `Closed`，只排除 `Archived`，因此关闭 UI 资源不会让历史 session 从恢复入口消失。

---

## 12. 写入时机

### 12.1 新建 root session

```text
Kernel::new_session(cwd)
  ├─ allocate SessionId
  ├─ create SessionRecorder
  ├─ write SessionMetaRecord
  ├─ append SessionManifestRecord(Active)
  ├─ spawn_thread(..., recorder)
  └─ insert sessions map
```

### 12.2 用户 prompt

```text
run_loop receives Op::Prompt
  ├─ create turn_id
  ├─ render SystemPrompt
  ├─ write TurnContextRecord
  ├─ execute_turn()
  │   ├─ write user MessageRecord
  │   ├─ write assistant/tool MessageRecord after context push
  │   └─ flush recorder after each accepted canonical message
  └─ write TurnCompleteRecord / TurnAbortedRecord
```

### 12.3 subagent spawn

```text
AgentControl::spawn()
  ├─ allocate child SessionId
  ├─ create child SessionMetaRecord(parent_session_id, agent_path, role, nickname)
  ├─ append parent AgentEdge(Open)
  ├─ append child manifest entry
  ├─ register child mailbox and registry metadata
  └─ send initial Op::InterAgentMessage to child
```

### 12.4 close session

```text
Kernel::close_session(session_id)
  ├─ send shutdown/close op
  ├─ flush recorder
  ├─ append manifest status Closed
  └─ remove live session from HashMap
```

---

## 13. 恢复流程

### 13.1 load_session(session_id)

```text
Kernel::load_session(session_id)
  ├─ if live in sessions map: return SessionCreated
  ├─ manifest.lookup(session_id)
  ├─ replay session JSONL
  ├─ build InMemoryContext from MessageRecord
  ├─ build Thread with same SessionId and restored ContextManager
  ├─ register root/subagent metadata in AgentRegistry
  ├─ register mailbox
  ├─ insert sessions map
  └─ return SessionCreated
```

### 13.2 replay 规则

1. 第一条有效 `SessionMetaRecord` 作为 session identity。
2. `MessageRecord` 按文件顺序 push 到新的 `InMemoryContext`。
3. 重复/损坏行默认跳过并记录 warning。
4. 最后一条未完成 turn 不会自动继续执行。
5. 恢复后新的 turn append 到同一个 JSONL 文件。

### 13.3 parent 恢复 child

默认策略：

- `load_session(root)` 只恢复 root，不自动启动所有 child。
- 提供 `restore_subagents = true` 的内部选项给未来 UI/ACP 使用。
- 当需要恢复 subagent 树时，读取 parent session 文件中的 `AgentEdgeRecord`，只恢复最新状态为 `Open` 的 child。

这样可以避免打开一个历史 root session 时意外启动大量 child task。

---

## 14. Inter-agent message 持久化边界

当前 `clawcode` 的 `InterAgentMessage` 会被 run loop 当作一次 turn input 处理。第一版沿用这个模型：

- 一旦 `Op::InterAgentMessage` 被 child session 的 run loop 接收，就写入 `TurnContextRecord(kind = InterAgentMessage)`。
- 消息内容作为 user-equivalent input 进入 `MessageRecord`，确保恢复时 child 能看到该任务。
- 只进入 mailbox 但尚未被 run loop 消费的消息不保证恢复。

如需强保证，需要增加：

```rust
/// Durable mailbox lifecycle event for crash-safe inter-agent delivery.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct MailboxRecord {
    /// Sender agent path.
    pub from: AgentPath,
    /// Recipient agent path.
    pub to: AgentPath,
    /// Message content to deliver.
    pub content: String,
    /// Delivery lifecycle status.
    pub status: MailboxRecordStatus,
}
```

该能力不进入第一版。

---

## 15. Recorder API

```rust
/// Append-only writer for one session JSONL file.
pub struct SessionRecorder {
    path: PathBuf,
    tx: mpsc::Sender<RecorderCommand>,
}

impl SessionRecorder {
    /// Create a recorder for a new session file and defer file materialization until first write.
    pub async fn create(params: CreateRecorderParams) -> io::Result<Self>;

    /// Reopen an existing session file and append future records to it.
    pub async fn resume(path: PathBuf) -> io::Result<Self>;

    /// Queue records for append-only persistence.
    pub async fn append(&self, records: &[PersistedPayload]) -> io::Result<()>;

    /// Flush all queued records and wait for durable completion.
    pub async fn flush(&self) -> io::Result<()>;

    /// Stop the background writer after flushing queued records.
    pub async fn shutdown(&self) -> io::Result<()>;
}
```

实现要求：

- 后台 task 串行写文件，避免多个 turn 并发写乱序。
- append 成功后立即 flush，第一版优先可靠性，不做批量优化。
- 写失败时保留内存 pending queue，下次 flush 重试。
- record 序列化失败应返回错误，不写入半条 JSON。

---

## 16. 集成点

### 16.1 `crates/kernel/src/lib.rs`

- `Kernel` 增加 `session_store: Arc<SessionStore>`。
- `new_session()` 创建 recorder 并传入 `spawn_thread()`。
- `load_session()` 从 `SessionStore` replay，而不是只查内存。
- `close_session()` flush recorder 并更新 manifest。

### 16.2 `crates/kernel/src/session.rs`

- `Thread` 增加 `recorder: Option<SessionRecorder>`。
- `Session` 增加 `recorder: Option<SessionRecorder>`。
- `spawn_thread()` 接收 recorder 与 restored context。
- `run_loop()` 在 turn 开始/结束写 `TurnContextRecord` 与 completion marker。

### 16.3 `crates/kernel/src/turn.rs`

- `TurnContext` 增加 `turn_id`、`provider_id`、`model_id`、`recorder`。
- `execute_turn()` 在 context push 后写 `MessageRecord`。
- preamble render 后写入 `TurnContextRecord.rendered_preamble`。

### 16.4 `crates/kernel/src/agent/control.rs`

- spawn child 成功后写 parent `AgentEdge(Open)`。
- close child/subtree 时写 parent `AgentEdge(Closed)`。
- resume child 时从 `SessionMetaRecord` 恢复 `AgentMetadata`。

---

## 17. 错误处理

| 场景 | 策略 |
|---|---|
| manifest 缺失 | 扫描 `sessions/**/*.jsonl` 重建内存 lookup |
| manifest 行损坏 | 跳过该行，继续读取后续行 |
| session 文件缺失 | `KernelError::SessionNotFound` |
| session 文件部分行损坏 | 跳过损坏行，返回 warning event 或 tracing warning |
| 第一条 meta 缺失 | 拒绝恢复该文件 |
| 最后 turn 未完成 | replay 已完成消息，追加 synthetic aborted marker 可选 |
| recorder flush 失败 | 当前 API 返回 internal error，不删除内存 session |

---

## 18. 测试计划

### 18.1 unit tests

1. `SessionRecorder` 写入 JSONL 后可按顺序读取。
2. manifest 同一 `session_id` 多条记录时最后一条胜出。
3. replay 跳过损坏行但保留有效消息。
4. 未完成 turn 恢复时不会继续执行。
5. `AgentEdge(Open/Closed)` 最新状态计算正确。

### 18.2 integration tests

1. 创建 session → prompt → drop kernel → 新 kernel load_session → 继续 prompt。
2. 创建 root → spawn subagent → child 完成 first turn → reload root → 可发现 child metadata。
3. close child 后 reload root，不把 closed child 作为 open child 恢复。
4. 修改 AGENTS.md 后 reload，旧 turn 的 `rendered_preamble` 仍保存在文件中。
5. 损坏 manifest 后通过扫描 session 文件恢复 lookup。

---

## 19. 分阶段实现

### Phase 1: root session file-only 持久化

- 新增 `persistence` 模块。
- 实现 recorder、manifest、replay。
- `new_session/load_session/close_session` 接入 root session。
- 只恢复 completed canonical messages。

### Phase 2: subagent 持久化

- `SessionMetaRecord` 接入 parent/agent metadata。
- parent 写入 `AgentEdgeRecord`。
- load root 后可以列出 child metadata。
- 支持按需恢复 child session。

### Phase 3: prompt 与 turn 完整性增强

- 每 turn 保存 `rendered_preamble`。
- 检测 interrupted turn 并追加 synthetic marker。
- 增加恢复 warning event。

### Phase 4: 可选增强

- mailbox journal。
- archived sessions。
- manifest 重建命令。
- 后续如需要再引入 SQLite 索引。

---

## 20. 决策点

需要确认的实现边界：

1. 第一版是否默认 `enabled = true`，还是需要显式开启。
2. 恢复 root session 时，是否自动恢复 open subagent，还是只在用户显式查看/交互时 lazy restore。
3. `rendered_preamble` 是否按每 turn 都保存；本 spec 建议保存，以避免 system prompt 漂移。
4. 未完成 turn 是否追加 synthetic `TurnAbortedRecord`；本 spec 建议 replay 时可选追加，但不要自动继续执行。

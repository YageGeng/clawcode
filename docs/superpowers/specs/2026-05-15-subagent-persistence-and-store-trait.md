# Subagent 持久化 & Store 抽象为独立 crate

## 背景

当前 `SessionStore` 是 kernel 内部的一个具体 struct，没有 trait 抽象，无法替换后端。更关键的是，subagent 的 spawn/close 完全不写持久化记录（`AgentEdgeRecord` 定义了但全库零写入），load session 也只能恢复 root session，subagent 树在恢复后消失。

本次改动两个目标：
1. 将 store 抽象为 trait，并独立成一个 crate
2. 实现 subagent 的持久化与恢复

---

## 1. 新建 `crates/store/`

### 1.1 文件结构

```
crates/store/
├── Cargo.toml
└── src/
    ├── lib.rs          # pub mod + re-exports
    ├── record.rs       # 从 kernel/persistence/record.rs 搬出，所有类型改为 pub
    ├── recorder.rs     # SessionRecorder trait + FileSessionRecorder
    ├── manifest.rs     # 从 kernel/persistence/manifest.rs 搬出
    ├── replay.rs       # 从 kernel/persistence/replay.rs 搬出，增加 agent_edges 收集
    ├── traits.rs       # SessionStore trait（新增）
    └── file_store.rs   # FileSessionStore（原 SessionStore 改名并 impl trait）
```

### 1.2 `Cargo.toml`

依赖：`protocol`, `tokio`, `serde`, `serde_json`, `chrono`, `typed-builder`, `async-trait`

### 1.3 `record.rs` — 记录类型（搬出 + 改可见性）

从 `crates/kernel/src/persistence/record.rs` 搬出。所有 `pub(crate)` 改为 `pub`。内容无功能变化：

- `PersistedRecord` — 带时间戳和 schema_version 的包装结构
- `PersistedPayload` — serde tagged enum：`SessionMeta`, `TurnContext`, `Message`, `TurnComplete`, `TurnAborted`, `AgentEdge`
- `SessionMetaRecord` — 已有 `parent_session_id`, `agent_role`, `agent_nickname` 字段
- `TurnContextRecord`, `TurnKindRecord`, `MessageRecord`, `TurnCompleteRecord`, `TurnAbortedRecord` — 不变
- `AgentEdgeRecord` — `parent_session_id`, `child_session_id`, `child_agent_path`, `child_role`, `status`
- `AgentEdgeStatusRecord` — `Open | Closed`
- `SCHEMA_VERSION`, `timestamp_now`

### 1.4 `recorder.rs` — 写入抽象

```rust
/// 向 session 持久化存储追加记录
#[async_trait]
pub trait SessionRecorder: Send + Sync {
    async fn append(&self, payloads: &[PersistedPayload]) -> io::Result<()>;
    async fn flush(&self) -> io::Result<()>;
}
```

`FileSessionRecorder` 就是原来的 `SessionRecorder`，实现以上 trait。内部保持不变：`Arc<Mutex<()>>` + JSONL append + fsync。

### 1.5 `traits.rs` — 存储层抽象

```rust
/// Session 生命周期持久化抽象
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 创建新 session 文件，写入 SessionMeta，追加 manifest
    async fn create_session(&self, params: CreateSessionParams)
        -> io::Result<Option<Box<dyn SessionRecorder>>>;

    /// 加载已有 session，返回重放后的状态和一个可继续追加的 recorder
    fn load_session(&self, session_id: &SessionId)
        -> io::Result<Option<(ReplayedSession, Box<dyn SessionRecorder>)>>;

    /// 关闭 session，flush recorder，标记 manifest Closed
    async fn close_session(&self, session_id: &SessionId, recorder: Option<&dyn SessionRecorder>)
        -> io::Result<()>;

    /// 列出所有活跃 session，可选按 cwd 过滤
    fn list_sessions(&self, cwd: Option<&Path>) -> io::Result<Vec<SessionInfo>>;
}
```

`CreateSessionParams` 新增三个字段（用于 subagent spawn）：

```rust
#[builder(default, setter(strip_option))]
pub parent_session_id: Option<SessionId>,

#[builder(default, setter(strip_option))]
pub agent_role: Option<String>,

#[builder(default, setter(strip_option))]
pub agent_nickname: Option<String>,
```

### 1.6 `manifest.rs` — 元数据索引（搬出）

从 `crates/kernel/src/persistence/manifest.rs` 搬出，`pub(crate)` 改为 `pub`。

`active_manifest_record` 签名增加 `parent_session_id` 参数。

### 1.7 `replay.rs` — 重放逻辑（搬出 + 增强）

从 `crates/kernel/src/persistence/replay.rs` 搬出。主要改动：

`ReplayedSession` 增加字段：
```rust
/// 从 JSONL 中提取的所有 AgentEdge 记录，用于恢复 subagent 树
pub agent_edges: Vec<AgentEdgeRecord>,
```

`replay_session_file` 中原来 `AgentEdge(_)` 被 `_ => {}` 忽略，改为收集到 `agent_edges` 中。

### 1.8 `file_store.rs` — 文件后端实现

原 `kernel::persistence::store::SessionStore` 搬过来，改名 `FileSessionStore`，实现 `SessionStore` trait。内部逻辑不变。日期路径算法（civil_from_days）和 `default_data_home` 也一起搬。

---

## 2. 修改 `crates/kernel/`

### 2.1 删除 `persistence/` 整个模块

删除以下文件：
- `crates/kernel/src/persistence/mod.rs`
- `crates/kernel/src/persistence/record.rs`
- `crates/kernel/src/persistence/recorder.rs`
- `crates/kernel/src/persistence/replay.rs`
- `crates/kernel/src/persistence/manifest.rs`
- `crates/kernel/src/persistence/store.rs`

### 2.2 `Cargo.toml`

新增：`session-store = { path = "../store" }`

### 2.3 类型替换一览

| 旧类型（kernel 内） | 新类型（session-store） |
|---|---|
| `crate::persistence::SessionStore` | `Arc<dyn session_store::SessionStore>` |
| `crate::persistence::SessionRecorder` | `Arc<dyn session_store::SessionRecorder>` |
| `crate::persistence::PersistedPayload` | `session_store::PersistedPayload` |
| `crate::persistence::MessageRecord` | `session_store::MessageRecord` |
| `crate::persistence::TurnContextRecord` | `session_store::TurnContextRecord` |
| `crate::persistence::TurnKindRecord` | `session_store::TurnKindRecord` |
| `crate::persistence::TurnCompleteRecord` | `session_store::TurnCompleteRecord` |
| `crate::persistence::TurnAbortedRecord` | `session_store::TurnAbortedRecord` |
| `crate::persistence::CreateSessionParams` | `session_store::CreateSessionParams` |

以下字段类型需要更新：
- `Kernel.session_store`: `Arc<dyn SessionStore>`
- `Thread.recorder`: `Option<Arc<dyn SessionRecorder>>`
- `Session.recorder`: `Option<Arc<dyn SessionRecorder>>`
- `TurnContext.recorder`: `Option<Arc<dyn SessionRecorder>>`
- `spawn_thread` 参数 `recorder`: `Option<Arc<dyn SessionRecorder>>`
- `with_recorder` 参数同理

### 2.4 `agent/control.rs` — Subagent 持久化

#### 2.4.1 `AgentControl` 新增字段

```rust
/// Session 持久化存储（None 时 subagent 不持久化）
session_store: Option<Arc<dyn SessionStore>>,

/// 已注册 agent 的 recorder 映射，用于写 AgentEdge
recorders: Mutex<HashMap<SessionId, Arc<dyn SessionRecorder>>>,
```

`AgentControl::new()` 新增参数 `session_store: Option<Arc<dyn SessionStore>>`。

新增方法：
```rust
/// 注册一个 session 的 recorder，使 spawn/close 能写入 AgentEdge
pub(crate) async fn register_recorder(&self, session_id: SessionId, recorder: Arc<dyn SessionRecorder>);

/// 移除 recorder（cleanup 时调用）
pub(crate) async fn unregister_recorder(&self, session_id: &SessionId);
```

#### 2.4.2 `spawn()` 中新增持久化逻辑

在 Step 7 commit 之后（即在 registry/mailbox 注册之后）插入：

1. 如果 `session_store` 存在：
   a. 调用 `store.create_session()` 创建子 agent 的持久化文件，`CreateSessionParams` 填入：
      - `parent_session_id = Some(parent_session_id)`
      - `agent_role = Some(role_name)`
      - `agent_nickname = Some(nickname)`
      - `agent_path = child_path`
   b. 将子 recorder 设置到 `handle.recorder`
   c. 将子 recorder 注册到 `self.recorders`
2. 向父 agent 的 recorder 写入 `AgentEdgeRecord(Open)`：
   - `parent_session_id`, `child_session_id`, `child_agent_path`, `child_role`, `status: Open`
3. 如果任一步失败，记录 warn 日志（不阻塞 spawn）

#### 2.4.3 `close_agent()` 中新增持久化逻辑

在清理 registry/mailbox 之前：

1. 获取子 agent 的 recorder 和父 session id
2. 向父 recorder 写入 `AgentEdgeRecord(Closed)`
3. 调用 `store.close_session(child_id, child_recorder)`
4. 移除 `self.recorders` 中的条目

### 2.5 `agent/registry.rs` — 新增恢复方法

```rust
/// 恢复一个已持久化的 subagent，跳过 slot counting 和 depth check。
/// 直接注册 agent_path、agent_nickname、agent_id 到 tree。
pub(crate) fn restore_agent(
    &self,
    agent_id: SessionId,
    agent_path: AgentPath,
    nickname: Option<String>,
    role: Option<String>,
) -> Result<(), String> {
    let mut agents = self.active_agents
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Check path uniqueness
    if agents.agent_tree.contains_key(agent_path.as_str()) {
        return Err(format!("agent path already exists: {agent_path}"));
    }

    // Register nickname
    let final_nickname = if let Some(ref nick) = nickname {
        if agents.used_agent_nicknames.contains(nick) {
            // Nickname collision — generate unique variant
            let variant = format!("{nick}_r");
            agents.used_agent_nicknames.insert(variant.clone());
            Some(variant)
        } else {
            agents.used_agent_nicknames.insert(nick.clone());
            nickname
        }
    } else {
        None
    };

    agents.agent_tree.insert(
        agent_path.to_string(),
        AgentMetadata::builder()
            .agent_id(agent_id)
            .agent_path(agent_path)
            .agent_nickname(final_nickname)
            .agent_role(role)
            .build(),
    );

    Ok(())
}
```

### 2.6 `lib.rs` — Kernel 改动

#### 2.6.1 `new()` 和 `Kernel` 结构体

- `session_store` 字段类型改为 `Arc<dyn SessionStore>`
- `AgentControl::new()` 调用时传入 `session_store`
- `Kernel::new()` 中先后创建 store 和 agent_control，确保 agent_control 持有 store 引用

#### 2.6.2 `load_session()` 增加 subagent 恢复

在现有恢复逻辑（load → spawn_live_thread → register_root_thread → register_mailbox）完成之后，新增：

```rust
// 注册 root 的 recorder 到 agent_control
agent_ctrl.register_recorder(session_id.clone(), recorder).await;

// 递归恢复 subagent 树
self.restore_subagent_tree(
    &replayed.agent_edges,
    &agent_ctrl,
    &app_cfg,
).await;
```

#### 2.6.3 新增 `restore_subagent_tree` 方法

```rust
async fn restore_subagent_tree(
    &self,
    edges: &[AgentEdgeRecord],
    agent_control: &Arc<AgentControl>,
    app_cfg: &Arc<AppConfig>,
) {
    for edge in edges.iter().filter(|e| e.status == AgentEdgeStatusRecord::Open) {
        // 跳过已 live 的（避免重复恢复）
        if agent_control.registry.agent_id_for_path(&edge.child_agent_path).is_some() {
            continue;
        }

        // 1. 加载子 session
        let Some((child_replayed, child_recorder)) =
            self.session_store.load_session(&edge.child_session_id).unwrap_or(None)
        else {
            tracing::warn!(child_id = %edge.child_session_id, "failed to load subagent session");
            continue;
        };

        // 2. 重建 live thread
        let handle = match self.spawn_live_thread(
            edge.child_session_id.clone(),
            child_replayed.meta.cwd.clone(),
            Box::new(InMemoryContext::from_messages(child_replayed.messages.clone())),
            Some(child_recorder),
            Arc::clone(app_cfg),
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(child_id = %edge.child_session_id, error = %e, "failed to restore subagent thread");
                continue;
            }
        };

        // 3. 注册到 registry
        if let Err(e) = agent_control.registry.restore_agent(
            edge.child_session_id.clone(),
            edge.child_agent_path.clone(),
            child_replayed.meta.agent_nickname.clone(),
            child_replayed.meta.agent_role.clone(),
        ) {
            tracing::warn!(child_id = %edge.child_session_id, error = %e, "failed to register restored subagent");
            continue;
        }

        // 4. 注册 mailbox 和 recorder
        let sid = edge.child_session_id.clone();
        let mb = handle.mailbox.clone();
        let ag = Arc::clone(agent_control);
        tokio::spawn(async move {
            ag.register_mailbox(sid.clone(), mb).await;
            ag.register_recorder(sid, child_recorder).await;
        });

        // 5. 加入 live sessions map
        self.sessions.lock().await.insert(
            edge.child_session_id.clone(),
            handle,
        );

        // 6. 递归恢复子 agent 的子 agent
        self.restore_subagent_tree(&child_replayed.agent_edges, agent_control, app_cfg).await;
    }
}
```

### 2.7 `session.rs` — Thread/Session 字段类型更新

- `Thread.recorder`: `Option<Arc<dyn SessionRecorder>>`
- `Session.recorder`: `Option<Arc<dyn SessionRecorder>>`
- `spawn_thread` 参数 `recorder`: `Option<Arc<dyn SessionRecorder>>`
- `with_recorder` 参数同理
- `persist_turn_complete`, `persist_turn_aborted` 参数同理

`SessionRecorder` trait 没有 `Clone`，所以用 `Arc<dyn SessionRecorder>` wrap 一层即可 clone。

### 2.8 `turn.rs` — 导入路径更新

将 `crate::persistence::*` 导入改为 `session_store::*`，类型不变但来源变了。`TurnContext` 中 recorder 字段类型从 `Option<SessionRecorder>` 改为 `Option<Arc<dyn SessionRecorder>>`。

---

## 3. 数据流总结

### 3.1 Subagent Spawn

```
AgentControl::spawn()
  → store.create_session(parent_id, role, nickname)  → child_recorder
  → spawn_thread(recorder = Some(child_recorder))
  → self.recorders.insert(child_id, child_recorder)
  → parent_recorder.append(AgentEdgeRecord(Open))
```

### 3.2 Subagent Close

```
AgentControl::close_agent(agent_path)
  → 从 registry 获取 child_id
  → 从 self.recorders 获取 child_recorder
  → 从 edge 获取 parent_id → 从 self.recorders 获取 parent_recorder
  → parent_recorder.append(AgentEdgeRecord(Closed))
  → store.close_session(child_id, &child_recorder)
  → self.recorders.remove(child_id)
  → （原有逻辑）清理 registry/mailbox/status
```

### 3.3 Session Load（含 subagent 树恢复）

```
Kernel::load_session(root_id)
  → store.load_session(root_id)
  → 重放得到 ReplayedSession { meta, messages, agent_edges }
  → spawn_live_thread(root_id)  // 原有
  → register_root_thread        // 原有
  → register_mailbox            // 原有
  → register_recorder(root_id)  // 新增
  → 遍历 agent_edges 中所有 Open edge：
      → store.load_session(child_id)
      → spawn_live_thread(child_id)
      → registry.restore_agent(child_path, nickname, role)
      → register_mailbox + register_recorder
      → 递归恢复子 agent 的子 agent
  → 返回 SessionCreated
```

---

## 4. 需要修改的文件清单

| 操作 | 文件 |
|---|---|
| 新建 | `crates/session-store/Cargo.toml` |
| 新建 | `crates/session-store/src/lib.rs` |
| 新建 | `crates/session-store/src/record.rs` |
| 新建 | `crates/session-store/src/recorder.rs` |
| 新建 | `crates/session-store/src/manifest.rs` |
| 新建 | `crates/session-store/src/replay.rs` |
| 新建 | `crates/session-store/src/traits.rs` |
| 新建 | `crates/session-store/src/file_store.rs` |
| 修改 | `crates/kernel/Cargo.toml` |
| 修改 | `crates/kernel/src/lib.rs` |
| 修改 | `crates/kernel/src/session.rs` |
| 修改 | `crates/kernel/src/turn.rs` |
| 修改 | `crates/kernel/src/agent/control.rs` |
| 修改 | `crates/kernel/src/agent/registry.rs` |
| 删除 | `crates/kernel/src/persistence/`（整个目录，6 个文件） |

---

## 5. 验证方式

### 5.1 编译

```bash
cargo build -p session-store
cargo build -p kernel
cargo build  # 全量
```

### 5.2 测试

```bash
cargo test -p session-store
cargo test -p kernel
```

### 5.3 端到端

1. 启动 clawcode，创建 session
2. 让 agent spawn subagent
3. 检查 JSONL 文件中有 `"type": "agent_edge"` 记录
4. 检查 manifest 中有子 session 条目，带 `parent_session_id`
5. 关闭 clawcode，重新启动，resume session
6. `list_agents` 能看到之前的 subagent
7. 给 subagent 发消息能正常工作

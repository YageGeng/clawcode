# ThreadManager Subagent Implementation Plan

> **给 agentic workers：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans` 按任务执行本计划。所有步骤使用 checkbox (`- [ ]`) 追踪。提交代码前必须先获得用户明确许可，并且必须先运行 pre-commit。

**目标：** 将 clawcode 的 subagent 升级为独立 thread，并引入 `ThreadManager` 与 `AgentGraphStore`，让 subagent 具备独立持久化、可恢复拓扑、明确通信入口和 parent-visible 完成结果。

**架构：** `ThreadManager` 负责 live thread 生命周期、加载、关闭和 `Op` 投递；`AgentControl` 继续负责 agent 语义、路径、role、nickname 和拓扑约束；`crates/store` 提供 `AgentGraphStore` 持久化父子 edge。第一版 `AgentGraphStore` 由 `FileSessionStore` 实现，复用父 session JSONL 中的 append-only `AgentEdgeRecord`。

**技术栈：** Rust、Tokio、async-trait、typed-builder、现有 `protocol` / `kernel` / `store` crate、现有 JSONL session persistence。

---

## 文件结构

- 新建 `crates/store/src/agent_graph.rs`
  - 定义 `AgentEdge`、`AgentEdgeStatus`、`AgentGraphStore`。
  - 实现 `AgentEdgeRecord` 与 graph 模型之间的转换。
  - 包含折叠 edge log 的纯函数测试。
- 修改 `crates/store/src/lib.rs`
  - 导出 `AgentGraphStore`、`AgentEdge`、`AgentEdgeStatus`。
- 修改 `crates/store/src/file_store.rs`
  - 为 `FileSessionStore` 实现 `AgentGraphStore`。
  - 添加 append edge、按 parent replay children、disabled no-op 测试。
- 新建 `crates/kernel/src/thread_manager.rs`
  - 定义 `ThreadManager`、`SpawnThreadParams`、`LoadThreadParams`。
  - 封装 live `HashMap<SessionId, Thread>`、spawn/load/get/send_op/close。
- 修改 `crates/kernel/src/lib.rs`
  - 用 `ThreadManager` 收口 `Kernel.sessions` 访问。
  - root `new_session` / `load_session` / `prompt` / `cancel` / `close_session` 经过 `ThreadManager`。
  - restore subagent 时从 `AgentGraphStore` 查询 open children。
- 修改 `crates/kernel/src/agent/control.rs`
  - `AgentControl` 依赖 `Arc<ThreadManager>` 和 `Arc<dyn AgentGraphStore>`。
  - spawn subagent 时先创建 child recorder，再启动 child thread。
  - edge 写入和关闭状态改走 `AgentGraphStore`。
  - follow-up delivery 改走 `ThreadManager::send_op`。
- 修改 `crates/protocol/src/op.rs`
  - 让 `Op::InterAgentMessage` 携带完整 `InterAgentMessage`，保留 `trigger_turn` 语义。
- 修改 `crates/kernel/src/session.rs`
  - 在 turn 完成后通知 `AgentControl`，用于 parent-visible completion notification。
  - 保持 `spawn_thread` 只负责底层 runtime wire-up，不直接管理 graph。

## 关键落地决策

- `Kernel` 先创建 `Arc<ThreadManager>`，再创建 `AgentControl` 并把同一个 manager 注入进去，避免 `AgentControl` 与 `ThreadManager` 构造时互相持有导致循环依赖。
- `ThreadManager::spawn_thread` 的 `SpawnThreadParams` 中包含 `agent_control: Option<Arc<AgentControl>>`，因此 `ThreadManager` 本身不需要持有 `AgentControl`。
- `ThreadManager` 必须真正承担 live thread 生命周期：`spawn_thread`、`load_thread`、`insert_thread`、`get_thread`、`take_rx`、`send_op`、`close_thread` 都在 manager 内部完成，`Kernel` 不再直接读写 live thread map。
- `Op::InterAgentMessage` 改为携带 `protocol::InterAgentMessage`。当 `trigger_turn=false` 时，session 只把消息写入 pending queue 并持久化为 user-visible message；当 `trigger_turn=true` 时，session 先 drain pending queue，再用该消息启动 turn。
- completion notification 复用 `InterAgentMessage { trigger_turn: false }`，写入 parent pending queue 和 parent history，但不主动启动 parent turn。

---

## Task 1: 实现 AgentGraphStore 接口和模型

**Files:**
- Create: `crates/store/src/agent_graph.rs`
- Modify: `crates/store/src/lib.rs`
- Test: `crates/store/src/agent_graph.rs`

- [ ] **Step 1: 写失败测试，覆盖 edge log 折叠**

在 `crates/store/src/agent_graph.rs` 先加入模型、函数签名和测试。测试应先失败，因为折叠函数尚未实现。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentPath, SessionId};

    #[test]
    fn latest_edge_status_wins_when_child_is_closed() {
        let parent = SessionId("parent".to_string());
        let child = SessionId("child".to_string());
        let path = AgentPath("/root/child".to_string());
        let records = vec![
            edge_record(&parent, &child, &path, "reviewer", AgentEdgeStatus::Open),
            edge_record(&parent, &child, &path, "reviewer", AgentEdgeStatus::Closed),
        ];

        let children = fold_agent_edges(parent.clone(), records, Some(AgentEdgeStatus::Open));

        assert!(children.is_empty());
    }

    #[test]
    fn folded_edges_preserve_latest_role_and_path() {
        let parent = SessionId("parent".to_string());
        let child = SessionId("child".to_string());
        let first_path = AgentPath("/root/old".to_string());
        let latest_path = AgentPath("/root/new".to_string());
        let records = vec![
            edge_record(&parent, &child, &first_path, "", AgentEdgeStatus::Open),
            edge_record(&parent, &child, &latest_path, "coder", AgentEdgeStatus::Open),
        ];

        let children = fold_agent_edges(parent.clone(), records, None);

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].child_agent_path, latest_path);
        assert_eq!(children[0].child_role.as_deref(), Some("coder"));
    }

    fn edge_record(
        parent: &SessionId,
        child: &SessionId,
        path: &AgentPath,
        role: &str,
        status: AgentEdgeStatus,
    ) -> AgentEdgeRecord {
        AgentEdgeRecord::builder()
            .parent_session_id(parent.clone())
            .child_session_id(child.clone())
            .child_agent_path(path.clone())
            .child_role(role.to_string())
            .status(status.into())
            .build()
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p store agent_graph -- --nocapture
```

Expected: FAIL，错误应指向 `fold_agent_edges`、`AgentEdge` 或 `AgentGraphStore` 尚未实现。

- [ ] **Step 3: 实现最小接口和折叠逻辑**

在 `crates/store/src/agent_graph.rs` 实现：

```rust
use std::collections::HashMap;

use async_trait::async_trait;
use protocol::{AgentPath, SessionId};

use crate::record::{AgentEdgeRecord, AgentEdgeStatusRecord};

/// Durable lifecycle status for an agent graph edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEdgeStatus {
    /// The parent-child edge is active and should be restored.
    Open,
    /// The child edge has been explicitly closed.
    Closed,
}

/// Durable parent-child edge returned by the graph store.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct AgentEdge {
    /// Parent session id that owns the child edge.
    pub parent_session_id: SessionId,
    /// Child session id referenced by the edge.
    pub child_session_id: SessionId,
    /// Child agent path used for routing after restore.
    pub child_agent_path: AgentPath,
    /// Optional role name for the child agent.
    #[builder(default, setter(strip_option))]
    pub child_role: Option<String>,
    /// Latest durable edge status.
    pub status: AgentEdgeStatus,
}

/// Persistence boundary for durable agent topology.
#[async_trait]
pub trait AgentGraphStore: Send + Sync {
    /// Append or refresh a parent-child edge status.
    async fn upsert_agent_edge(
        &self,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        child_agent_path: AgentPath,
        child_role: Option<String>,
        status: AgentEdgeStatus,
    ) -> std::io::Result<()>;

    /// Append a status update for an existing parent-child edge.
    async fn set_agent_edge_status(
        &self,
        parent_session_id: &SessionId,
        child_session_id: &SessionId,
        status: AgentEdgeStatus,
    ) -> std::io::Result<()>;

    /// Return the latest child edges for a parent, optionally filtered by status.
    fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        status: Option<AgentEdgeStatus>,
    ) -> std::io::Result<Vec<AgentEdge>>;
}

/// Fold append-only edge records so the latest record for each child wins.
pub(crate) fn fold_agent_edges(
    parent_session_id: SessionId,
    records: Vec<AgentEdgeRecord>,
    status_filter: Option<AgentEdgeStatus>,
) -> Vec<AgentEdge> {
    let mut latest = HashMap::<SessionId, AgentEdge>::new();
    for record in records
        .into_iter()
        .filter(|record| record.parent_session_id == parent_session_id)
    {
        let child_role = if record.child_role.is_empty() {
            None
        } else {
            Some(record.child_role)
        };
        let builder = AgentEdge::builder()
            .parent_session_id(record.parent_session_id)
            .child_session_id(record.child_session_id.clone())
            .child_agent_path(record.child_agent_path)
            .status(record.status.into());
        // `strip_option` keeps normal callers ergonomic, so set the role only when present.
        let edge = match child_role {
            Some(child_role) => builder.child_role(child_role).build(),
            None => builder.build(),
        };
        latest.insert(record.child_session_id, edge);
    }
    let mut edges = latest.into_values().collect::<Vec<_>>();
    if let Some(status) = status_filter {
        edges.retain(|edge| edge.status == status);
    }
    edges.sort_by(|left, right| left.child_session_id.0.cmp(&right.child_session_id.0));
    edges
}
```

本计划把 spec 中的 `anyhow::Result` 收窄为 `std::io::Result`，因为第一版接口位于 `crates/store` 且错误都来自文件持久化。`kernel` 侧负责把 `io::Error` 映射为 `KernelError::Internal`。

同时实现 `From<AgentEdgeStatusRecord> for AgentEdgeStatus`、`From<AgentEdgeStatus> for AgentEdgeStatusRecord`，并在 `crates/store/src/lib.rs` 导出新模块。

- [ ] **Step 4: 运行测试确认通过**

Run:

```bash
rtk cargo test -p store agent_graph -- --nocapture
```

Expected: PASS。

---

## Task 2: FileSessionStore 实现 AgentGraphStore

**Files:**
- Modify: `crates/store/src/file_store.rs`
- Test: `crates/store/src/file_store.rs`

- [ ] **Step 1: 写失败测试，覆盖 append、close、disabled**

在 `store_tests` 中添加测试：

```rust
#[tokio::test]
async fn agent_graph_store_lists_latest_open_children() {
    use crate::{AgentEdgeStatus, AgentGraphStore};

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore { data_home: temp.path().to_path_buf(), enabled: true };
    let parent = SessionId("parent".to_string());
    let child = SessionId("child".to_string());
    let path = AgentPath("/root/child".to_string());

    store.create_session(root_params(parent.clone())).await.expect("create").expect("recorder");
    store.upsert_agent_edge(parent.clone(), child.clone(), path.clone(), Some("coder".to_string()), AgentEdgeStatus::Open)
        .await
        .expect("open edge");

    let open = store.list_agent_children(&parent, Some(AgentEdgeStatus::Open)).expect("list");

    assert_eq!(open.len(), 1);
    assert_eq!(open[0].child_session_id, child);
    assert_eq!(open[0].child_role.as_deref(), Some("coder"));
}

#[tokio::test]
async fn agent_graph_store_closed_child_is_not_returned_as_open() {
    use crate::{AgentEdgeStatus, AgentGraphStore};

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore { data_home: temp.path().to_path_buf(), enabled: true };
    let parent = SessionId("parent".to_string());
    let child = SessionId("child".to_string());
    let path = AgentPath("/root/child".to_string());

    store.create_session(root_params(parent.clone())).await.expect("create").expect("recorder");
    store.upsert_agent_edge(parent.clone(), child.clone(), path, Some("coder".to_string()), AgentEdgeStatus::Open)
        .await
        .expect("open edge");
    store.set_agent_edge_status(&parent, &child, AgentEdgeStatus::Closed).await.expect("close edge");

    let open = store.list_agent_children(&parent, Some(AgentEdgeStatus::Open)).expect("list open");

    assert!(open.is_empty());
}

#[tokio::test]
async fn disabled_agent_graph_store_is_noop() {
    use crate::{AgentEdgeStatus, AgentGraphStore};

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore { data_home: temp.path().to_path_buf(), enabled: false };
    let parent = SessionId("parent".to_string());
    let child = SessionId("child".to_string());

    store.upsert_agent_edge(parent.clone(), child, AgentPath("/root/child".to_string()), None, AgentEdgeStatus::Open)
        .await
        .expect("disabled write");

    let children = store.list_agent_children(&parent, None).expect("disabled list");
    assert!(children.is_empty());
}
```

如果测试需要复用 session 创建参数，添加测试 helper `root_params(session_id: SessionId) -> CreateSessionParams`，函数级注释必须为英文。

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p store agent_graph_store -- --nocapture
```

Expected: FAIL，错误应指向 `FileSessionStore` 尚未实现 `AgentGraphStore`。

- [ ] **Step 3: 实现 FileSessionStore 的 graph 方法**

实现策略：

- `upsert_agent_edge` 用 `read_latest_manifest` 查 parent session 文件。
- 用 `FileSessionRecorder::new(path)` append `PersistedPayload::AgentEdge`。
- `set_agent_edge_status` 先 `list_agent_children(parent, None)` 找 child 最新 edge，保留 path/role，再 append 新 status。
- `list_agent_children` replay parent session 文件，调用 `fold_agent_edges`。

新增 helper 函数必须有英文函数级注释，例如：

```rust
/// Resolve the persisted JSONL path for a session id from the manifest.
fn session_path_for_id(&self, session_id: &SessionId) -> io::Result<Option<PathBuf>> {
    let manifest = read_latest_manifest(&self.data_home)?;
    Ok(manifest
        .get(session_id)
        .map(|record| resolve_manifest_path(&self.data_home, &record.path)))
}
```

- [ ] **Step 4: 运行 store 测试**

Run:

```bash
rtk cargo test -p store -- --nocapture
```

Expected: PASS。

---

## Task 3: 引入 ThreadManager 并解决依赖构造顺序

**Files:**
- Create: `crates/kernel/src/thread_manager.rs`
- Modify: `crates/kernel/src/lib.rs`
- Test: `crates/kernel/src/thread_manager.rs`、`crates/kernel/src/lib.rs`

- [ ] **Step 1: 写失败测试，覆盖 missing session 行为**

在 `thread_manager.rs` 添加单元测试，先让编译失败，因为 `ThreadManager` 尚未存在。测试目标是 manager 的 live map 行为，不测试真实 LLM 调用。

```rust
#[tokio::test]
async fn thread_manager_returns_session_not_found_for_missing_send() {
    let manager = ThreadManager::new();
    let missing = SessionId("missing".to_string());

    let error = manager
        .send_op(&missing, Op::Cancel { session_id: missing.clone() })
        .await
        .expect_err("missing session should fail");

    assert!(matches!(error, KernelError::SessionNotFound(id) if id == missing));
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel thread_manager -- --nocapture
```

Expected: FAIL，错误应指向 `ThreadManager` 未定义。

- [ ] **Step 3: 实现 ThreadManager live map 基础 API**

实现要点：

- `ThreadManager` 内部持有 `Mutex<HashMap<SessionId, Thread>>`。
- `insert_thread` 注册已创建的 `Thread`。
- `get_thread` 返回 cloned `Thread`。
- `take_rx` 从目标 thread 调用 `Thread::take_rx`。
- `send_op` 对不存在 session 返回 `KernelError::SessionNotFound`。
- `close_thread` 先发送 `Op::CloseSession`，再从 map 移除。
- 所有新增函数必须有英文函数级注释。

- [ ] **Step 4: 定义 spawn/load 参数结构**

新增 `SpawnThreadParams` 和 `LoadThreadParams`。字段超过 3 个，必须使用 `typed-builder`。`SpawnThreadParams` 必须包含：

- `session_id`
- `cwd`
- `context`
- `recorder`
- `agent_path`
- `llm`
- `tools`
- `agent_control: Option<Arc<AgentControl>>`
- `approval`
- `app_config`

`LoadThreadParams` 必须包含：

- `session_id`
- `cwd`
- `history`
- `recorder`
- `agent_path`
- 其余 runtime 参数同 `SpawnThreadParams`

`ThreadManager` 不持有 `AgentControl`，而是在 `SpawnThreadParams` 中接收 `agent_control`。这解决 `Kernel -> ThreadManager -> AgentControl -> ThreadManager` 的构造循环。

- [ ] **Step 5: 实现 spawn_thread 和 load_thread**

`ThreadManager::spawn_thread` 调用底层 `crate::session::spawn_thread`，然后把返回的 `Thread` 插入 live map。`ThreadManager::load_thread` 先用 `InMemoryContext::from_messages(history)` 构建 context，再调用 `spawn_thread`。

新增函数必须有英文函数级注释。`Arc<T>` clone 必须使用 `Arc::clone(&value)`。

- [ ] **Step 6: Kernel 改用 ThreadManager**

`Kernel` 中把 `sessions: Mutex<HashMap<SessionId, Thread>>` 替换为 `thread_manager: Arc<ThreadManager>`。迁移以下路径：

- `new_session`：创建 recorder 后调用 `thread_manager.spawn_thread`。
- `load_session`：加载 replayed history 后调用 `thread_manager.load_thread`。
- `list_sessions`：从 `thread_manager.live_sessions()` 读取 live sessions。
- `prompt`：调用 `thread_manager.take_rx` 和 `thread_manager.send_op`。
- `cancel`：调用 `thread_manager.cancel_thread`，由 manager 内部发送 `Op::Cancel { ... }` 并保留 live thread。
- `close_session`：先 `SessionStore::close_session`，再 `thread_manager.close_thread`。

构造顺序必须是：

1. 创建 concrete `Arc<FileSessionStore>`。
2. 创建 `Arc<ThreadManager>`。
3. 创建 `AgentControl::new(..., Arc::clone(&thread_manager), session_store, agent_graph_store)`。
4. 创建 `Kernel`。

- [ ] **Step 7: 运行 kernel 测试**

Run:

```bash
rtk cargo test -p kernel -- --nocapture
```

Expected: PASS。

---

## Task 4: AgentControl 使用 ThreadManager 和 AgentGraphStore

**Files:**
- Modify: `crates/kernel/src/agent/control.rs`
- Modify: `crates/kernel/src/lib.rs`
- Test: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: 写失败测试，覆盖 spawn 前创建 child recorder 和 graph edge**

在 `agent/control.rs` 的测试模块中使用 temp `FileSessionStore`。测试断言：

- spawn 后 child session 可以通过 `SessionStore::load_session` 加载。
- parent `AgentGraphStore::list_agent_children(parent, Open)` 返回 child。
- child edge 的 role 等于传入 role。

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel agent::control -- --nocapture
```

Expected: FAIL，当前实现会异步创建 recorder，且 edge 写入绕过 `AgentGraphStore`。

- [ ] **Step 3: 修改 AgentControl 依赖**

修改 `AgentControl` 字段：

- `pub session_store: Option<Arc<dyn SessionStore>>`
- 新增 `pub agent_graph_store: Option<Arc<dyn AgentGraphStore>>`
- 新增 `thread_manager: Arc<ThreadManager>`

`Kernel::new` 需要把同一个 concrete `Arc<FileSessionStore>` 同时转换为 `Arc<dyn SessionStore>` 和 `Arc<dyn AgentGraphStore>` 注入。所有 `Arc<T>` clone 必须使用 `Arc::clone(&value)`。

- [ ] **Step 4: 修改 spawn 流程**

spawn 的顺序必须变为：

1. reserve depth/path/nickname。
2. 解析 parent session id。
3. 创建 child `CreateSessionParams` 和 recorder。
4. 调用 `ThreadManager::spawn_thread` 启动 child，传入 recorder、child path 和 `Some(Arc::clone(self))`。
5. 注册 mailbox/status/metadata。
6. 调用 `AgentGraphStore::upsert_agent_edge(..., AgentEdgeStatus::Open)`。
7. 调用 `ThreadManager::send_op(child_id, Op::InterAgentMessage { message })` 触发首轮，其中 `message.trigger_turn=true`。

不得保留当前 `tokio::spawn(async move { create_session ... })` 的异步补写 recorder 逻辑。

- [ ] **Step 5: 修改 close 流程**

`write_closed_agent_edge` 改为调用：

```rust
graph_store
    .set_agent_edge_status(&parent_sid, thread_id, AgentEdgeStatus::Closed)
    .await
```

再请求 `ThreadManager` 关闭 child thread。关闭 descendant 时也遵循相同顺序。

- [ ] **Step 6: 运行 agent control 测试**

Run:

```bash
rtk cargo test -p kernel agent::control -- --nocapture
```

Expected: PASS。

---

## Task 5: 基于 AgentGraphStore 实现递归恢复

**Files:**
- Modify: `crates/kernel/src/lib.rs`
- Test: `crates/kernel/src/lib.rs`

- [ ] **Step 1: 写失败测试，覆盖 open child 恢复和 closed child 跳过**

测试应创建 root session、append 一个 open child edge、append 一个 closed child edge，然后 `load_session(root)`。断言 registry 中存在 open child path，不存在 closed child path。

关键断言：

```rust
assert!(kernel.agent_control.registry.agent_id_for_path(&open_path).is_some());
assert!(kernel.agent_control.registry.agent_id_for_path(&closed_path).is_none());
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel restore_subagent -- --nocapture
```

Expected: FAIL，当前 restore 仍依赖 replayed root 的 `agent_edges` 参数，并且 `spawn_live_thread` 使用 `AgentPath::root()`。

- [ ] **Step 3: 修改 restore_subagent_tree**

恢复逻辑改为：

- 输入 parent session id，而不是 `&[AgentEdgeRecord]`。
- 调用 `AgentGraphStore::list_agent_children(parent, Some(AgentEdgeStatus::Open))`。
- 对每个 child 加载 child session history。
- 调用 `ThreadManager::load_thread` 时传入 child 的真实 `agent_path`。
- 注册 registry、mailbox、recorder。
- 递归调用 child session id。

不得再通过 `replayed.agent_edges` 参数恢复子树；`ReplayedSession.agent_edges` 可以暂时保留给兼容 replay，但 restore 入口必须以 `AgentGraphStore` 为准。

- [ ] **Step 4: 运行恢复测试**

Run:

```bash
rtk cargo test -p kernel restore_subagent -- --nocapture
```

Expected: PASS。

---

## Task 6: agent 通信改走 ThreadManager::send_op

**Files:**
- Modify: `crates/protocol/src/op.rs`
- Modify: `crates/kernel/src/agent/control.rs`
- Modify: `crates/kernel/src/session.rs`
- Test: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: 写失败测试，覆盖 Op 携带 trigger_turn**

在 `protocol` 测试中覆盖 `Op::InterAgentMessage` 序列化和反序列化，断言其中的 `InterAgentMessage.trigger_turn` 能保留。

Run:

```bash
rtk cargo test -p protocol inter_agent_message -- --nocapture
```

Expected: FAIL，当前 `Op::InterAgentMessage` 没有携带 `trigger_turn`。

- [ ] **Step 2: 修改 Op::InterAgentMessage**

将 `crates/protocol/src/op.rs` 中的 `InterAgentMessage` 变体改为：

```rust
InterAgentMessage {
    message: crate::agent::InterAgentMessage,
}
```

更新所有构造点和匹配点。所有兼容性处理集中在 kernel 内部，不新增第二套 mailbox 协议。

- [ ] **Step 3: 写失败测试，覆盖 send_message 和 followup_task 行为**

测试目标：

- `send_message(trigger_turn=false)` 不应立即启动 turn，但消息必须进入目标 thread 的 pending inter-agent queue。
- `followup_task(trigger_turn=true)` 应通过 `ThreadManager::send_op` 发送 `Op::InterAgentMessage`。

测试不应依赖真实 LLM。为 `ThreadManager` 添加 `#[cfg(test)]` probe，记录 `send_op` 投递的 `Op`；agent control 测试通过该 probe 断言 `send_message` 和 `followup_task` 都走 `ThreadManager::send_op`。

- [ ] **Step 4: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel send_message -- --nocapture
rtk cargo test -p kernel followup_task -- --nocapture
```

Expected: FAIL，当前 `send_message` 写 mailbox，run loop 不消费该 mailbox。

- [ ] **Step 5: 实现统一投递和 pending queue**

实现策略：

- `AgentControl::send_message` 始终构造 `InterAgentMessage { trigger_turn }`，并调用 `ThreadManager::send_op(target_id, Op::InterAgentMessage { message })`。
- `Session` 新增 `pending_inter_agent_messages: Vec<InterAgentMessage>`。
- `run_loop` 收到 `Op::InterAgentMessage { message }` 且 `message.trigger_turn=false` 时，只将 message push 到 pending queue，并把格式化后的 `Message::user(...)` 写入 context 和 recorder。
- `run_loop` 收到 `trigger_turn=true` 时，先 drain pending queue，把 pending messages 格式化为 model-visible `Message::user(...)` 写入 context 和 recorder，再用当前 message 的 content 启动 inter-agent turn。
- 普通 `Op::Prompt` 开始前也要 drain pending queue，让 `send_message(false)` 在 parent/child 下一次自然 turn 中可见。

格式化函数使用固定英文前缀，便于测试稳定断言：

```rust
/// Render an inter-agent message as model-visible user context.
fn render_inter_agent_message(message: &InterAgentMessage) -> String {
    format!(
        "[inter-agent message from {} to {}]\n{}",
        message.from, message.to, message.content
    )
}
```

新增 helper 函数必须写英文函数级注释，并在复杂注入逻辑前写英文注释说明为什么要在 turn 边界 drain pending items。

- [ ] **Step 6: 运行通信测试**

Run:

```bash
rtk cargo test -p protocol inter_agent_message -- --nocapture
rtk cargo test -p kernel send_message -- --nocapture
rtk cargo test -p kernel followup_task -- --nocapture
```

Expected: PASS。

---

## Task 7: child completion notification

**Files:**
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/agent/control.rs`
- Test: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: 写失败测试，覆盖 child 完成后 parent 可见通知**

测试断言 child `TurnComplete` 后：

- child status 更新为 `Completed`。
- parent 的 persisted history 中出现一条 completion notification。
- notification 的 `trigger_turn=false`，不会主动触发 parent turn。

- [ ] **Step 2: 定义 last assistant message 提取规则**

在 `Session` runtime 中维护 `last_assistant_message: Option<String>`。当 `execute_turn` 成功后，从 context history 最后一条 assistant message 提取 text；如果没有 assistant text，则为 `None`。提取 helper 必须有英文函数级注释。

如果当前 `execute_turn` 不返回 assistant message，本任务不要改 provider 层，直接从 `ContextManager::history()` 提取。

- [ ] **Step 3: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel completion_notification -- --nocapture
```

Expected: FAIL，当前 child 完成不会主动通知 parent。

- [ ] **Step 4: 实现完成通知**

实现策略：

- `Session::run_loop` 在 turn 成功或失败后调用 `AgentControl::notify_child_terminal_turn`。
- `AgentControl` 根据 child metadata 找 parent session id。
- notification 通过 `ThreadManager::send_op(parent_id, Op::InterAgentMessage { message })` 写入 parent，`message.trigger_turn=false`。
- 通知内容包含 child path/nickname、child session id、final status、last assistant message。
- `AgentControl` 更新 child `status_watchers` 和 registry 中可见状态；`Completed` 使用提取到的 last assistant message。

- [ ] **Step 5: 运行完成通知测试**

Run:

```bash
rtk cargo test -p kernel completion_notification -- --nocapture
```

Expected: PASS。

---

## Task 8: 最终验证

**Files:**
- Verify: workspace

- [ ] **Step 1: 格式化**

Run:

```bash
rtk cargo fmt --all
```

Expected: command succeeds。

- [ ] **Step 2: 运行 targeted tests**

Run:

```bash
rtk cargo test -p protocol -- --nocapture
rtk cargo test -p store -- --nocapture
rtk cargo test -p kernel -- --nocapture
```

Expected: all PASS。

- [ ] **Step 3: 运行 workspace clippy**

Run:

```bash
rtk cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

Expected: PASS，无 warnings。

- [ ] **Step 4: 运行 pre-commit**

Run:

```bash
rtk pre-commit run --all-files
```

Expected: PASS。

- [ ] **Step 5: 提交前检查**

Run:

```bash
rtk git status --short
rtk git diff --stat
```

Expected: 只包含本计划相关的 spec、plan、store、kernel 改动。此时不要执行 `git commit`，除非用户明确授权。

---

## 覆盖检查

- `AgentGraphStore` 本版本实现：Task 1、Task 2。
- `FileSessionStore` 复用 JSONL edge log：Task 2。
- `ThreadManager` 收口 live thread 生命周期：Task 3。
- subagent 独立 thread 且首轮前拿到 recorder：Task 4。
- `AgentControl` 不直接写 `AgentEdgeRecord`：Task 4。
- open children 递归恢复，closed edge 跳过：Task 5。
- `Op::InterAgentMessage` 保留 `trigger_turn`：Task 6。
- agent 通信通过 `ThreadManager::send_op`：Task 6。
- child 完成主动通知 parent：Task 7。
- pre-commit 前置验证：Task 8。

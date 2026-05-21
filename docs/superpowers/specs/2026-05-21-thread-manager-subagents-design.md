# 基于 ThreadManager 的 Subagent 设计

## 目标

让 clawcode 的 subagent 机制向 Codex 架构靠拢：每个 subagent 都应成为一个一等公民的独立 thread。第一步是引入 `ThreadManager`，由它统一负责 thread 生命周期、live thread 查找、操作投递以及 thread 持久化绑定。

本设计刻意把 thread 生命周期和 agent 语义拆开：

- `ThreadManager` 管运行时 thread。
- `AgentControl` 管 agent 身份、role、path、nickname、depth 和拓扑关系。
- `SessionStore` 管每个 thread 自己的对话持久化。
- `AgentGraphStore` 管持久化的父子拓扑。

## 当前问题

当前 subagent 实现会通过 `spawn_thread` 启动 child runtime，但 child context 是 `InMemoryContext::new()`，并且传入的 recorder 是 `None`。后续虽然会异步创建 child recorder，并注册到 `AgentControl.recorders`，但这不会回填到已经运行中的 child session。因此，subagent 的 turn 不能可靠地写入自己的 session history。

当前 agent 拓扑也分散在 live registry 和 parent session 的 edge record 里。理论上可以递归恢复，但对 Codex 风格的 thread tree 来说还不够稳固。通信也不完整：follow-up message 会进入 mailbox，但 session run loop 主要消费的是 operation channel 里的 `Op`。

## 目标架构

### ThreadManager

新增内部模块 `crates/kernel/src/thread_manager.rs`，在 `Kernel` 中持有一个 `ThreadManager`。

职责：

- 持有 live threads：`HashMap<SessionId, Thread>`。
- 创建 root thread 和 subagent thread。
- 加载持久化历史并恢复 thread。
- 关闭和移除 live thread。
- 向目标 thread 投递 `Op`。
- 暴露 thread status 和 event receiver。
- 确保每个新建 thread 在 runtime 启动前就拿到自己的 recorder。

非职责：

- 解析 agent path。
- 分配 agent nickname。
- 选择 role。
- 管理 parent-child 拓扑。
- 执行 agent depth 限制。

`ThreadManager` 初始 API 应保持窄边界：

```rust
pub(crate) struct ThreadManager { ... }

impl ThreadManager {
    pub(crate) async fn spawn_thread(&self, params: SpawnThreadParams) -> Result<Thread, KernelError>;
    pub(crate) async fn load_thread(&self, params: LoadThreadParams) -> Result<Thread, KernelError>;
    pub(crate) async fn get_thread(&self, session_id: &SessionId) -> Option<Thread>;
    pub(crate) async fn send_op(&self, session_id: &SessionId, op: Op) -> Result<(), KernelError>;
    pub(crate) async fn close_thread(&self, session_id: &SessionId) -> Result<(), KernelError>;
}
```

参数结构体字段会超过 3 个，因此应按项目规则使用 `typed-builder`。

### AgentControl

重构 `AgentControl`，让它依赖 `Arc<ThreadManager>`。

`AgentControl` 保留的职责：

- 将 path 或 nickname 解析为 `SessionId`。
- 执行最大 agent depth 和最大线程数限制。
- 预留并提交 agent path 和 nickname。
- 构建 child `AgentPath`。
- 应用 role 对模型的选择逻辑。
- 持久化和查询 parent-child graph edge。
- 在 agent 语义层处理 spawn、resume、list、close 和 message。

`AgentControl::spawn` 不应再直接调用 `crate::session::spawn_thread`。它应该准备好 `SpawnThreadParams`，然后调用 `ThreadManager::spawn_thread`。

### Thread 持久化

当持久化启用时，每个 root 和 subagent thread 都必须有自己的 session 文件。

thread 创建流程：

1. 解析 thread id、cwd、agent path、role metadata、model、approval policy 和 app config。
2. 通过 `SessionStore::create_session` 创建 session recorder。
3. 把 recorder 传入 `spawn_thread`。
4. 在 `ThreadManager` 中注册 live thread。
5. 在 `AgentControl` 中注册 agent metadata。
6. 通过 `AgentGraphStore` 持久化 parent-child edge。
7. 通过 `ThreadManager::send_op` 投递初始输入。

关键不变量：child thread 的 runtime 必须在第一个 turn 开始前就拿到自己的 recorder。

### AgentGraphStore

引入显式的拓扑持久化边界。`AgentGraphStore` 应定义在 `crates/store` crate 中，而不是定义在 `kernel` 内部。本版本必须实现该接口，并由 `FileSessionStore` 提供第一版可用实现。实现应复用现有 session JSONL 文件中的 `AgentEdgeRecord`，这样可以避免在第一阶段引入新的持久化文件格式。

API 应建模 Codex 的持久化有向 edge：

```rust
pub enum AgentEdgeStatus {
    Open,
    Closed,
}

#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct AgentEdge {
    pub parent_session_id: SessionId,
    pub child_session_id: SessionId,
    pub child_agent_path: AgentPath,
    pub child_role: Option<String>,
    pub status: AgentEdgeStatus,
}

#[async_trait::async_trait]
pub trait AgentGraphStore: Send + Sync {
    async fn upsert_agent_edge(
        &self,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        child_agent_path: AgentPath,
        child_role: Option<String>,
        status: AgentEdgeStatus,
    ) -> anyhow::Result<()>;

    async fn set_agent_edge_status(
        &self,
        parent_session_id: &SessionId,
        child_session_id: &SessionId,
        status: AgentEdgeStatus,
    ) -> anyhow::Result<()>;

    async fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        status: Option<AgentEdgeStatus>,
    ) -> anyhow::Result<Vec<AgentEdge>>;
}
```

`kernel` 侧只依赖 `AgentGraphStore` trait。`AgentControl` 必须通过 `AgentGraphStore` 接口写入和查询 edge，而不能直接写原始 edge record。这样 `ThreadManager` 和 `AgentControl` 只处理运行时语义，持久化布局仍由 `crates/store` 负责。

#### FileSessionStore 实现细节

本版本不新增独立 graph 文件。`FileSessionStore` 应把 graph 看成父 session JSONL 中的 append-only edge log：

- `upsert_agent_edge` 通过 manifest 定位 `parent_session_id` 对应的 session JSONL 文件，并向该文件 append 一个 `PersistedPayload::AgentEdge`。
- `set_agent_edge_status` 同样定位 parent session 文件，并 append 一个带新 status 的 `AgentEdgeRecord`。
- `list_agent_children` replay parent session 文件中的所有 `AgentEdgeRecord`，按 `child_session_id` 折叠，保留每个 child 的最后一条 edge 作为当前状态。
- `status` 过滤在折叠后执行，因此一个 child 先 `Open` 后 `Closed` 时，不会出现在 open children 列表中。
- persistence disabled 时，写入操作应成为 no-op，查询返回空列表，行为与 `SessionStore` 当前 disabled 语义保持一致。

这种设计保持 JSONL append-only，不需要原地改写历史记录，也不需要在第一阶段维护单独索引。代价是 `list_agent_children` 需要 replay parent session 文件；本版本接受这个成本，因为查询只发生在 restore、list 和 close 这类低频路径。

#### 数据模型映射

`AgentGraphStore` 的公开模型应与现有 `AgentEdgeRecord` 解耦：

- `AgentEdgeStatus` 映射到现有 `AgentEdgeStatusRecord`。
- `AgentEdge.child_role` 使用 `Option<String>`，但落盘到现有 `AgentEdgeRecord.child_role` 时，暂时用空字符串表示 `None`。
- `AgentEdge` 字段超过 3 个，必须按项目规则使用 `typed-builder`。

如果后续把 JSONL edge record 升级为 `Option<String>` role 字段，应只改 `store` 内部映射，不影响 `kernel` 对 `AgentGraphStore` 的调用。

#### 一致性规则

`AgentGraphStore` 应提供以下语义保证：

- 同一个 `(parent_session_id, child_session_id)` 可以出现多条 edge record，最后一条记录代表当前状态。
- `upsert_agent_edge(Open)` 用于 spawn 或 restore 时确认 active edge。
- `set_agent_edge_status(Closed)` 用于 close child 或 close parent subtree 时关闭 edge。
- close parent subtree 时，`AgentControl` 应先写 closed edge，再请求 `ThreadManager` 关闭 child thread。
- restore 只递归加载 `Open` 状态的 children。

#### 错误边界

`AgentGraphStore` 的错误应只表示持久化失败，不表达 agent 语义错误：

- parent session 不存在：返回 store error，由 `AgentControl` 转成 kernel error。
- session JSONL 存在但部分 record 损坏：复用现有 replay 语义，跳过损坏行并继续。
- append 失败：返回 error，spawn/close 流程应失败或至少向 caller 暴露失败，不能静默丢失拓扑变化。

### 通信

agent 间通信应通过 `ThreadManager::send_op` 投递，而不是进入一个 run loop 未必消费的旁路 mailbox。

推荐的 `Op` 语义：

- `send_message`：入队一个 inter-agent communication item，但不启动 turn。
- `followup_task`：入队同样的 item，并触发目标 thread 开始或继续 turn。
- child completion：向 parent 入队完成通知，默认不触发 parent turn。

session runtime 应有一个明确位置来接收 pending inter-agent message，并决定它们进入当前 active turn，还是保留到下一个 turn。

### 完成结果

child thread 应从 turn event 更新自己的状态：

- `TurnStarted` -> `Running`
- `TurnComplete(last_message)` -> `Completed(last_message)`
- cancellation -> `Interrupted`
- error -> `Errored`
- shutdown -> `Shutdown`

当 subagent 到达 final status 时，`AgentControl` 应向直接 parent thread 发送结构化完成通知。通知应包含：

- child agent path 或 nickname
- child session id
- final status
- final assistant message，如果存在

该通知应作为 inter-agent communication item 持久化到 parent thread history 中。

### 恢复流程

root restore：

1. `Kernel` 要求 `ThreadManager` 加载 root thread。
2. `AgentControl` 注册 root 映射。
3. `AgentControl` 查询 `AgentGraphStore` 中 open 状态的 children。
4. 对每个 child，`ThreadManager` 加载该 child 的 session history。
5. `AgentControl` 注册 child metadata，并递归处理其 children。

默认不恢复 closed edge。

### 迁移步骤

1. 新增 `ThreadManager`，先把 `Kernel.sessions` 访问收口到它后面。
2. 将 root `new_session`、`load_session`、`prompt`、`cancel`、`close_session` 路径改为经过 `ThreadManager`。
3. 修改 `AgentControl::spawn`，改为调用 `ThreadManager::spawn_thread`。
4. 将 child recorder 创建改为 child runtime 启动前的同步步骤。
5. 在 `crates/store` 中新增 `AgentGraphStore` 接口，并让 `FileSessionStore` 基于当前 `AgentEdgeRecord` 实现该接口。
6. 将 `AgentControl` 的 edge 写入和 children 查询全部迁移到 `AgentGraphStore`。
7. 基于 graph edge 实现递归恢复。
8. 用 `ThreadManager::send_op` 替换 mailbox-based follow-up delivery。
9. 添加 child completion notification，让 parent thread 能看到 subagent 完成结果。

## 测试计划

需要覆盖的重点测试：

- root thread creation 仍能通过 `ThreadManager` 工作。
- subagent spawn 会创建独立持久化 session 文件。
- subagent 第一个 turn 会写入自己的 history。
- root restore 会递归加载 open subagent。
- closed subagent 默认不会恢复。
- `send_message` 只入队，不触发 turn。
- `followup_task` 会触发目标 thread。
- child completion 会记录 parent-visible notification。
- nested subagent 会保留正确 parent-child topology。

## 非目标

- 完整对齐 Codex rollout trace。
- 完整对齐 app-server API。
- 支持 remote thread store。
- 大规模 crate 抽取。
- 第一阶段替换 `SessionStore` 文件格式。

## 设计决策

- 本版本必须实现 `AgentGraphStore`，并复用现有 session JSONL edge record。这样能降低持久化迁移成本，同时让 `AgentControl` 不再直接写原始 edge。
- `send_message` 会把 model-visible inter-agent item 放入目标 thread 的下一次 mailbox delivery point。它自身不启动 turn。
- `followup_task` 会入队相同 inter-agent item，并请求目标 thread 开始或继续 turn。
- completion notification 默认不触发 parent turn。parent 可在下一次 turn 或 wait/status 工具中观察到该通知。

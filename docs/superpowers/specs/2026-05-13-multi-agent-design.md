# 多代理运行时设计

**日期**: 2026-05-13
**状态**: 待审核

---

## 1. 目标

为 clawcode 实现完整的多代理运行时，使 LLM 能够在单个会话树中创建、管理、通信和关闭子代理。采用共享 Session 树模型（与 Codex 一致）。

## 2. 模块总览

```
crates/config/src/multi_agent.rs       # 新增 - MultiAgentConfig
crates/protocol/src/agent.rs           # 修改 - AgentStatus 补充变体
crates/protocol/src/event.rs           # 修改 - 补充子代理事件
crates/kernel/src/agent/mod.rs         # 新增 - 模块根
crates/kernel/src/agent/registry.rs    # 新增 - AgentRegistry
crates/kernel/src/agent/control.rs     # 新增 - AgentControl
crates/kernel/src/agent/mailbox.rs     # 新增 - Mailbox / MailboxReceiver
crates/kernel/src/agent/role.rs        # 新增 - AgentRole 系统
crates/kernel/src/lib.rs               # 修改 - 集成 AgentControl
crates/kernel/src/session.rs           # 修改 - Thread/Session 加入 mailbox + agent_path
crates/kernel/src/turn.rs              # 修改 - 注入 AgentControl + Mailbox
crates/tools/src/builtin/agents.rs     # 新增 - 子代理管理工具
```

## 3. 类型与协议层

### 3.1 AgentStatus 补充

`protocol/src/agent.rs`，在现有 5 个变体基础上增加：

- `PendingInit` — 代理已预留但尚未启动运行
- `NotFound` — 查询的代理路径或昵称在注册表中不存在

### 3.2 Event 补充

`protocol/src/event.rs`，新增事件变体：

- `AgentSpawned` — `{ session_id, agent_path, agent_nickname, agent_role }`，子代理创建完成时发射
- 复用已有 `AgentStatusChange` — 状态流转时发射，`agent_path` 区分来源
- 现有 `ToolCall`、`AgentMessageChunk` 等事件已携带 `agent_path` 字段，无需修改

### 3.3 InterAgentMessage 保持

已有 `InterAgentMessage { from, to, content, trigger_turn }`，结构不变，仅增加实际投递通道。

### 3.4 MultiAgentConfig

`config/src/multi_agent.rs`：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MultiAgentConfig {
    pub max_concurrent_threads_per_session: usize,  // 默认 8
    pub max_spawn_depth: i32,                       // 默认 8
    pub min_wait_timeout_ms: u64,                   // 默认 1000
    pub hide_spawn_metadata: bool,                  // 默认 false
}
```

从 `claw.toml` 的 `[multi_agent]` section 加载，集成到 `AppConfig` 中。

## 4. AgentRegistry（代理注册表）

单文件 `kernel/src/agent/registry.rs`，预估 ~200 行。

### 数据结构

- `AgentMetadata { agent_id, agent_path, agent_nickname, agent_role, last_task_message }`
- `ActiveAgents` — `HashMap<ThreadId, AgentMetadata>` + 路径索引 `HashMap<AgentPath, ThreadId>` + 昵称索引 `HashMap<String, ThreadId>`
- `AgentRegistry` — `Mutex<ActiveAgents>` + `AtomicUsize total_count` + 配置 `max_threads`
- `SpawnReservation` — 两阶段提交 guard（持有 slot + nickname + path），`commit()` 注册，`Drop` 时自动释放

### 核心方法

| 方法 | 功能 |
|------|------|
| `reserve(path, nickname, role)` | 检查路径/昵称冲突和线程上限，返回 `SpawnReservation` |
| `commit(reservation)` | 完成注册，更新索引 |
| `lookup_by_path(path)` | 按路径查找 `AgentMetadata` |
| `lookup_by_nickname(name)` | 按昵称查找 |
| `list_active(prefix)` | 列出活跃代理，可选路径前缀过滤 |
| `remove(thread_id)` | 移除代理，清理索引 |
| `next_thread_depth(parent_path)` | 计算子代理的 spawn depth |

### 昵称池

从静态候选列表（内嵌 ~50 个名字）中随机分配。当所有名字都已使用时，追加数字后缀重置。不依赖外部 `agent_names.txt` 文件，保持自包含。

## 5. AgentControl（代理控制面）

单文件 `kernel/src/agent/control.rs`，预估 ~350 行。

### 数据结构

```rust
pub struct AgentControl {
    session_id: SessionId,
    registry: Arc<AgentRegistry>,
    /// 根 session 的 Thread 句柄，用于 spawn 子 session
    root_handle: Thread,
    /// AgentStatus 的 watch channel 集合
    status_watchers: Mutex<HashMap<ThreadId, watch::Sender<AgentStatus>>>,
}
```

### 核心方法

| 方法 | 功能 |
|------|------|
| `spawn(parent_path, role, prompt, fork_mode)` | 完整 spawn 流程：深度检查 → 预留 → 创建 Thread → commit → 发射事件 → 启动 |
| `send_message(from, to, content, trigger_turn)` | 通过 Mailbox 向目标代理投递消息 |
| `list_agents(prefix)` | 列出活跃代理 |
| `close_agent(agent_path)` | 找到 ThreadId，递归关闭所有后代，清理 registry |
| `resolve_agent_reference(name_or_path)` | 按名称或路径解析目标 ThreadId |
| `subscribe_status(thread_id)` | 返回 `watch::Receiver<AgentStatus>` 供异步等待 |

### Spawn 流程

1. 从 registry 计算 `next_thread_depth(parent_path)`
2. 检查 ≤ `max_spawn_depth`
3. 调用 `registry.reserve()` 获取 SpawnReservation
4. 构建子 Session：如果 `fork_mode = None`，使用空 context；如果 `fork_mode = Some(N)`，复制父 Session 最近 N 轮对话
5. 调用 `spawn_thread()` 创建 Thread，返回 `LiveAgent { thread_id, metadata, status: PendingInit }`
6. 调用 `registry.commit()` 完成注册
7. 向父 Session 的 event stream 发射 `AgentSpawned` 事件
8. 通过 mailbox 向子代理发送初始 prompt（作为 `InterAgentMessage`）
9. 启动 completion watcher（监听 status 变化，完成后通知父代理）
10. 返回 `LiveAgent`

### 与 Kernel 的集成

- `AgentControl` 在根 session 创建时初始化（`Kernel::new_session()`）
- 所有子 agent 共享同一个 `Arc<AgentControl>`
- Kernel 的 `sessions` map 扩展为同时存储 `AgentControl` 引用

## 6. Mailbox（代理间邮箱）

单文件 `kernel/src/agent/mailbox.rs`，预估 ~150 行。

### 数据结构

```rust
pub struct Mailbox {
    tx: mpsc::UnboundedSender<InterAgentMessage>,
    seq: AtomicU64,          // 递增消息序列号
    wake: watch::Sender<u64>, // 唤醒信号（发送最新 seq）
}

pub struct MailboxReceiver {
    rx: mpsc::UnboundedReceiver<InterAgentMessage>,
    wake: watch::Receiver<u64>,
    read_seq: AtomicU64,     // 已读到的序列号
}
```

### 核心方法

| Mailbox | MailboxReceiver |
|---------|-----------------|
| `send(msg)` — seq+1，投递，wake 通知 | `drain()` — 收集所有待处理消息，更新 read_seq |
| `subscribe()` — 返回 `watch::Receiver<u64>` | `has_pending_trigger_turn()` — 检查未读消息中是否有 trigger_turn=true |
| | `has_pending()` — 是否有未读消息 |

### 工作流

- 每个 Session 持有一对 `(Mailbox, MailboxReceiver)`
- `AgentControl::send_message()` 通过 `sender_mailboxes` 查找目标 Session 的 Mailbox 并投递
- Session 的 `run_loop` 在每轮 turn 开始时调用 `receiver.drain()`，检查是否有 trigger_turn 消息
- 如果有 trigger_turn 消息，将其内容作为用户输入执行 turn
- `wait_agent` 工具通过 `mailbox.subscribe()` 的 `watch::Receiver` 等待新消息通知

### 对现有 run_loop 的修改

`session.rs` 的 `run_loop` 需要在 `Op::Prompt` 分支之外，增加 `Op::InterAgentMessage` 的处理：
- 投递到当前 Session 的 Mailbox
- 如果 `trigger_turn=true`，触发一次 turn 执行（以消息内容作为输入）

## 7. AgentRole（代理角色系统）

单文件 `kernel/src/agent/role.rs`，预估 ~200 行。

### 数据结构

```rust
pub struct AgentRole {
    pub name: String,
    pub description: String,
    pub nickname_candidates: Vec<String>,
    pub config_overrides: HashMap<String, String>,
}

pub struct AgentRoleSet {
    roles: HashMap<String, AgentRole>,
}
```

### 内置角色

| 角色 | 覆盖 |
|------|------|
| `default` | 无覆盖，完全继承父配置 |
| `explorer` | 使用轻量模型、低 reasoning、简洁 system prompt |
| `worker` | 使用完整模型、高 reasoning，适合代码实现 |

### 外部角色配置

从 `claw.toml` 的 `[agents.<name>]` section 加载自定义角色：

```toml
[agents.code-reviewer]
description = "专门做代码审查"
nickname_candidates = ["rei", "linter"]
model = "deepseek/deepseek-v4-flash"
```

### 核心函数

- `resolve_role_config(role_name, parent_config, role_set)` → 合并后的 `(model_id, provider_id, reasoning_effort)`
- `apply_role_to_config(role, config)` → 返回覆盖后的 `CompletionRequest` 参数

## 8. Agent 管理工具

单文件 `tools/src/builtin/agents.rs`，预估 ~250 行。

### 工具列表

| 工具名 | 参数 | 功能 | 需审批 |
|--------|------|------|--------|
| `spawn_agent` | `task_name`, `role`(default="default"), `prompt`, `fork_turns`(可选) | 创建子代理，返回 `{ agent_path, nickname }` | 否 |
| `send_message` | `to`, `content`, `trigger_turn`(default=false) | 向子代理发送消息 | 否 |
| `followup_task` | `to`, `content` | 向子代理发送消息并触发 turn（等同于 `send_message` with `trigger_turn=true`） | 否 |
| `wait_agent` | `agent_path`(可选) | 等待指定/任意子代理完成，返回状态和最终消息 | 否 |
| `list_agents` | `path_prefix`(可选) | 列出活跃代理及其状态 | 否 |
| `close_agent` | `agent_path` | 关闭代理及所有后代 | 是 |

### 实现要点

- 每个工具实现 `Tool` trait
- 通过 `AgentControl` 的 `Arc` 引用访问控制面
- `AgentControl` 通过 `ToolRegistry` 的上下文注入（在 Kernel 构建时设置）

### 工具注册

在 `tools/src/builtin/mod.rs` 的 `register_builtins()` 中增加 agent 工具的注册入口，接收 `Option<Arc<AgentControl>>` 参数。

## 9. 线程深度与 Fork 管理

### 深度限制

- `AgentPath::root()` 的 depth = 0
- `root.join("explorer")` 的 depth = 1
- 每个 join 增加深度 1
- 检查逻辑：`path.segments().len() - 1 <= max_spawn_depth`

### Fork 模式

```rust
enum ForkMode {
    None,               // 全新子代理，空上下文
    LastNTurns(usize),  // 从父代理复制最近 N 轮对话
}
```

实现：从父 Session 的 `ContextManager` 中取出最近 N 条 Message（User + Assistant 对），克隆后注入子 Session。

## 10. 事件流

### Spawn 事件序列

```
[父 Session]
  AgentSpawned { agent_path: "/root/code_reviewer", agent_nickname: "Aria", agent_role: "default" }

[子 Session（通过 agent_path 区分）]
  AgentStatusChange { agent_path: "/root/code_reviewer", status: Running }
  AgentMessageChunk { agent_path: "/root/code_reviewer", text: "..." }
  ToolCall { agent_path: "/root/code_reviewer", ... }
  ...
  AgentStatusChange { agent_path: "/root/code_reviewer", status: Completed { message: Some("done") } }
```

### 跨代理消息流

```
[Agent A → Agent B]
1. Agent A 调用 send_message(to="/root/explorer", content="...", trigger_turn=true)
2. AgentControl 将 InterAgentMessage 投递到 B 的 Mailbox
3. B 的 run_loop 在 drain() 时收到消息
4. 如果 trigger_turn=true，B 执行 turn 处理消息
5. 如果需要，B 完成后通过 send_message 回复 A
```

## 11. 依赖关系

```
config/multi_agent    ──无内部依赖
protocol/{agent,event} ──无内部依赖
kernel/agent/registry ──→ protocol
kernel/agent/mailbox  ──→ protocol
kernel/agent/role     ──→ config + protocol
kernel/agent/control  ──→ registry + mailbox + role + session + protocol
kernel/agent/mod       ──→ 以上全部
kernel/session         ──→ agent/mailbox (注入)
kernel/turn            ──→ agent/control (注入)
tools/builtin/agents   ──→ protocol + kernel/agent/control (Arc 引用)
```

## 12. 不变式与错误语义

- **路径唯一性**: 同一时刻，每个 `AgentPath` 最多对应一个活跃代理
- **昵称唯一性**: 同一会话树中，昵称不重复
- **级联关闭**: 关闭父代理时，递归关闭所有后代（先子后父）
- **Spawn 原子性**: 任一步骤失败，已预留的资源必须释放（通过 `Drop`）
- **深度守卫**: spawn 时检查 `spawn_depth ≤ max_spawn_depth`，拒绝超限请求
- **通知可靠性**: `AgentStatusChange` 事件在状态变更时发送，`AgentSpawned` 仅发送给父会话

## 13. 测试点

- `AgentRegistry`: 并发 spawn + release 不冲突；昵称池溢出后正确重置；路径冲突检测
- `AgentControl`: spawn → send → wait → close 完整生命周期；深度限制拦截；fork 历史正确性
- `Mailbox`: 并发 send/drain/trigger_turn 检测；序列号单调递增
- `AgentRole`: 内置角色覆盖正确；自定义角色合并优先级；缺失角色回退 default
- Agent 工具: 每个工具的 schema 生成正确；参数校验；集成测试（spawn → list → send → wait → close）

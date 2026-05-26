# TUI Subagent 列表与上下文切换设计

**日期**: 2026-05-26
**状态**: 待用户审核
**参考**: `/home/isbest/Documents/WorkSpace/codex/codex-rs/tui/src/app/session_lifecycle.rs`、`/home/isbest/Documents/WorkSpace/codex/codex-rs/tui/src/app/agent_navigation.rs`

## 1. 背景

clawcode 已经有 subagent 运行时、session 持久化和父子拓扑记录：

- `AgentMetadata` 保存 agent 的 `SessionId`、`AgentPath`、nickname、role、status 和 parent session。
- `AgentControl::spawn` 会为 child 创建独立 session，并通过 `AgentGraphStore` 记录 parent-child edge。
- `Kernel::restore_subagent_tree` 可以按 open edge 恢复 child session。

但当前 TUI 仍是单 session 模型。启动时只创建一个 `AppState`，`PromptRequest` 总是发给这个 `AppState.session_id()`，`AppState::apply_session_update` 会忽略其他 session 的 ACP notification。因此，即使 subagent 有自己的 session，TUI 也无法展示 subagent 列表，更无法切换到 subagent 的 transcript 和输入上下文。

ACP 当前没有标准 subagent schema。为了不修改 ACP 协议，本设计采用 `_meta` 扩展传递 subagent 列表元数据，同时保留现有 `SessionNotification -> AppState -> render` 作为 transcript 渲染路径。

## 2. 目标

1. 在 TUI 输入框下方的底部区域展示 agent picker，列表中始终包含主 agent 和所有可见 subagent。
2. 保留 `/agent` 作为本地 TUI 入口，用于打开或聚焦输入框下方的 agent picker。
3. 当有 subagent 被创建出来后，agent 列表可以自动出现在输入框下方；`/agent` 用于进入选择模式。
4. 用户可以用方向键上下移动 agent 选中项，并按回车切换到选中 agent 的 session 上下文；切回主 agent 的方式是选中 `Main [default]` 后按回车。
5. 用户可以在 root agent 和 subagent 之间来回切换，切换后看到对应 session 的 transcript，并把后续 prompt 发给当前选中的 session。
6. 采用 Codex 的状态管理模型和 `/agent` picker 入口，但 picker 的呈现和交互按本项目需求调整：
   - 单独维护 agent navigation 状态。
   - 按 first-seen 顺序稳定展示和选择。
   - picker 显示在输入框下方区域，而不是弹窗。
   - picker 使用 `Up` / `Down` 选择、`Enter` 切换上下文。
   - 切换时保存当前 session UI 状态，激活目标 session UI 状态。
   - 已关闭 agent 仍可保留在列表中，用于查看历史。
7. 不修改 ACP schema，通过 `_meta` 扩展补充 subagent 列表元数据。
8. transcript 展示继续走现有 ACP `SessionNotification`，只在 TUI 上层按 `session_id` 分发。

## 3. 非目标

1. 不在本阶段引入新的 ACP 标准消息类型。
2. 不把 subagent transcript 塞进 `_meta`。
3. 不让 `AppState` 直接管理多个 session。`AppState` 仍代表一个 ACP session。
4. 不重写 subagent runtime、AgentControl 或 AgentGraphStore。
5. 不实现跨进程外部 ACP server 的多 session 管理。当前仍以 in-process ACP server 为主。
6. 不改变 model-facing `spawn_agent`、`wait_agent`、`list_agents` 工具协议，除非后续实现计划单独批准。
7. 不新增除 `/agent` 之外的 subagent 专用 slash 命令。
8. 不使用弹窗式 picker 作为主交互。`/agent` 打开的 agent picker 应嵌入输入框下方区域。

## 4. 当前问题

### 4.1 TUI 状态是单 session

当前 `crates/tui/src/app.rs` 持有单个 `AppState`。收到 ACP notification 后直接调用：

```text
AppEvent::SessionNotification(notification)
  -> state.apply_session_update(notification)
```

而 `AppState::apply_session_update` 内部会检查 `notification.session_id != self.session_id` 并直接返回。这对单 session 是正确的，但对 subagent 切换不够。

### 4.2 subagent lifecycle event 没有端到端链路

`protocol::Event` 已定义 `AgentSpawned` 和 `AgentStatusChange`，但当前 kernel/tools 代码没有实际发射这些事件，ACP adapter 也没有把它们转换成 TUI 可消费的更新。因此不能假设 lifecycle event 已经可用。

### 4.3 UI 缺少 child session id

切换上下文的关键是 child `SessionId`。当前 model-facing `list_agents` 输出主要是 agent path/status/task message，适合模型使用，但不适合作为 UI 切换入口。UI 必须从结构化 metadata 获取 `session_id`、`parent_session_id`、`agent_path`、nickname、role 和 status。

## 5. 选定架构

新增一个 TUI 上层 session router，保持 `AppState` 单 session 语义不变。

```text
ACP Client
  -> AppEvent::SessionNotification(session_id, update)
  -> TuiSessionRouter
       ├── active_session_id
       ├── states: HashMap<SessionId, AppState>
       ├── agent_navigation: AgentNavigationState
       └── per-session view/composer snapshot
  -> 当前 active AppState 渲染
```

核心原则：

- `AppState` 只 reduce 一个 session 的 transcript、tool call、usage 和 prompt 状态。
- 多 session 的生命周期、选择、切换和 inline agent picker 状态都放在 `app.rs` 或新的 TUI router 模块。
- `_meta` 只更新 `agent_navigation`，不直接生成 transcript cell。
- 如果 ACP notification 的 `session_id` 不是当前 active session，也要写入对应 `AppState`，只是暂不渲染。

## 6. 数据模型

### 6.1 AgentNavigationState

参考 Codex 的 `AgentNavigationState`，在 TUI 中新增轻量导航状态。

```rust
pub(crate) struct AgentNavigationState {
    agents: HashMap<SessionId, AgentPickerEntry>,
    order: Vec<SessionId>,
}

pub(crate) struct AgentPickerEntry {
    pub(crate) session_id: SessionId,
    pub(crate) parent_session_id: Option<SessionId>,
    pub(crate) agent_path: Option<String>,
    pub(crate) nickname: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) status: AgentPickerStatus,
    pub(crate) is_root: bool,
}
```

设计规则：

- `order` 记录 first-seen 顺序，后续 metadata 更新不能改变顺序。
- root session 必须始终作为一个 entry，显示为 `Main [default]`。
- root session 在 `order` 中固定为第一项，这样用户总能从任何 subagent 通过 `/agent` picker 选回主 agent。
- closed/final agent 不从列表中立即删除，而是标记为 closed，允许查看历史。
- 若同一个 `session_id` 收到多次 upsert，更新 metadata 和 status。
- 若收到 remove，可以删除 metadata-only 且不可恢复的 agent；默认不主动 remove。

字段数量超过 3 的 Rust struct 在实现时必须使用 `typed-builder`。

### 6.2 SessionRouterState

新增 TUI 上层状态，不替代 `AppState`。

```rust
pub(crate) struct SessionRouterState {
    active_session_id: SessionId,
    states: HashMap<SessionId, AppState>,
    agent_navigation: AgentNavigationState,
    view_snapshots: HashMap<SessionId, ViewState>,
    composer_snapshots: HashMap<SessionId, Composer>,
}
```

实现时可以先不独立建这个 struct，而是在 `app.rs` 中引入等价字段；但长期建议拆出独立模块，避免 `app.rs` 继续膨胀。

### 6.3 AgentPickerPanelState

新增底部区域内的 agent picker 选择状态。它只负责 UI 焦点、展开状态和选中项，不负责 session transcript。

```rust
pub(crate) struct AgentPickerPanelState {
    visible: bool,
    focused: bool,
    selected_index: usize,
}
```

显示规则：

- 只有 root session 时默认隐藏，避免占用输入区空间。
- 第一个 subagent metadata 到达后可以显示一个紧凑列表或当前 agent 提示。
- `/agent` 打开完整 picker 并把焦点放到列表选择上。
- 完整 picker 必须包含 root session，对应显示名为 `Main [default]`。
- 当前 active session 和 selected session 可以不同。active 表示正在显示的上下文，selected 表示列表中准备切换的目标。
- 如果 selected agent 被关闭，选中项保留；回车切换时进入只读或 replay-only 视图，具体取决于 session 是否可加载。
- 如果 selected_index 越界，按当前 ordered agent list 截断到最后一个可选项。

## 7. ACP `_meta` 扩展

ACP schema 中 `ToolCallUpdate` 有顶层 `_meta` 字段。本设计使用该字段承载 subagent metadata。

命名空间使用 `clawcode.subagents`，避免和其他 ACP 扩展冲突。

### 7.1 upsert payload

```json
{
  "clawcode": {
    "subagents": {
      "version": 1,
      "event": "upsert",
      "agents": [
        {
          "session_id": "child-session-id",
          "parent_session_id": "parent-session-id",
          "agent_path": "/root/task",
          "nickname": "finder",
          "role": "worker",
          "status": "running",
          "is_root": false
        }
      ]
    }
  }
}
```

### 7.2 status payload

```json
{
  "clawcode": {
    "subagents": {
      "version": 1,
      "event": "status",
      "agents": [
        {
          "session_id": "child-session-id",
          "status": "completed"
        }
      ]
    }
  }
}
```

### 7.3 snapshot payload

当 TUI 新建或加载 root session 后，ACP server 应发送一次 snapshot，让 UI 能初始化 inline agent picker。

```json
{
  "clawcode": {
    "subagents": {
      "version": 1,
      "event": "snapshot",
      "agents": [
        {
          "session_id": "root-session-id",
          "parent_session_id": null,
          "agent_path": "/root",
          "nickname": null,
          "role": null,
          "status": "running",
          "is_root": true
        }
      ]
    }
  }
}
```

### 7.4 发送载体

由于 ACP 没有单独的 metadata-only session update，建议发送一个 `ToolCallUpdate`，其 `tool_call_id` 使用稳定的内部 id，例如：

```text
clawcode-subagents
```

该 update 可以不设置 visible content，只设置 `_meta`。TUI reducer 需要先解析 `_meta`，成功处理后不把这个内部 update 渲染成普通 tool cell。

如果后续发现 ACP client 或 schema 对空 `ToolCallUpdateFields` 兼容性不好，可以设置一个不会显示的内部 `title`，并在 TUI 中按 `_meta.clawcode.subagents` 拦截。

## 8. ACP Adapter 变更

`crates/acp/src/agent.rs` 负责把 kernel/session 信息转换成 ACP notification。

需要新增的职责：

1. `handle_new_session` 成功后发送 root agent snapshot。
2. `handle_load_session` 成功并恢复 subagent tree 后发送完整 snapshot。
3. `handle_prompt` 中若收到 subagent metadata 相关事件或工具结果，发送 `_meta` update。
4. 若后续补齐 kernel `AgentSpawned` / `AgentStatusChange` 发射，ACP adapter 应把它们转换为 `_meta` upsert/status。

第一阶段可以从 `AgentControl` 或 `Kernel` 暴露只读 snapshot API：

```rust
async fn agent_snapshot(&self, root_session_id: &SessionId) -> Vec<AgentUiMetadata>
```

该 API 只用于 UI metadata，不改变 model-facing tools。字段应来自 registry 和 persisted graph，而不是解析 `list_agents` 文本。

## 9. TUI 交互

### 9.1 `/agent` picker

保留 `/agent` slash command。该命令是本地 TUI 命令，不发送给模型；它打开或聚焦输入框下方的 agent picker。

agent picker 是底部输入区域的一部分，位于 composer 下方或 composer 附近的固定区域，不使用居中弹窗。

显示行为：

1. 从 `AgentNavigationState` 读取 ordered entries。
2. 只有 root session 时，`/agent` 可以显示只含 `Main [default]` 的列表和“暂无 subagent”的轻量提示，也可以直接返回。
3. 有一个或多个 subagent 后，输入框下方可以自动显示紧凑 agent 列表；输入 `/agent` 后进入完整选择模式。
4. 以 `Main [default]`、`nickname [role]`、`Agent (<short session id>)` 的顺序生成显示名。
5. 当前 active session 标记为 current。
6. 当前 selected session 使用高亮或箭头标记。
7. status 使用简单文本或状态点：
   - running: 绿色点
   - pending: 黄色点
   - completed/closed: dim 点
   - errored: 红色点
8. 列表较长时只显示有限行数，并保持 selected item 可见。

### 9.2 键盘选择

`/agent` picker 的键盘行为：

1. 用户输入 `/agent` 并确认后，picker 获得焦点。
2. picker 聚焦时，`Up` 移动到上一个 agent。
3. picker 聚焦时，`Down` 移动到下一个 agent。
4. picker 聚焦时，`Enter` 触发 `SelectAgentSession(selected_session_id)`。
5. `Esc` 关闭 picker 或退出 picker 焦点，回到 composer。
6. 如果 picker 未聚焦，`Up` / `Down` 保持现有 composer 行导航或历史导航行为，不抢占编辑。
7. 遍历顺序使用 `AgentNavigationState.order`。

### 9.3 切换 session

`SelectAgentSession(session_id)` 流程：

1. 如果目标就是当前 active session，直接返回。
2. 保存当前 session 的 `ViewState` 和 `Composer`。
3. 确保目标 session 有 `AppState`：
   - 已有 live `AppState`：直接使用。
   - 没有但可 load：调用 ACP `LoadSessionRequest` 或本地 session load 路径创建 state，并 replay history。
   - load 失败：显示错误，不切换。
4. 设置 `active_session_id = target_session_id`。
5. 恢复目标 session 的 `ViewState` 和 `Composer`，若没有则使用默认值。
6. 清理屏幕并重绘目标 transcript。
7. 更新 footer 中的 active agent label。

切回主 agent 不需要单独命令。主 agent 是 picker 中的第一项，用户选中 `Main [default]` 并按 `Enter` 后，执行同一个 `SelectAgentSession(root_session_id)` 流程。

### 9.4 prompt 路由

提交 prompt 时不再固定使用启动 session：

```text
Composer submit
  -> active_session_id
  -> PromptRequest(active_session_id, text)
```

`PromptFinished` / `PromptFailed` 也必须带 session id 或由 prompt task 记录所属 session。否则用户在 prompt 运行期间切换 session 时，完成状态可能错误地落到当前 active session。

### 9.5 底部区域布局

agent picker 是底部输入区域的扩展，而不是 transcript 中的 cell。

布局规则：

1. composer 保持主要输入位置。
2. agent picker 出现在 composer 下方的可扫描区域。
3. agent picker 只占用必要高度，默认最多显示 3 到 5 行，超出后滚动。
4. active agent label 可以继续出现在 footer/status 区，但不能替代可选择列表。
5. 切换 session 时不清除用户正在编辑的其他 session draft；每个 session 的 composer snapshot 独立保存。

## 10. Notification 路由

当前 `AppEvent::SessionNotification(Box<SessionNotification>)` 保持可用，但处理逻辑要从“直接 apply 到当前 state”改为“按 session id 分发”。

建议：

```text
handle_app_event(SessionNotification(notification)):
  router.apply_session_notification(notification)
  if notification.session_id == active_session_id:
      schedule render
  else:
      mark inactive session dirty
```

`apply_session_notification` 规则：

- 如果 `notification.update` 是带 `_meta.clawcode.subagents` 的 `ToolCallUpdate`，先更新 `AgentNavigationState`。
- 如果该 metadata-only update 不应显示，则不继续传给 `AppState`。
- 普通 session update 传给对应 `AppState`。
- 若对应 `AppState` 不存在：
  - 如果 notification 是 metadata-only，可以只更新 navigation。
  - 如果 notification 是 transcript update，应创建临时 `AppState` 或缓存 pending updates，避免丢失 inactive session 输出。

## 11. 与 Codex 的映射

Codex 概念到 clawcode 映射：

| Codex | clawcode |
| --- | --- |
| `ThreadId` | ACP `SessionId` / protocol `SessionId` |
| `AgentNavigationState` | 新增 TUI `AgentNavigationState` |
| `ThreadEventChannel` | 可选的 per-session notification buffer |
| `active_thread_id` | `active_session_id` |
| `select_agent_thread` | `select_agent_session` |
| `thread/read` fallback | `LoadSessionRequest` 或 session store replay |
| `CollabAgentToolCall` history cell | 现有 tool call cell，后续可用 `_meta` 增强 |
| `/agent` picker | 采用入口；呈现位置改为 composer 下方 inline picker |

本项目不需要完整复制 Codex app-server thread API。核心是复制 UI 层的稳定列表、session 状态隔离、切换和 replay 思路；同时保留 Codex 的 `/agent` 入口，但把 picker 交互改成输入框下方的 inline 选择列表。

## 12. 错误处理

1. `_meta` payload 解析失败：记录 debug/warn，忽略 metadata，不影响 transcript。
2. `session_id` 缺失：忽略该 agent entry。
3. status 未知：映射为 `Unknown`，显示为普通 dim 状态。
4. 切换到不可加载 session：保留当前 session，显示错误。
5. inactive session 收到 approval request：第一阶段可以在当前 session 顶部提示“另一个 agent 需要审批”；后续可实现 pending inactive approval 列表。
6. prompt 运行时切换 session：允许切换，但完成事件必须回到 prompt 所属 session。

## 13. 测试策略

### 13.1 ACP adapter tests

- new session 后发送 root snapshot `_meta`。
- load session 后发送 root + restored subagents snapshot。
- `AgentSpawned` 转换为 `_meta` upsert。
- `AgentStatusChange` 转换为 `_meta` status。
- metadata-only update 不包含可见 content 时仍可序列化。

### 13.2 TUI state tests

- `AgentNavigationState::upsert` 保持 first-seen 顺序。
- 同一 agent 更新 status 不改变顺序。
- closed agent 仍保留在 `/agent` picker 中。
- `Up` / `Down` 在 picker 列表中循环。
- root 显示为 `Main [default]`。
- root 永远位于 picker 第一项。
- `/agent` 打开后，`AgentPickerPanelState.focused` 变为 true。
- picker 未聚焦时，`Up` / `Down` 不进入 agent picker 焦点。

### 13.3 TUI router tests

- 非 active session notification 不会被丢弃。
- active session 切换后渲染目标 transcript。
- prompt submit 使用 active session id。
- prompt 完成事件落回原 session。
- `_meta` metadata-only update 不生成 tool cell。

### 13.4 集成测试

- root spawn subagent 后，输入 `/agent` 可以在输入框下方 picker 中看到 child。
- picker 中使用 `Up` / `Down` 选中 child，按 `Enter` 后可以看到 child transcript。
- 从 child 打开 `/agent`，选中 `Main [default]` 并按 `Enter` 后，可以切回 root，且 root transcript 保持不变。
- child closed 后仍可在 `/agent` picker 中查看历史。

## 14. 分阶段实施

### Phase 1: metadata 与导航状态

1. 定义 subagent `_meta` payload 类型。
2. ACP server 在 new/load session 后发送 snapshot。
3. TUI 解析 `_meta` 并维护 `AgentNavigationState`。
4. 添加 `/agent` 入口和输入框下方 inline picker，只展示列表，暂不切换 transcript。

### Phase 2: 多 session router

1. 引入 `HashMap<SessionId, AppState>`。
2. `SessionNotification` 按 session id 分发。
3. prompt submit 使用 `active_session_id`。
4. `PromptFinished` / `PromptFailed` 绑定 session id。

### Phase 3: session 切换

1. 实现 `SelectAgentSession`。
2. 保存和恢复 per-session `ViewState` / `Composer`。
3. 清屏并重绘目标 transcript。
4. 实现 `/agent` inline picker 的 `Up` / `Down` 选择和 `Enter` 切换。

### Phase 4: lifecycle 增强

1. kernel 发射 `AgentSpawned` / `AgentStatusChange`。
2. ACP adapter 转成 `_meta` upsert/status。
3. closed/final 状态实时更新 `/agent` picker。
4. inactive session approval 提示和跳转。

## 15. 验收标准

1. 只有 root session 时，`/agent` 至少显示 `Main [default]`，并可附带“暂无 subagent”的轻量提示。
2. subagent 创建后，输入 `/agent` 会在输入框下方打开 picker，并显示 `Main [default]` 与 child 的 nickname/role/status。
3. 用户可以在 picker 中用 `Up` / `Down` 选中 root 或 child，并按 `Enter` 切换上下文。
4. 用户可以从 root 切换到 child，再通过选择 `Main [default]` 切回 root。
5. 切换后提交 prompt 会发送给当前 active session。
6. picker 未聚焦时，方向键不抢占正常 composer 编辑行为。
7. inactive session 的 ACP notification 不会丢失。
8. `_meta` 扩展不会污染可见 transcript。
9. 不需要修改 ACP schema。
10. 不新增除 `/agent` 之外的 subagent 专用 slash command。

## 16. 需要避免的设计偏差

1. 不要把 `_meta` 当成 transcript 通道。
2. 不要在 `AppState` 内部硬塞多个 session。
3. 不要解析 model-facing `list_agents` 文本作为 UI 数据源。
4. 不要假设 `AgentSpawned` 已经端到端可用；实现前必须补发射和 ACP 转换。
5. 不要新增除 `/agent` 之外的 subagent 专用 slash command。
6. 不要把 `/agent` picker 做成居中弹窗；它应显示在输入框下方区域。
7. 不要让方向键在 picker 未聚焦时抢占 composer 编辑行为。
8. 不要在第一阶段删除 closed agent，否则用户无法回看 subagent 历史。

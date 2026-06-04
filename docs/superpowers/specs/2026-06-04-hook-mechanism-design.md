# Hook 机制设计方案


## 1. 概述

### 1.1 目标

在 clawcode 的 agent 生命周期关键节点插入用户可配置的 hook 脚本，实现：

- **安全拦截** — 工具执行前审查并阻止危险操作（PreToolUse）
- **输入改写** — 工具执行前自动化改写参数（PreToolUse updatedInput）
- **上下文注入** — 会话/工具执行后向模型注入额外信息（SessionStart, PostToolUse, PostCompact）
- **工作流自动化** — 在生命周期节点触发任意自定义脚本
- **审批增强** — 通过 PermissionRequest hook 参与工具审批流程

### 1.2 非目标

- 不实现 Windows 支持（commandWindows 字段）
- 不实现 `prompt` / `agent` handler 类型（对标 Codex：两者也未实现）
- 不实现 `async` hook（对标 Codex：发现时 emit warning 并 skip）
- 不实现 hook trust 模型（Codex 的信任系统复杂度高，先做能跑的）
- 不内嵌脚本运行时（仅子进程命令方式）

## 2. 架构

### 2.1 Crate 结构

新增 `crates/hook/`，依赖现有 `config`。config crate 新增 hook 配置类型。

```
crates/
├── config/          # 现有，新增 hook 配置类型
│   └── src/
│       └── hook.rs       # HookEventsToml, MatcherGroup, HookHandlerConfig
│
├── hook/            # 新建 — 全部 hook 逻辑
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # pub API: HookEngine, HookConfig, 常量
│       ├── engine/
│       │   ├── mod.rs            # HookEngine — 创建、预览、执行
│       │   ├── discovery.rs      # 从 hooks.json 发现 hook
│       │   ├── dispatcher.rs     # matcher 匹配 + 异步并行调度
│       │   ├── command_runner.rs # 子进程 stdin/stdout 执行
│       │   └── output_parser.rs  # stdout JSON → 业务结果
│       └── events/
│           ├── mod.rs
│           ├── pre_tool_use.rs
│           ├── post_tool_use.rs
│           ├── session_start.rs
│           ├── user_prompt_submit.rs
│           ├── pre_compact.rs
│           ├── post_compact.rs
│           ├── stop.rs
│           └── permission_request.rs
│
├── kernel/          # 现有，在关键路径上插入 hook 调用
│   └── src/
│       ├── session.rs    # SessionStart hook
│       ├── turn.rs       # PreToolUse, PostToolUse, Stop, UserPromptSubmit
│       ├── compaction.rs # PreCompact, PostCompact hook
│       └── approval/     # PermissionRequest hook
```

**分层原则**：`config` 只拥有类型，`hook` 拥有全部逻辑，`kernel` 只在关键位置调用 `HookEngine` 的几个公开方法。

### 2.2 数据流

```
hooks.json ──→ discovery ──→ [ConfiguredHandler] ──→ dispatcher
                                                          │
                         event 触发时 ──→ 匹配 matcher ──→ 并行执行 command
                                                               │
                                               stdin (JSON) ──→ 子进程
                                                               │
                                               stdout (JSON) ←── 子进程
                                                               │
                                               output_parser ──→ 业务决策
```

## 3. Hook 点与集成位置

### 3.1 优先级分阶段

| 批次 | Hook | 注入位置 | 触发时机 |
|------|------|----------|----------|
| P0 | **PreToolUse** | `turn.rs:dispatch_tool()` — tool.execute() 之前 | 工具执行前，可 block 或 rewrite 参数 |
| P0 | **PostToolUse** | `turn.rs:dispatch_tool()` — tool.execute() 之后 | 工具执行成功返回后，可注入 feedback |
| P1 | **SessionStart** | `session.rs:event_stream()` 首个消息前 | 会话启动，可注入初始上下文 |
| P1 | **Stop** | `turn.rs:execute_turn()` — LLM 返回 final 后 | 模型完成回答，可 block 并提示继续 |
| P2 | **UserPromptSubmit** | `turn.rs:execute_turn()` — context.push(user_message) 前 | 用户提交 prompt，可 block/注入 |
| P2 | **PreCompact** | `compaction.rs:compact_history()` — LLM 压缩前 | 自动或手动压缩前 |
| P2 | **PostCompact** | `compaction.rs:compact_history()` — LLM 压缩后 | 压缩完成，可注入上下文 |
| P3 | **PermissionRequest** | `turn.rs:resolve_tool_approval()` 内部 | 需要用户审批时 |
| P3 | **SubagentStart** | agent spawn 时 | 子 agent 启动 |
| P3 | **SubagentStop** | agent 完成时 | 子 agent 停止 |

### 3.2 实现顺序

1. **第一批**（P0）：PreToolUse + PostToolUse — 搭建 hook crate 骨架 + 执行管道
2. **第二批**（P1）：SessionStart + Stop — 扩展生命周期覆盖
3. **第三批**（P2）：UserPromptSubmit + PreCompact + PostCompact
4. **第四批**（P3）：PermissionRequest + SubagentStart + SubagentStop

## 4. 配置格式

### 4.1 配置类型

在 `crates/config/src/hook.rs` 中定义：

```rust
pub struct HooksFile {
    pub hooks: HookEventsToml,
}

pub struct HookEventsToml {
    pub pre_tool_use: Vec<MatcherGroup>,
    pub permission_request: Vec<MatcherGroup>,
    pub post_tool_use: Vec<MatcherGroup>,
    pub pre_compact: Vec<MatcherGroup>,
    pub post_compact: Vec<MatcherGroup>,
    pub session_start: Vec<MatcherGroup>,
    pub user_prompt_submit: Vec<MatcherGroup>,
    pub subagent_start: Vec<MatcherGroup>,
    pub subagent_stop: Vec<MatcherGroup>,
    pub stop: Vec<MatcherGroup>,
}

pub struct MatcherGroup {
    pub matcher: Option<String>,     // regex，None = 匹配所有
    pub hooks: Vec<HookHandlerConfig>,
}

pub enum HookHandlerConfig {
    Command {
        command: String,
        timeout: Option<u64>,         // 默认 600 秒
        status_message: Option<String>,
    },
}
```

### 4.2 配置位置

- 项目级：`.claw/hooks.json`
- 用户级：`~/.claw/hooks.json`

配置加载时合并多层（对标 Codex 的 config layer stack），用户层优先级高于项目层。

### 4.3 配置示例

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "^shell$",
      "hooks": [{
        "command": "python3 .claw/hooks/pre_shell.py",
        "timeout": 60,
        "statusMessage": "检查 shell 命令..."
      }]
    }, {
      "matcher": "apply_patch",
      "hooks": [{
        "command": "node .claw/hooks/pre_edit.js"
      }]
    }],
    "PostToolUse": [{
      "matcher": "^shell$",
      "hooks": [{
        "command": "python3 .claw/hooks/post_shell.py"
      }]
    }],
    "SessionStart": [{
      "hooks": [{
        "command": "python3 .claw/hooks/session_start.py",
        "statusMessage": "加载项目上下文..."
      }]
    }]
  }
}
```

### 4.4 关键设计决策

| 决策 | 选择 | 原因 |
|------|------|------|
| 配置格式 | 仅 JSON | 对标 Codex 主流格式，TOML 嵌套数组体验差 |
| Handler 类型 | 仅 `command` | Codex 的 `prompt`/`agent` 也还未实现，YAGNI |
| Windows 覆盖 | 不支持 | 一期不考虑 Windows |
| `async` 钩子 | 暂不支持 | Codex 也 skip |
| Trust 模型 | 暂不引入 | Codex 的 trust 系统复杂，先做能跑的 |

## 5. Matcher 机制

### 5.1 正则匹配

对标 Codex，matcher 使用正则表达式匹配工具名称。clawcode 钩子工具名称为：

| 工具名称 | 说明 |
|----------|------|
| `shell` | 终端命令执行 |
| `write_stdin` | 向后台进程写入 |
| `apply_patch` | 文件编辑 |
| `spawn_agent` | 子 agent 创建 |
| `skill` | 技能调用 |
| `mcp__*` | MCP 工具（动态名称） |

### 5.2 匹配行为

- `matcher` 为 `None` 或不提供：匹配所有工具
- 无效正则（如 `"["`）：自动忽略 matcher（视为匹配所有），不 emit error
- `"*"` 在 PreToolUse/PostToolUse 中视为匹配所有（等同于 None）
- 匹配器后缀匹配保持不变时自动合并相邻 matcher 组

### 5.3 匹配器验证

非工具相关的 hook 点（UserPromptSubmit, Stop, PreCompact, PostCompact）中，无效的正则 matcher 在发现阶段被忽略并将 handler 视为匹配所有。

## 6. 执行模型

### 6.1 子进程执行

```
stdin (JSON) ──→ [$SHELL -lc <command>] ──→ stdout (JSON 输出)
                       │
                       ├──→ stderr (文本, 用于错误/反馈)
                       └──→ exit code (0=成功, 2=block/需反馈, 非0=错误)
```

| 属性 | 值 | 说明 |
|------|-----|------|
| Shell | `$SHELL` 或 `/bin/sh` | 与 Codex 一致 |
| 工作目录 | 项目 cwd | hook 脚本可访问项目文件 |
| 默认超时 | 600 秒 | 可通过 `timeout` 字段覆盖 |
| kill_on_drop | true | 引擎丢弃时终止子进程 |
| 大输出 | 溢写至临时文件 | stdout 超过阈值时避免占 token |
| env 注入 | 可选 | 后续支持插件环境变量 |

### 6.2 执行并发性

- 同一事件的所有匹配 hook 并行执行（与 Codex 行为一致）
- 所有 hook 完成后统一收集输出并解析判决
- PreToolUse 中，任意 hook deny 立即生效，后续跳过执行

## 7. 通信协议

### 7.1 通用输出层

每个 hook 响应都包含通用字段（对标 Codex `HookUniversalOutputWire`）：

```json
{
  "continue": true,          // false = 阻止后续操作
  "stopReason": "pause",     // block 时的原因
  "suppressOutput": false,   // 是否隐藏模型输出
  "systemMessage": "..."     // 给用户的系统消息
}
```

### 7.2 PreToolUse

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "cwd": "/path/to/project",
  "hook_event_name": "PreToolUse",
  "model": "deepseek-v4-pro",
  "permission_mode": "default",
  "tool_name": "shell",
  "tool_input": { "command": "rm -rf /" },
  "tool_use_id": "call-1"
}
```

**输出（deny / allow with rewrite）：**

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "禁止删除根目录",
    "additionalContext": "请用 trash 替代 rm"
  }
}
```

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "updatedInput": { "command": "ls -la" }
  }
}
```

- `permissionDecision`：`"allow"` | `"deny"` | `"ask"`（一期仅支持 allow/deny）
- `updatedInput` 仅在 `permissionDecision: "allow"` 时有效
- `permissionDecision: "deny"` 必须提供 `permissionDecisionReason`

### 7.3 PostToolUse

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "cwd": "/path/to/project",
  "hook_event_name": "PostToolUse",
  "tool_name": "shell",
  "tool_input": { "command": "ls -la" },
  "tool_response": { "stdout": "...", "exit_code": 0 },
  "tool_use_id": "call-1"
}
```

**输出：**

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "验证输出包含 3 个目录"
  }
}
```

- `additionalContext`：注入到模型上下文中
- `systemMessage`（通用字段）：用户可见的系统消息

### 7.4 SessionStart

**输出：**

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "项目使用 React 18 strict mode，请避免 class component"
  }
}
```

- `source` 字段标识启动原因：`"startup"` | `"resume"` | `"clear"` | `"compact"`

### 7.5 Stop

**输出：**

```json
{
  "continue": false,
  "stopReason": "pause",
  "systemMessage": "Stop hook: 需要先运行测试"
}
```

- `continue: false` 继续 turn 但不结束会话
- stderr 文本自动作为 continuation prompt 注入给模型

### 7.6 UserPromptSubmit

**输出：**

```json
{
  "continue": true,
  "decision": "block",
  "reason": "敏感信息检测：prompt 中包含 API key"
}
```

### 7.7 PreCompact / PostCompact

**输入**包含 `trigger` 字段（`"manual"` | `"auto"`）。**输出**使用通用输出层。

### 7.8 PermissionRequest

保留 `decision.behavior` 字段（`"allow"` | `"deny"`）。一期仅允许 allow/deny 决策。

## 8. HookEngine API

```rust
// crates/hook/src/lib.rs

pub struct HookEngine { /* ... */ }

impl HookEngine {
    /// 从 hooks.json 构建引擎
    pub fn new(config: HookConfig) -> Self;

    // ── 预览（返回将要执行的 hook 列表）──
    pub fn preview_session_start(&self, req: &SessionStartRequest) -> Vec<HookRunSummary>;
    pub fn preview_pre_tool_use(&self, req: &PreToolUseRequest) -> Vec<HookRunSummary>;
    pub fn preview_post_tool_use(&self, req: &PostToolUseRequest) -> Vec<HookRunSummary>;
    pub fn preview_user_prompt_submit(&self, req: &UserPromptSubmitRequest) -> Vec<HookRunSummary>;
    pub fn preview_pre_compact(&self, req: &PreCompactRequest) -> Vec<HookRunSummary>;
    pub fn preview_post_compact(&self, req: &PostCompactRequest) -> Vec<HookRunSummary>;
    pub fn preview_stop(&self, req: &StopRequest) -> Vec<HookRunSummary>;
    pub fn preview_permission_request(&self, req: &PermissionRequestRequest) -> Vec<HookRunSummary>;

    // ── 执行 ──
    pub async fn run_session_start(&self, req: SessionStartRequest) -> SessionStartOutcome;
    pub async fn run_pre_tool_use(&self, req: PreToolUseRequest) -> PreToolUseOutcome;
    pub async fn run_post_tool_use(&self, req: PostToolUseRequest) -> PostToolUseOutcome;
    pub async fn run_user_prompt_submit(&self, req: UserPromptSubmitRequest) -> UserPromptSubmitOutcome;
    pub async fn run_pre_compact(&self, req: PreCompactRequest) -> PreCompactOutcome;
    pub async fn run_post_compact(&self, req: PostCompactRequest) -> PostCompactOutcome;
    pub async fn run_stop(&self, req: StopRequest) -> StopOutcome;
    pub async fn run_permission_request(&self, req: PermissionRequestRequest) -> PermissionRequestOutcome;
}
```

每个 `Outcome` 类型包含：
- `hook_events: Vec<HookCompletedEvent>` — 执行记录
- 事件专属字段（`should_block`, `updated_input`, `additional_contexts` 等）

## 9. Kernel 集成

### 9.1 Session 级别

- `Session` 持有 `Arc<HookEngine>`
- 启动时从 config 构建引擎
- `event_stream()` 在首个消息前调用 `run_session_start()`

### 9.2 Turn 级别

- `TurnContext` 持有 engine 引用
- `execute_turn()` 入口：`run_user_prompt_submit()`
- `dispatch_tool()`：`run_pre_tool_use()` → `tool.execute()` → `run_post_tool_use()`
- 模型 final 后：`run_stop()`

### 9.3 Approval 级别

- `resolve_tool_approval()`：`run_permission_request()` 在用户审批前执行
- hook 的 allow/deny 决策优先于用户审批

## 10. 测试策略

### 10.1 单元测试

- `config::hook` — 配置序列化/反序列化
- `hook::engine::discovery` — 从多层 hooks.json 发现 handler
- `hook::engine::dispatcher` — matcher 匹配逻辑、并行调度
- `hook::engine::command_runner` — 子进程执行（用 echo/true/false 作为测试命令）
- `hook::engine::output_parser` — JSON 输出解析 + 边界情况

### 10.2 集成测试

- 端到端：PreToolUse block 一个 shell 命令
- 端到端：PostToolUse 注入额外上下文
- 端到端：SessionStart 注入初始上下文
- 多层配置合并


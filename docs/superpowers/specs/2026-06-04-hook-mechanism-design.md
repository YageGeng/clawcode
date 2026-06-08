# Hook 机制设计方案


## 1. 概述

### 1.1 目标

在 clawcode 的 agent 生命周期关键节点插入用户可配置的 hook 脚本，实现：

- **安全拦截** — 工具执行前审查并阻止危险操作（PreToolUse）
- **输入改写** — 工具执行前自动化改写参数（PreToolUse updatedInput）
- **上下文注入** — 会话、用户输入、工具执行后向模型注入额外信息（SessionStart, UserPromptSubmit, PostToolUse）
- **工作流自动化** — 在生命周期节点触发任意自定义脚本
- **审批增强** — 通过 PermissionRequest hook 参与工具审批流程

### 1.2 非目标

- 不实现 Windows 支持（`commandWindows` 字段仅保留在配置类型中用于兼容解析，非 Windows 平台忽略）
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
│       └── hook.rs       # HooksFile, HookEventsToml, MatcherGroup, HookHandlerConfig
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
| P0 | **PreToolUse** | `turn.rs:dispatch_tool()` — `tool.invocation()` 之前 | 工具执行前，可 block 或 rewrite 参数；rewrite 后必须用新参数重新构建 invocation 和审批信息 |
| P0 | **PostToolUse** | `turn.rs:dispatch_tool()` — `execute_invocation()` stream 消费完成之后 | 工具执行结束后触发，成功和工具级失败都触发，可注入 feedback/additionalContext |
| P1 | **SessionStart** | `session.rs:run_loop()` 首次处理 `Op::Prompt` / resume 类操作前 | 会话启动，可注入初始上下文 |
| P1 | **Stop** | `turn.rs:execute_turn()` — 无工具调用且收到 final、准备 `return Ok(turn_usage)` 之前 | 模型完成回答，可 block 并提示继续 |
| P2 | **UserPromptSubmit** | `turn.rs:execute_turn()` — context.push(user_message) 前 | 用户提交 prompt，可 block/注入 |
| P2 | **PreCompact** | `compaction.rs:compact_history()` — LLM 压缩前 | 自动或手动压缩前 |
| P2 | **PostCompact** | `compaction.rs:compact_history()` — LLM 压缩后 | 压缩完成后，可记录系统消息或停止后续流程 |
| P3 | **PermissionRequest** | `turn.rs:resolve_tool_approval()` 内部，用户审批事件发送前 | 需要用户审批时；hook 决策优先于 UI 审批 |
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
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct HooksFile {
    #[serde(default)]
    pub hooks: HookEventsToml,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct HookEventsToml {
    #[serde(rename = "PreToolUse", default)]
    pub pre_tool_use: Vec<MatcherGroup>,
    #[serde(rename = "PermissionRequest", default)]
    pub permission_request: Vec<MatcherGroup>,
    #[serde(rename = "PostToolUse", default)]
    pub post_tool_use: Vec<MatcherGroup>,
    #[serde(rename = "PreCompact", default)]
    pub pre_compact: Vec<MatcherGroup>,
    #[serde(rename = "PostCompact", default)]
    pub post_compact: Vec<MatcherGroup>,
    #[serde(rename = "SessionStart", default)]
    pub session_start: Vec<MatcherGroup>,
    #[serde(rename = "UserPromptSubmit", default)]
    pub user_prompt_submit: Vec<MatcherGroup>,
    #[serde(rename = "SubagentStart", default)]
    pub subagent_start: Vec<MatcherGroup>,
    #[serde(rename = "SubagentStop", default)]
    pub subagent_stop: Vec<MatcherGroup>,
    #[serde(rename = "Stop", default)]
    pub stop: Vec<MatcherGroup>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatcherGroup {
    /// Regex or exact matcher. None means match all supported matcher inputs.
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub hooks: Vec<HookHandlerConfig>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum HookHandlerConfig {
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default, rename = "commandWindows", alias = "command_windows")]
        command_windows: Option<String>,
        #[serde(default, rename = "timeout")]
        timeout_sec: Option<u64>,
        #[serde(default)]
        r#async: bool,
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    #[serde(rename = "prompt")]
    Prompt {},
    #[serde(rename = "agent")]
    Agent {},
}
```

### 4.2 配置位置

- 项目级：`.claw/hooks.json`
- 用户级：`~/.claw/hooks.json`

配置加载时按“低优先级在前，高优先级在后”的顺序追加 handler，不做覆盖式合并。项目层先加载，用户层后加载；最终匹配到的 handler 都会执行，执行报告按声明顺序稳定展示。

### 4.3 配置示例

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "^shell$",
      "hooks": [{
        "type": "command",
        "command": "python3 .claw/hooks/pre_shell.py",
        "timeout": 60,
        "statusMessage": "检查 shell 命令..."
      }]
    }, {
      "matcher": "apply_patch",
      "hooks": [{
        "type": "command",
        "command": "node .claw/hooks/pre_edit.js"
      }]
    }],
    "PostToolUse": [{
      "matcher": "^shell$",
      "hooks": [{
        "type": "command",
        "command": "python3 .claw/hooks/post_shell.py"
      }]
    }],
    "SessionStart": [{
      "hooks": [{
        "type": "command",
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
| 配置格式 | 仅 JSON | clawcode 一期只读取 `.claw/hooks.json` / `~/.claw/hooks.json`，不支持在主 TOML 中内联 hooks |
| Handler 类型 | 类型层兼容 `command`/`prompt`/`agent`，执行层仅支持 `command` | 非 command handler 发现时 warning 并 skip，避免兼容配置直接解析失败 |
| Windows 覆盖 | 保留字段但不执行 Windows 分支 | 一期不考虑 Windows runtime，但配置类型不拒绝 `commandWindows` |
| `async` 钩子 | 暂不支持 | 发现时 warning 并 skip，避免调用方误以为后台 hook 已生效 |
| Trust 模型 | 暂不引入 | Codex 的 trust 系统复杂，先做能跑的 |

## 5. Matcher 机制

### 5.1 正则匹配

对标 Codex，matcher 可以是精确名称、`|` 分隔的精确名称集合，或正则表达式。clawcode hook 使用工具 registry 暴露的 canonical tool name；必要时可以提供 matcher alias，但 hook stdin 中始终保留 canonical `tool_name`。

| 工具名称 | 说明 |
|----------|------|
| `shell` | 终端命令执行 |
| `write_stdin` | 向后台进程写入 |
| `apply_patch` | 文件编辑 |
| `spawn_agent` | 子 agent 创建 |
| `skill` | 技能调用 |
| `mcp__*` | MCP 工具（动态名称） |

### 5.2 匹配行为

- `matcher` 为 `None`、不提供、空字符串或 `"*"`：匹配该事件支持的所有 matcher 输入
- 只包含 ASCII 字母、数字、`_`、`|` 的 matcher 按精确匹配处理，例如 `shell|apply_patch`
- 其他 matcher 按 regex 处理，例如 `^mcp__.*__write`
- 无效正则（如 `"["`）：发现阶段 emit warning，并跳过整个 matcher group，不能视为匹配所有
- 同一个 handler 即使同时匹配 canonical name 和 alias，也只能执行一次

### 5.3 匹配器验证

不同事件的 matcher 输入如下：

| Hook | matcher 输入 | 说明 |
|------|--------------|------|
| PreToolUse / PostToolUse / PermissionRequest | tool name + matcher aliases | 用于匹配具体工具 |
| SessionStart | `source` | `startup` / `resume` / `clear` / `compact` |
| PreCompact / PostCompact | `trigger` | `manual` / `auto` |
| SubagentStart / SubagentStop | `agent_type` | 用于按子 agent 类型过滤 |
| UserPromptSubmit / Stop | 无 | 配置中的 matcher 被忽略，所有 handler 都执行 |

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
- 所有 hook 完成后统一收集输出并解析判决；报告顺序按配置声明顺序稳定输出
- PreToolUse 中，任意 hook deny 生效并阻止工具执行；由于同一事件并发执行，不做“第一个 deny 立即取消其他 hook”的优化
- 多个 PreToolUse hook 返回 `updatedInput` 时，采用最后完成的 rewrite；如果任意 hook deny，则丢弃所有 rewrite

## 7. 通信协议

### 7.1 通用输出层

每个 hook 响应都包含通用字段（对标 Codex `HookUniversalOutputWire`）：

```json
{
  "continue": true,
  "stopReason": "pause",
  "suppressOutput": false,
  "systemMessage": "..."
}
```

### 7.2 PreToolUse

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "agent_type": "worker",
  "transcript_path": "/path/to/transcript.jsonl",
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
- `continue: false`、`stopReason`、`suppressOutput` 在 PreToolUse 中不支持；出现时视为 hook 输出无效
- 如果输出 `updatedInput`，kernel 必须使用新输入重新调用 `tool.invocation()` 并重新计算 `exec_approval_requirement()`

### 7.3 PostToolUse

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "agent_type": "worker",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "PostToolUse",
  "model": "deepseek-v4-pro",
  "permission_mode": "default",
  "tool_name": "shell",
  "tool_input": { "command": "ls -la" },
  "tool_response": { "content": "...", "is_error": false },
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
- `continue: false` 表示停止当前工具循环，并将 `stopReason` / `reason` 作为模型反馈
- `decision: "block"` 表示把 `reason` 作为下一轮模型反馈，但不标记工具执行本身失败

### 7.4 SessionStart

**输入：**

```json
{
  "session_id": "sess-1",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "SessionStart",
  "model": "deepseek-v4-pro",
  "permission_mode": "default",
  "source": "startup"
}
```

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
- `additionalContext` 注入为一条 model-visible user context message，并持久化到 replay history；不拼接到 system prompt，避免影响后续动态 preamble 生成
- `continue: false` 停止会话启动后的首个 turn，不创建 continuation prompt

### 7.5 Stop

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "Stop",
  "model": "deepseek-v4-pro",
  "permission_mode": "default",
  "stop_hook_active": false,
  "last_assistant_message": "最终回答文本"
}
```

**输出：**

```json
{
  "continue": false,
  "stopReason": "pause",
  "systemMessage": "Stop hook: 需要先运行测试"
}
```

- `continue: false` 停止当前 turn，不创建 continuation prompt
- `decision: "block"` 或 exit code 2 的 stderr 文本作为 continuation prompt 注入给模型

### 7.6 UserPromptSubmit

**输入：**

```json
{
  "session_id": "sess-1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "agent_type": "worker",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "UserPromptSubmit",
  "model": "deepseek-v4-pro",
  "permission_mode": "default",
  "prompt": "用户输入"
}
```

**输出：**

```json
{
  "continue": true,
  "decision": "block",
  "reason": "敏感信息检测：prompt 中包含 API key"
}
```

- `decision: "block"` 必须提供非空 `reason`
- `hookSpecificOutput.additionalContext` 会在用户 prompt 之后作为额外上下文注入本 turn

### 7.7 PreCompact / PostCompact

**输入**包含 `trigger` 字段（`"manual"` | `"auto"`）。**输出**使用通用输出层。一期不支持 `additionalContext`，PostCompact 只用于记录 warning、系统消息或停止后续流程。

### 7.8 PermissionRequest

**输入**与 PreToolUse 类似，但不包含 `tool_use_id`。保留 `decision.behavior` 字段（`"allow"` | `"deny"`）。一期仅允许 allow/deny 决策：

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "deny",
      "message": "禁止执行该命令"
    }
  }
}
```

- 任意 deny 优先于 allow
- 没有 hook 决策时继续原有 `resolve_tool_approval()` 流程
- `updatedInput`、`updatedPermissions`、`interrupt` 为保留字段，一期出现时视为 hook 输出无效

## 8. HookEngine API

```rust
// crates/hook/src/lib.rs

pub struct HookEngine { /* ... */ }

impl HookEngine {
    /// Build an engine from discovered hook configuration.
    pub fn new(config: HookConfig) -> Self;

    // Preview the hook runs that would execute for each event.
    pub fn preview_session_start(&self, req: &SessionStartRequest) -> Vec<HookRunSummary>;
    pub fn preview_pre_tool_use(&self, req: &PreToolUseRequest) -> Vec<HookRunSummary>;
    pub fn preview_post_tool_use(&self, req: &PostToolUseRequest) -> Vec<HookRunSummary>;
    pub fn preview_user_prompt_submit(&self, req: &UserPromptSubmitRequest) -> Vec<HookRunSummary>;
    pub fn preview_pre_compact(&self, req: &PreCompactRequest) -> Vec<HookRunSummary>;
    pub fn preview_post_compact(&self, req: &PostCompactRequest) -> Vec<HookRunSummary>;
    pub fn preview_stop(&self, req: &StopRequest) -> Vec<HookRunSummary>;
    pub fn preview_permission_request(&self, req: &PermissionRequestRequest) -> Vec<HookRunSummary>;

    // Execute matching hooks and fold their event-specific outcomes.
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
- `spawn_thread()` 从 config 构建 `HookEngine` 并放入 runtime / handle
- `run_loop()` 维护 `session_start_ran` 标记；首个 prompt/resume 操作执行 turn 前调用 `run_session_start()`
- `SessionStart.additionalContext` 作为一条普通 model-visible user context message 注入 `ContextManager`，并通过 `SessionRecorder` 持久化，保证 replay 与 live history 一致

### 9.2 Turn 级别

- `TurnContext` 持有 engine 引用
- `execute_turn()` 入口：`run_user_prompt_submit()` 必须发生在 `context.push(user_message)` 前；block 时不持久化用户 prompt，additionalContext 在用户 prompt 被接受后注入
- `dispatch_tool()` 精确顺序：
  1. 收到 provider 的 tool name + raw arguments
  2. 调用 `run_pre_tool_use()`，输入为 raw arguments
  3. 如果 deny，发送 failed tool call update，并把 deny reason 作为 tool result 返回给模型
  4. 如果有 `updatedInput`，替换 `tool_call.function.arguments`
  5. 使用最终 arguments 调用 `tool.invocation()` 并计算 `exec_approval_requirement()`
  6. 调用 `resolve_tool_approval()`，需要 UI 审批时先运行 `PermissionRequest`
  7. 调用 `tool.execute_invocation()` 并消费 streaming output
  8. 基于最终 model-facing output 构造 `PostToolUseRequest`
  9. 调用 `run_post_tool_use()`；`additionalContext` 作为独立 user context message 注入下一次模型请求，不混入 provider tool result
- 模型 final 后且无 tool calls 时：先 `run_stop()`；如果 `decision:block` 或 exit code 2 产生 continuation prompt，则把 continuation 作为新的 user message 注入并继续 loop；如果没有 block，才结束 turn

### 9.3 Approval 级别

- `resolve_tool_approval()`：`run_permission_request()` 在用户审批前执行，输入使用最终 tool arguments
- hook 的 allow/deny 决策优先于用户审批
- deny 直接返回 `ReviewDecision::Denied` 语义的 model-facing rejection；allow 跳过 UI 审批但不写入 session approval cache

## 10. 测试策略

### 10.1 单元测试

- `config::hook` — 配置序列化/反序列化
- `hook::engine::discovery` — 从多层 hooks.json 发现 handler
- `hook::engine::dispatcher` — matcher 匹配逻辑、无效 matcher warning + skip、alias 去重、并行调度
- `hook::engine::command_runner` — 子进程执行（用 echo/true/false 作为测试命令）
- `hook::engine::output_parser` — JSON 输出解析 + 边界情况

### 10.2 集成测试

- 端到端：PreToolUse block 一个 shell 命令
- 端到端：PreToolUse `updatedInput` 后重新构建 invocation，并用新参数计算审批与实际执行
- 端到端：PostToolUse 注入额外上下文
- 端到端：PostToolUse 在工具级失败时仍执行，并能把 feedback 传回下一次模型请求
- 端到端：SessionStart 注入初始上下文
- 端到端：Stop block 后创建 continuation prompt 并继续 turn
- 端到端：PermissionRequest deny 优先于用户审批事件
- 多层配置合并
- Hook timeout、exit code 2、JSON-looking invalid stdout、空命令、async handler skip

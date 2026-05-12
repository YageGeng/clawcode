# 工具执行层设计方案

## 目标

将 `Tool` trait + `ToolRegistry` 从 kernel 移入独立 `tools` crate。实现 shell 命令执行、文件读写、patch 替换四个内置工具。审批流程对齐 Codex 的同步等待模式（oneshot channel），执行前检查审批策略，用户决策后继续或中断。

## Crate 结构

```
crates/tools/               # 新建独立 crate
├── Cargo.toml
└── src/
    ├── lib.rs              # 模块声明 + register_builtins()
    ├── shell.rs            # Shell 命令执行
    ├── file.rs             # ReadFile, WriteFile, ApplyPatch
    └── mcp.rs              # McpTool trait 预留桩

crates/kernel/src/
├── tool.rs                  # 原 Tool trait 移走，改为引用 tools crate
├── turn.rs                 # execute_turn 内联审批等待逻辑
└── session.rs              # run_loop 新增 pending_approvals 处理
```

## Tool trait（tools crate）

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

    /// Whether this specific invocation requires user approval.
    /// Default: true (safe-by-default).
    fn needs_approval(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    /// Execute the tool with the given JSON arguments.
    /// Returns the output string on success, or an error message on failure.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        cwd: &Path,
    ) -> Result<String, String>;
}
```

## 内置工具

| 工具名 | needs_approval | 功能 | 参数 |
|---|---|---|---|
| `shell` | `true` | 执行 shell 命令 | `command: String`, `cwd: Option<String>` |
| `read_file` | `false` | 读取文件内容 | `path: String`, `offset: Option<usize>`, `limit: Option<usize>` |
| `write_file` | `true` | 创建/覆盖文件 | `path: String`, `content: String` |
| `apply_patch` | `true` | 搜索替换文本块 | `path: String`, `search: String`, `replace: String` |

### Shell 实现

- 调用 `/bin/sh -c <command>`
- 超时 30 秒
- 返回 stdout + stderr + exit code，截断至 4096 字符

### 文件工具实现

- `read_file`：打开文件，按 offset/limit 切片返回
- `write_file`：写入完整内容，自动创建父目录
- `apply_patch`：读文件内容，`str::replace(search, replace)`，结果不同则写回

## 审批流程

采用 Codex 的同步等待模式：工具执行前如果 `needs_approval()` 为 true，先发送审批请求事件，然后通过 `oneshot::channel()` 等待客户端返回审批决策。

### execute_turn 内联流程

```
LlmStreamEvent::ToolCall { tool_call, internal_call_id }

    1. tool = ToolRegistry.get(&tool_call.function.name)
    2. tool.needs_approval(&args) ?
         false → 直接 execute，发送 ToolCallUpdate
         true  → 进入审批

    审批流程:
      a. 发送 Event::ExecApprovalRequested {
           session_id, call_id: internal_call_id,
           tool_name, arguments, cwd
         }
      b. let (tx, rx) = oneshot::channel();
      c. pending_approvals[internal_call_id] = tx;
      d. let decision = rx.await;  // 阻塞等待
      e. match decision {
           AllowOnce | AllowAlways → execute, 发送 ToolCallUpdate { Completed }
           RejectOnce | RejectAlways | Abort → 发送 ToolCallUpdate { Failed }
         }
```

### Session 接收审批响应

```rust
// run_loop 新增 arm
Some(Op::ExecApprovalResponse { call_id, decision, .. }) => {
    if let Some(tx) = pending_approvals.remove(&call_id) {
        let _ = tx.send(decision);
    }
}
```

### 协议新增

```rust
// event.rs 新增
Event::ExecApprovalRequested {
    session_id: SessionId,
    call_id: String,
    tool_name: String,
    arguments: serde_json::Value,
    cwd: PathBuf,
}

// op.rs 新增
Op::ExecApprovalResponse {
    call_id: String,
    decision: ReviewDecision,
}
Op::PatchApprovalResponse {
    call_id: String,
    decision: ReviewDecision,
}

// permission.rs 新增（与 PermissionOptionKind 同文件）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Abort,
}
```

## ACP 映射

| 内部 Event | ACP 输出 |
|---|---|
| `ExecApprovalRequested` | `RequestPermissionRequest` 发送至客户端并等待响应 |
| `AgentMessageChunk` | `SessionUpdate::AgentMessageChunk` |
| `ToolCall` | `SessionUpdate::ToolCall(ToolCall)` |
| `ToolCallUpdate` | `SessionUpdate::ToolCallUpdate(ToolCallUpdate)` |
| `TurnComplete` | `PromptResponse.stop_reason` |

## MCP 预留

```rust
// tools/src/mcp.rs

/// Reserved trait for future MCP server integration.
#[async_trait]
pub trait McpTool: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, String>;
}

/// Placeholder — does nothing, returns unimplemented.
pub struct NoopMcp;

impl McpTool for NoopMcp {
    fn name(&self) -> &str { "noop_mcp" }
    async fn execute(&self, _: serde_json::Value) -> Result<String, String> {
        Err("MCP not yet implemented".into())
    }
}
```

## 依赖关系

```
tools ──► protocol  (ToolDefinition, ReviewDecision, PermissionOptionKind)
tools ──► tokio     (process::Command, fs)

kernel ──► tools    (Tool, ToolRegistry, register_builtins)
kernel ──► protocol (Event, Op, SessionState types)
kernel ──► provider (LlmFactory, ArcLlm)
```

## 实施计划概述

1. 新建 `crates/tools/` crate，移入 `Tool` trait + `ToolRegistry`
2. 实现 `shell.rs`、`file.rs`、`mcp.rs`
3. 添加 `ReviewDecision` 到 protocol
4. 添加 `Op::ExecApprovalResponse`、`Event::ExecApprovalRequested`
5. 更新 kernel：`execute_turn` 审批等待、`run_loop` 处理响应
6. 更新 ACP agent 翻译 `ExecApprovalRequested` → ACP `RequestPermissionRequest`

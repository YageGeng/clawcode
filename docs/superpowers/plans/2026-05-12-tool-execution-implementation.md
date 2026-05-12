# 工具执行层实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 创建独立 `tools` crate，实现 Shell/文件读写/Patch 四个内置工具，审批流程对齐 Codex 同步等待模式。

**架构：** Tool trait + ToolRegistry 移入 tools crate。execute_turn 中 tool call 拦截 → needs_approval 检查 → oneshot 阻塞等客户端响应。Session run_loop 接收 Op::ExecApprovalResponse 唤醒 oneshot。

**技术栈：** tokio (process::Command, fs, sync::oneshot), async-trait, serde_json, protocol (ToolDefinition, ReviewDecision)

**规格文档：** `docs/superpowers/specs/2026-05-12-tool-execution-design.md`

---

## 文件清单

| 文件 | 操作 | 用途 |
|---|---|---|
| `crates/tools/Cargo.toml` | 新建 | 独立 tools crate |
| `crates/tools/src/lib.rs` | 新建 | 模块声明 + Tool trait + ToolRegistry + register_builtins |
| `crates/tools/src/shell.rs` | 新建 | Shell 命令执行 |
| `crates/tools/src/file.rs` | 新建 | ReadFile, WriteFile, ApplyPatch |
| `crates/tools/src/mcp.rs` | 新建 | McpTool trait 预留 |
| `crates/protocol/src/permission.rs` | 修改 | 新增 ReviewDecision 枚举 |
| `crates/protocol/src/event.rs` | 修改 | 新增 ExecApprovalRequested |
| `crates/protocol/src/op.rs` | 修改 | 新增 ExecApprovalResponse, PatchApprovalResponse |
| `Cargo.toml`（workspace 根） | 修改 | 新增 tools = { path = "crates/tools" } |
| `crates/kernel/Cargo.toml` | 修改 | 新增 tools 依赖，移除不再需要的依赖 |
| `crates/kernel/src/tool.rs` | 删除 | 移入 tools crate |
| `crates/kernel/src/lib.rs` | 修改 | 移除 tool 模块，引用 tools crate |
| `crates/kernel/src/turn.rs` | 修改 | execute_turn 增加审批等待逻辑 |
| `crates/kernel/src/session.rs` | 修改 | run_loop 增加 pending_approvals 处理 |
| `crates/acp/src/agent.rs` | 修改 | ExecApprovalRequested → RequestPermissionRequest |

---

### 任务 1：创建 tools crate + 移动 Tool trait

**文件：**
- 创建：`crates/tools/Cargo.toml`
- 创建：`crates/tools/src/lib.rs`
- 创建：`crates/tools/src/mcp.rs`
- 修改：`Cargo.toml`（workspace 根）
- 修改：`crates/kernel/Cargo.toml`
- 删除：`crates/kernel/src/tool.rs`
- 修改：`crates/kernel/src/lib.rs`

- [ ] **步骤1：创建 tools Cargo.toml**

```toml
[package]
name = "tools"
edition.workspace = true
version.workspace = true
description = "Built-in agent tools: shell execution, file I/O, MCP stub"

[dependencies]
protocol = { path = "../protocol" }

async-trait = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["process", "fs", "time"] }
thiserror = { workspace = true }
```

- [ ] **步骤2：添加 workspace 成员依赖**

在 `Cargo.toml`（workspace 根）末尾添加：

```toml
tools = { path = "crates/tools" }
```

- [ ] **步骤3：更新 kernel Cargo.toml**

添加 `tools` 依赖：

```toml
tools = { path = "../tools" }
```

- [ ] **步骤4：编写 tools/src/lib.rs（Tool trait + ToolRegistry + MockEchoTool + register_builtins）**

```rust
//! Agent tool registry and built-in tools.

pub mod file;
pub mod mcp;
pub mod shell;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

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
    /// Default: `true` (safe-by-default).
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

/// Registry of available tools, keyed by tool name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Build tool definitions for the LLM completion request.
    #[must_use]
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
        match self.get(name) {
            Some(tool) => tool.execute(arguments, cwd).await,
            None => Err(format!("unknown tool: {name}")),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register all built-in tools into the given registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(Arc::new(shell::ShellCommand::new()));
    registry.register(Arc::new(file::ReadFile::new()));
    registry.register(Arc::new(file::WriteFile::new()));
    registry.register(Arc::new(file::ApplyPatch::new()));
}

// ── Mock tool for testing ──

/// A mock tool that echoes its arguments — for testing the tool pipeline.
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
            },
            "required": ["message"]
        })
    }
    fn needs_approval(&self, _: &serde_json::Value) -> bool { false }
    async fn execute(&self, args: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let msg = args["message"].as_str().unwrap_or("(no message)");
        Ok(format!("echo: {msg}"))
    }
}
```

- [ ] **步骤5：编写 tools/src/mcp.rs（预留桩）**

```rust
//! Reserved trait for future MCP server integration.

use async_trait::async_trait;

/// Reserved trait for future MCP server integration.
#[async_trait]
pub trait McpTool: Send + Sync {
    /// MCP tool name.
    fn name(&self) -> &str;
    /// Execute the MCP tool.
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, String>;
}

/// Placeholder — does nothing, returns an error indicating MCP is not yet implemented.
pub struct NoopMcp;

#[async_trait]
impl McpTool for NoopMcp {
    fn name(&self) -> &str {
        "noop_mcp"
    }
    async fn execute(&self, _: serde_json::Value) -> Result<String, String> {
        Err("MCP not yet implemented".into())
    }
}
```

- [ ] **步骤6：删除 kernel/src/tool.rs，更新 kernel/src/lib.rs**

删除 `crates/kernel/src/tool.rs`。

修改 `crates/kernel/src/lib.rs`：
- 移除 `pub mod tool;`
- 将 `use crate::tool::ToolRegistry;` 改为 `use tools::ToolRegistry;`

```rust
// 移除
pub mod tool;

// 导入改为
use tools::ToolRegistry;
```

- [ ] **步骤7：更新 kernel/src/turn.rs 和 kernel/src/session.rs**

将 `use crate::tool::*` 改为 `use tools::*`。

- [ ] **步骤8：构建验证**

```bash
cargo check -p tools -p kernel
```

- [ ] **步骤9：运行测试**

```bash
cargo test -p tools -p kernel
```

---

### 任务 2：实现 shell.rs

**文件：**
- 创建：`crates/tools/src/shell.rs`

- [ ] **步骤1：编写 shell.rs**

```rust
//! Shell command execution tool.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::Tool;

const OUTPUT_MAX_LEN: usize = 4096;
const SHELL_TIMEOUT_SECS: u64 = 30;

/// Executes arbitrary shell commands.
pub struct ShellCommand;

impl ShellCommand {
    /// Create a new shell tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ShellCommand {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellCommand {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout, stderr, and exit code"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the command"
                }
            },
            "required": ["command"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cwd: &Path,
    ) -> Result<String, String> {
        let command = arguments["command"]
            .as_str()
            .ok_or("missing 'command' argument")?
            .to_string();

        let work_dir = arguments["cwd"]
            .as_str()
            .map(Path::new)
            .unwrap_or(cwd);

        let result = timeout(
            Duration::from_secs(SHELL_TIMEOUT_SECS),
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&command)
                .current_dir(work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut formatted = format!(
                    "exit code: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status.code().unwrap_or(-1),
                    truncate(&stdout),
                    truncate(&stderr),
                );
                if formatted.len() > OUTPUT_MAX_LEN {
                    formatted.truncate(OUTPUT_MAX_LEN);
                    formatted.push_str("\n... (output truncated)");
                }
                Ok(formatted)
            }
            Ok(Err(e)) => Err(format!("command execution failed: {e}")),
            Err(_) => Err(format!(
                "command timed out after {SHELL_TIMEOUT_SECS}s"
            )),
        }
    }
}

fn truncate(s: &str) -> &str {
    if s.len() > OUTPUT_MAX_LEN / 2 {
        &s[..OUTPUT_MAX_LEN / 2]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_echo_hello() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({"command": "echo hello"}),
                Path::new("."),
            )
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello"));
        assert!(output.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn shell_missing_command() {
        let tool = ShellCommand::new();
        let result = tool.execute(serde_json::json!({}), Path::new(".")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_needs_approval() {
        let tool = ShellCommand::new();
        assert!(tool.needs_approval(&serde_json::json!({"command": "ls"})));
    }
}
```

- [ ] **步骤2：构建并运行测试**

```bash
cargo test -p tools -- shell
```

---

### 任务 3：实现 file.rs

**文件：**
- 创建：`crates/tools/src/file.rs`

- [ ] **步骤1：编写 file.rs**

```rust
//! File I/O tools: read, write, and patch.

use std::path::Path;

use async_trait::async_trait;
use tokio::fs;

use crate::Tool;

// ── ReadFile ──

/// Reads a file's content, optionally limited by offset and line count.
pub struct ReadFile;

impl ReadFile {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file's content, with optional offset and limit (line numbers)"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Max number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cwd: &Path,
    ) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;

        let content = fs::read_to_string(path)
            .await
            .map_err(|e| format!("failed to read {path}: {e}"))?;

        let lines: Vec<&str> = content.lines().collect();
        let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
        let limit = arguments["limit"].as_u64().map(|n| n as usize);

        let start = offset.min(lines.len());
        let end = limit
            .map(|l| (start + l).min(lines.len()))
            .unwrap_or(lines.len());

        let result: String = lines[start..end].join("\n");
        Ok(result)
    }
}

// ── WriteFile ──

/// Creates or overwrites a file with the given content.
pub struct WriteFile;

impl WriteFile {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cwd: &Path,
    ) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let content = arguments["content"]
            .as_str()
            .ok_or("missing 'content' argument")?;

        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create parent dir: {e}"))?;
        }

        fs::write(path, content)
            .await
            .map_err(|e| format!("failed to write {path}: {e}"))?;

        Ok(format!("wrote {} bytes to {path}", content.len()))
    }
}

// ── ApplyPatch ──

/// Searches for text in a file and replaces it.
pub struct ApplyPatch;

impl ApplyPatch {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ApplyPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Replace a text block in a file. Searches for the exact `search` string and replaces it with `replace`."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "search": { "type": "string", "description": "Exact text to find" },
                "replace": { "type": "string", "description": "Text to replace with" }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cwd: &Path,
    ) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let search = arguments["search"]
            .as_str()
            .ok_or("missing 'search' argument")?;
        let replace = arguments["replace"]
            .as_str()
            .ok_or("missing 'replace' argument")?;

        let original = fs::read_to_string(path)
            .await
            .map_err(|e| format!("failed to read {path}: {e}"))?;

        if let Some(patched) = original.split(search).nth(1) {
            // Found the search string — apply replacement once
            let result = original.replacen(search, replace, 1);
            fs::write(path, &result)
                .await
                .map_err(|e| format!("failed to write {path}: {e}"))?;
            Ok(format!(
                "patched {path}: replaced 1 occurrence, {} bytes",
                result.len()
            ))
        } else {
            Err(format!("search text not found in {path}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[tokio::test]
    async fn read_file_content() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "line1\nline2\nline3").unwrap();
        let path = f.path().to_string_lossy().to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(serde_json::json!({"path": path}), Path::new("."))
            .await
            .unwrap();
        assert!(result.contains("line2"));
    }

    #[tokio::test]
    async fn read_file_with_limit() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "a\nb\nc\nd").unwrap();
        let path = f.path().to_string_lossy().to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path, "offset": 1, "limit": 2}),
                Path::new("."),
            )
            .await
            .unwrap();
        assert_eq!(result, "b\nc");
    }

    #[tokio::test]
    async fn read_file_needs_no_approval() {
        let tool = ReadFile::new();
        assert!(!tool.needs_approval(&serde_json::json!({"path": "x"})));
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let path_str = path.to_string_lossy().to_string();

        let write = WriteFile::new();
        write
            .execute(
                serde_json::json!({"path": path_str, "content": "hello world"}),
                Path::new("."),
            )
            .await
            .unwrap();

        let read = ReadFile::new();
        let result = read
            .execute(serde_json::json!({"path": path_str}), Path::new("."))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn apply_patch_replaces_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patch.txt");
        let path_str = path.to_string_lossy().to_string();

        std::fs::write(&path, "before\ntarget\nafter").unwrap();

        let tool = ApplyPatch::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path_str, "search": "target", "replace": "REPLACED"}),
                Path::new("."),
            )
            .await
            .unwrap();
        assert!(result.contains("patched"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "before\nREPLACED\nafter");
    }

    #[tokio::test]
    async fn apply_patch_search_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "hello").unwrap();

        let tool = ApplyPatch::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path_str, "search": "xyz", "replace": "abc"}),
                Path::new("."),
            )
            .await;
        assert!(result.is_err());
    }
}
```

- [ ] **步骤2：添加 tempfile 到 tools Cargo.toml 的 dev-dependencies**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **步骤3：构建并运行测试**

```bash
cargo test -p tools -- file
```

---

### 任务 4：更新 protocol 新增类型

**文件：**
- 修改：`crates/protocol/src/permission.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/protocol/src/op.rs`

- [ ] **步骤1：在 permission.rs 新增 ReviewDecision**

```rust
/// User's decision in response to a tool approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Allow this single execution.
    AllowOnce,
    /// Allow and persist for future identical requests.
    AllowAlways,
    /// Reject this single execution.
    RejectOnce,
    /// Reject and persist for future identical requests.
    RejectAlways,
    /// Abort the entire turn.
    Abort,
}
```

- [ ] **步骤2：在 event.rs 新增 ExecApprovalRequested**

```rust
    /// The kernel requests user approval before executing a tool.
    ExecApprovalRequested {
        session_id: SessionId,
        /// Identifies the tool call awaiting approval.
        call_id: String,
        /// Name of the tool being requested.
        tool_name: String,
        /// JSON arguments for the tool invocation.
        arguments: serde_json::Value,
        /// Working directory for the tool execution.
        cwd: PathBuf,
    },
```

- [ ] **步骤3：在 op.rs 新增 ExecApprovalResponse 和 PatchApprovalResponse**

```rust
    /// User's response to an exec approval request.
    ExecApprovalResponse {
        /// Matches the `call_id` from ExecApprovalRequested.
        call_id: String,
        /// User's decision.
        decision: ReviewDecision,
    },
    /// User's response to a patch approval request.
    PatchApprovalResponse {
        call_id: String,
        decision: ReviewDecision,
    },
```

- [ ] **步骤4：确认 ReviewDecision import**

在 `op.rs` 顶部添加 `use crate::permission::ReviewDecision;`。

- [ ] **步骤5：构建验证**

```bash
cargo check -p protocol
```

---

### 任务 5：更新 kernel — execute_turn 审批等待

**文件：**
- 修改：`crates/kernel/src/turn.rs`
- 修改：`crates/kernel/src/session.rs`

- [ ] **步骤1：在 TurnContext 新增 pending_approvals 字段**

```rust
/// Immutable snapshot of all context needed to execute a single turn.
#[derive(Clone, typed_builder::TypedBuilder)]
pub(crate) struct TurnContext {
    pub session_id: SessionId,
    pub llm: ArcLlm,
    pub tools: Arc<ToolRegistry>,
    pub cwd: PathBuf,
    // ── approval ──
    #[builder(default)]
    pub pending_approvals: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<protocol::ReviewDecision>>>>,
}
```

`pending_approvals` 用 `Arc<Mutex<>>` 因为 TurnContext 需要 Clone 且审批 channel 需要在 turn 之间共享。

**不在 builder 中暴露：** 添加 `#[builder(default)]` 使调用方不需要显式传递。

- [ ] **步骤2：在 execute_turn 中，替换内联工具执行逻辑为审批等待**

```rust
LlmStreamEvent::ToolCall { tool_call, internal_call_id } => {
    let _ = tx_event.send(Event::ToolCall {
        session_id: ctx.session_id.clone(),
        agent_path: AgentPath::root(),
        call_id: internal_call_id.clone(),
        name: tool_call.function.name.clone(),
        arguments: tool_call.function.arguments.clone(),
        status: ToolCallStatus::InProgress,
    });

    // Check approval
    let tool = ctx.tools.get(&tool_call.function.name);
    let needs_approval = tool
        .as_ref()
        .is_some_and(|t| t.needs_approval(&tool_call.function.arguments));

    if needs_approval {
        // Send approval request
        let _ = tx_event.send(Event::ExecApprovalRequested {
            session_id: ctx.session_id.clone(),
            call_id: internal_call_id.clone(),
            tool_name: tool_call.function.name.clone(),
            arguments: tool_call.function.arguments.clone(),
            cwd: ctx.cwd.clone(),
        });

        // Wait for user decision
        let (tx_approve, rx_approve) = tokio::sync::oneshot::channel();
        {
            let mut approvals = ctx.pending_approvals.lock().await;
            approvals.insert(internal_call_id.clone(), tx_approve);
        }

        match rx_approve.await {
            Ok(decision) => match decision {
                ReviewDecision::AllowOnce
                | ReviewDecision::AllowAlways => {
                    // Execute
                    let output = ctx.tools.execute(
                        &tool_call.function.name,
                        tool_call.function.arguments.clone(),
                        &ctx.cwd,
                    ).await;
                    match output {
                        Ok(out) => {
                            assistant_content.push(AssistantContent::ToolCall(tool_call));
                            let _ = tx_event.send(Event::ToolCallUpdate {
                                session_id: ctx.session_id.clone(),
                                call_id: internal_call_id,
                                output_delta: Some(out),
                                status: Some(ToolCallStatus::Completed),
                            });
                        }
                        Err(err) => {
                            let _ = tx_event.send(Event::ToolCallUpdate {
                                session_id: ctx.session_id.clone(),
                                call_id: internal_call_id,
                                output_delta: Some(err),
                                status: Some(ToolCallStatus::Failed),
                            });
                        }
                    }
                }
                ReviewDecision::RejectOnce
                | ReviewDecision::RejectAlways => {
                    let _ = tx_event.send(Event::ToolCallUpdate {
                        session_id: ctx.session_id.clone(),
                        call_id: internal_call_id,
                        output_delta: Some("rejected by user".into()),
                        status: Some(ToolCallStatus::Failed),
                    });
                }
                ReviewDecision::Abort => {
                    return Err(KernelError::Cancelled);
                }
            },
            Err(_) => {
                // oneshot dropped — treat as abort
                return Err(KernelError::Cancelled);
            }
        }
    } else {
        // No approval needed — execute directly
        let output = ctx.tools.execute(
            &tool_call.function.name,
            tool_call.function.arguments.clone(),
            &ctx.cwd,
        ).await;
        match output {
            Ok(out) => { /* ... same as above */ }
            Err(err) => { /* ... same as above */ }
        }
    }
}
```

- [ ] **步骤3：在 session.rs 的 run_loop 新增审批响应处理**

```rust
Some(Op::ExecApprovalResponse { call_id, decision }) => {
    // Route to pending approval in the active turn
    // The pending_approvals map is held in TurnContext, which is
    // owned by the current execute_turn invocation.
    // For now, store a shared approvals map in SessionRuntime.

    if let Some(tx) = rt.pending_approvals.lock().await.remove(&call_id) {
        let _ = tx.send(decision);
    }
}
```

同时在 `Session` 结构体新增字段：

```rust
pub(crate) pending_approvals: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<protocol::ReviewDecision>>>>,
```

- [ ] **步骤4：在 spawn_thread 中将 pending_approvals 传递给 TurnContext**

```rust
let pending_approvals: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<ReviewDecision>>>> =
    Arc::new(tokio::sync::Mutex::new(HashMap::new()));

let runtime = Session {
    // ...
    pending_approvals: pending_approvals.clone(),
};
```

- [ ] **步骤5：构建验证**

```bash
cargo check -p kernel
```

---

### 任务 6：更新 ACP agent — ExecApprovalRequested 翻译

**文件：**
- 修改：`crates/acp/src/agent.rs`

- [ ] **步骤1：在 handle_prompt 的事件翻译中新增**

```rust
Event::ExecApprovalRequested {
    call_id,
    tool_name,
    arguments,
    ..
} => {
    // Send permission request to client
    let req = RequestPermissionRequest::new(
        call_id,
        format!("Allow execution of: {tool_name} with args: {arguments}"),
        vec![
            PermissionOption::new(
                "allow_once".to_string(),
                "Allow Once".to_string(),
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                "reject_once".to_string(),
                "Reject".to_string(),
                PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    // Send request and wait for response
    let response: RequestPermissionResponse = cx
        .send_request(req)
        .block_task()
        .await?;

    // Convert ACP permission decision back to internal ReviewDecision
    let decision = match response.outcome {
        PermissionOutcome::Selected(sel) => match sel.id.as_str() {
            "allow_once" => ReviewDecision::AllowOnce,
            "reject_once" => ReviewDecision::RejectOnce,
            _ => ReviewDecision::RejectOnce,
        },
        _ => ReviewDecision::RejectOnce,
    };

    // Send decision back to the kernel via Op
    let _ = handle_kernel
        .send_op(Op::ExecApprovalResponse {
            call_id,
            decision,
        })
        .await;
}
```

---

### 任务 7：最终验证

- [ ] **步骤1：构建全 workspace**

```bash
cargo build
```

- [ ] **步骤2：运行全部测试**

```bash
cargo test
```

- [ ] **步骤3：运行 clippy**

```bash
cargo clippy -- -D warnings
```

- [ ] **步骤4：运行 simple_client 验证端到端**

```bash
cargo run --example simple_client
```

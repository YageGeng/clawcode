# MCP 集成实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 为 clawcode 实现 MCP 集成，支持 Stdio + StreamableHTTP 传输，工具以 `mcp__<server>__<tool>` 前缀注册到 ToolRegistry。

**参考：** Spec `docs/superpowers/specs/2026-05-14-mcp-integration.md`

**架构：** `crates/mcp/`（独立 crate）→ `tools::mcp::McpToolHandler` → `kernel::spawn_thread` 启动并注册

**技术栈：** `rmcp` v1.6.0（已有）、`tokio::process`、`reqwest`

---

## 文件结构

| 文件 | 动作 |
|------|------|
| `crates/mcp/Cargo.toml` | **Create** |
| `crates/mcp/src/lib.rs` | **Create** |
| `crates/mcp/src/types.rs` | **Create** |
| `crates/mcp/src/transport.rs` | **Create** |
| `Cargo.toml`（根） | Modify — workspace dep |
| `crates/config/src/mcp.rs` | **Create** |
| `crates/config/src/config.rs` | Modify — AppConfig 加字段 |
| `crates/config/src/lib.rs` | Modify — `pub mod mcp` |
| `crates/tools/Cargo.toml` | Modify — 加 mcp dep |
| `crates/tools/src/mcp.rs` | **Replace** — stub → 正式实现 |
| `crates/kernel/Cargo.toml` | Modify — 加 mcp dep |
| `crates/kernel/src/session.rs` | Modify — 启动 MCP + 注册工具 |

---

### Task 1: 创建 `crates/mcp/` crate

**文件：** Create 4 files + Modify 根 Cargo.toml

- [ ] **Step 1: `crates/mcp/Cargo.toml`**

```toml
[package]
name = "mcp"
edition.workspace = true
version.workspace = true
description = "MCP client — server connection management, tool discovery, and tool calls"

[dependencies]
tokio = { workspace = true, features = ["process", "time", "sync"] }
tokio-util = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
rmcp = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
reqwest = { workspace = true, features = ["json"] }
```

- [ ] **Step 2: `crates/mcp/src/types.rs`**

```rust
//! MCP types: config, tool metadata, errors, startup status.

use std::collections::HashMap;

/// Runtime-ready MCP server configuration.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransportConfig,
    pub enabled: bool,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    StreamableHttp {
        url: String,
        bearer_token_env: Option<String>,
        http_headers: HashMap<String, String>,
    },
}

/// Tool metadata from `tools/list`.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server_name: String,
    pub raw_name: String,
    pub callable_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Per-server startup outcome.
#[derive(Debug, Clone)]
pub enum McpStartupStatus {
    Ready,
    Failed { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server '{server}' startup failed: {reason}")]
    Startup { server: String, reason: String },

    #[error("server '{server}' tool call '{tool}' timed out ({timeout_secs}s)")]
    ToolTimeout { server: String, tool: String, timeout_secs: u64 },

    #[error("server '{server}' not found or not connected")]
    ServerNotFound { server: String },

    #[error("MCP protocol error on '{server}': {msg}")]
    Protocol { server: String, msg: String },

    #[error("transport error: {0}")]
    Transport(String),
}

/// Normalize a tool name for model visibility.
/// Replaces characters outside `[a-zA-Z0-9_-]` with `_`, truncates to 64 chars.
pub fn normalize_tool_name(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sanitized.len() > 64 { sanitized[..64].to_string() } else { sanitized }
}
```

- [ ] **Step 3: `crates/mcp/src/transport.rs`**

```rust
//! Transport layer: stdio process spawning and StreamableHTTP.

use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;
use crate::types::{McpError, McpTransportConfig};

pub(crate) struct StdioProcess {
    child: tokio::process::Child,
}

impl StdioProcess {
    pub(crate) fn spawn(
        command: &str, args: &[String], env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in env { cmd.env(k, v); }
        let child = cmd.spawn().map_err(|e| McpError::Startup {
            server: command.to_string(),
            reason: format!("spawn failed: {e}"),
        })?;
        Ok(Self { child })
    }

    pub(crate) fn into_io(self) -> (tokio::process::ChildStdin, tokio::process::ChildStdout) {
        (self.child.stdin.unwrap(), self.child.stdout.unwrap())
    }
}

impl Drop for StdioProcess {
    fn drop(&mut self) { self.child.start_kill().ok(); }
}

/// Build an rmcp Transport from config. Returns optional process handle (stdio only).
pub(crate) async fn build_transport(
    config: &McpTransportConfig,
) -> Result<(rmcp::transport::Transport, Option<StdioProcess>), McpError> {
    match config {
        McpTransportConfig::Stdio { command, args, env } => {
            let proc = StdioProcess::spawn(command, args, env)?;
            let (stdin, stdout) = proc.into_io();
            let t = rmcp::transport::child_process::TokioChildProcess::new(stdin, stdout)
                .map_err(|e| McpError::Transport(format!("stdio: {e}")))?;
            Ok((rmcp::transport::Transport::from(t), Some(proc)))
        }
        McpTransportConfig::StreamableHttp { url, bearer_token_env, http_headers } => {
            let mut headers = reqwest::header::HeaderMap::new();
            for (k, v) in http_headers {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(k.as_bytes())
                        .map_err(|e| McpError::Transport(format!("bad header '{k}': {e}")))?,
                    reqwest::header::HeaderValue::from_str(v)
                        .map_err(|e| McpError::Transport(format!("bad value '{v}': {e}")))?,
                );
            }
            if let Some(env_var) = bearer_token_env {
                if let Ok(token) = std::env::var(env_var) {
                    headers.insert(
                        reqwest::header::AUTHORIZATION,
                        reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                            .map_err(|e| McpError::Transport(format!("bad token: {e}")))?,
                    );
                }
            }
            let t = rmcp::transport::streamable_http::client::ReqwestClient::builder()
                .url(url.parse().map_err(|e| McpError::Transport(format!("bad URL: {e}")))?)
                .headers(headers)
                .build();
            Ok((rmcp::transport::Transport::from(t), None))
        }
    }
}
```

- [ ] **Step 4: `crates/mcp/src/lib.rs`**

```rust
//! MCP client — manages server connections, tool discovery, and tool calls.

mod transport;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub use types::{McpError, McpServerConfig, McpStartupStatus, McpToolInfo, normalize_tool_name};

/// Manages all MCP server connections for a session.
pub struct McpConnectionManager {
    clients: HashMap<String, ManagedClient>,
    startup_status: HashMap<String, McpStartupStatus>,
    cancel_token: CancellationToken,
}

struct ManagedClient {
    server_name: String,
    service: Option<Arc<tokio::sync::Mutex<rmcp::service::RunningService<rmcp::service::RoleClient, ()>>>>,
    tools: Vec<McpToolInfo>,
    tool_timeout_secs: u64,
    _process: Option<transport::StdioProcess>,
}

impl McpConnectionManager {
    /// Start all enabled MCP servers in parallel.
    pub async fn start_all(configs: &[McpServerConfig]) -> Self {
        let cancel_token = CancellationToken::new();
        let mut handles = Vec::new();

        for config in configs {
            if !config.enabled { continue; }
            let cfg = config.clone();
            let token = cancel_token.clone();
            handles.push(tokio::spawn(async move {
                (cfg.name.clone(), ManagedClient::connect(&cfg, token).await)
            }));
        }

        let mut clients = HashMap::new();
        let mut startup_status = HashMap::new();

        for handle in handles {
            let (name, result) = handle.await.unwrap_or_else(|e| (
                String::new(),
                Err(McpError::Startup { server: "unknown".into(), reason: format!("join: {e}") }),
            ));
            match result {
                Ok(client) => {
                    startup_status.insert(name.clone(), McpStartupStatus::Ready);
                    clients.insert(name, client);
                }
                Err(e) => {
                    startup_status.insert(name.clone(), McpStartupStatus::Failed { reason: e.to_string() });
                }
            }
        }

        Self { clients, startup_status, cancel_token }
    }

    /// Per-server startup outcomes.
    pub fn startup_status(&self) -> &HashMap<String, McpStartupStatus> {
        &self.startup_status
    }

    /// Collect tools from all connected servers with `mcp__<server>__<tool>` naming.
    pub fn list_all_tools(&self) -> Vec<McpToolInfo> {
        let mut tools = Vec::new();
        for client in self.clients.values() {
            for t in &client.tools {
                let mut prefixed = t.clone();
                prefixed.callable_name = format!(
                    "mcp__{}__{}",
                    normalize_tool_name(&client.server_name),
                    normalize_tool_name(&t.raw_name),
                );
                prefixed.server_name = client.server_name.clone();
                tools.push(prefixed);
            }
        }
        tools
    }

    /// Call a tool on a named server. Errors returned as `Err(String)`, never panics.
    pub async fn call_tool(
        &self, server: &str, tool_name: &str, arguments: serde_json::Value,
    ) -> Result<String, String> {
        let client = self.clients.get(server).ok_or_else(|| {
            match self.startup_status.get(server) {
                Some(McpStartupStatus::Failed { reason }) =>
                    format!("MCP server '{server}' failed to start: {reason}"),
                _ => format!("MCP server '{server}' not found"),
            }
        })?;
        client.call_tool(tool_name, arguments).await
    }

    /// Shut down all connections.
    pub async fn shutdown(&mut self) {
        self.cancel_token.cancel();
        self.clients.clear();
    }
}

impl ManagedClient {
    async fn connect(config: &McpServerConfig, _cancel: CancellationToken) -> Result<Self, McpError> {
        use rmcp::service::{serve_client, RoleClient};
        use rmcp::model::{ClientRequest, InitializeRequest, Implementation};
        use tokio::time::timeout;

        let (transport, process) = transport::build_transport(&config.transport).await?;

        let client_service = RoleClient::new();
        let server_info = Implementation { name: "clawcode".into(), version: "0.1.0".into() };

        let running = timeout(
            Duration::from_secs(config.startup_timeout_secs),
            serve_client(client_service, transport),
        )
        .await
        .map_err(|_| McpError::Startup {
            server: config.name.clone(),
            reason: format!("timed out after {}s", config.startup_timeout_secs),
        })?
        .map_err(|e| McpError::Startup {
            server: config.name.clone(),
            reason: format!("handshake: {e}"),
        })?;

        running.peer().send_request(ClientRequest::InitializeRequest(InitializeRequest {
            protocol_version: rmcp::model::ProtocolVersion::V_2024_11_05,
            capabilities: Default::default(),
            client_info: server_info,
        })).await.map_err(|e| McpError::Protocol {
            server: config.name.clone(), msg: format!("initialize: {e}"),
        })?;

        running.peer().send_request(ClientRequest::InitializedNotification(
            rmcp::model::InitializedNotification::default(),
        )).await.map_err(|e| McpError::Protocol {
            server: config.name.clone(), msg: format!("notify initialized: {e}"),
        })?;

        let tools_result = running.peer().list_tools(None).await.map_err(|e| {
            McpError::Protocol { server: config.name.clone(), msg: format!("list_tools: {e}") }
        })?;

        let tools: Vec<McpToolInfo> = tools_result.tools.into_iter().map(|t| McpToolInfo {
            server_name: config.name.clone(),
            raw_name: t.name.clone(),
            callable_name: String::new(),
            description: t.description.unwrap_or_default(),
            input_schema: t.input_schema,
        }).collect();

        Ok(Self {
            server_name: config.name.clone(),
            service: Some(Arc::new(tokio::sync::Mutex::new(running))),
            tools,
            tool_timeout_secs: config.tool_timeout_secs,
            _process: process,
        })
    }

    async fn call_tool(&self, tool_name: &str, args: serde_json::Value) -> Result<String, String> {
        use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest, ContentBlock};
        use tokio::time::timeout;

        let svc = self.service.as_ref().ok_or_else(||
            format!("MCP server '{}' is not connected", self.server_name)
        )?;

        let request = ClientRequest::CallToolRequest(CallToolRequest {
            params: CallToolRequestParams { name: tool_name.to_string(), arguments: Some(args) },
            meta: None,
        });

        let result = timeout(
            Duration::from_secs(self.tool_timeout_secs),
            async { svc.lock().await.peer().send_request(request).await },
        )
        .await
        .map_err(|_| format!(
            "tool '{tool_name}' on '{}' timed out after {}s",
            self.server_name, self.tool_timeout_secs,
        ))?
        .map_err(|e| format!("tool call failed on '{}': {e}", self.server_name))?;

        Ok(result.content.into_iter()
            .filter_map(|b| match b { ContentBlock::Text(t) => Some(t.text), _ => None })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}
```

- [ ] **Step 5: 根 Cargo.toml 注册 crate**

在 `[workspace.dependencies]` 添加：
```toml
mcp = { path = "crates/mcp" }
```

同时在 `[workspace.dependencies]` 确认 `tokio-util` 存在（若不存在则添加 `tokio-util = { version = "0.7", features = ["rt"] }`）。

- [ ] **Step 6: 编译验证**

```bash
cargo build -p mcp
```

- [ ] **Step 7: 提交**

```bash
git add crates/mcp/ Cargo.toml
git commit -m "feat(mcp): create MCP client crate with connection manager and dual transport"
```

---

### Task 2: MCP 配置类型

**文件：** Create 1 + Modify 2

- [ ] **Step 1: `crates/config/src/mcp.rs`**

```rust
//! MCP server configuration from claw.toml.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Flat TOML struct for `[[mcp_servers]]`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,

    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_sec: u64,

    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_sec: u64,

    // Stdio
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,

    // StreamableHTTP
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    #[serde(default)]
    pub http_headers: Option<HashMap<String, String>>,
}

fn default_true() -> bool { true }
fn default_startup_timeout() -> u64 { 30 }
fn default_tool_timeout() -> u64 { 120 }
```

- [ ] **Step 2: `AppConfig` 加字段 + `lib.rs` 导出**

`crates/config/src/config.rs`：
```rust
use crate::mcp::McpServerConfig;

// In struct AppConfig, after `skills`:
#[serde(default)]
pub mcp_servers: Vec<McpServerConfig>,

// In Default impl:
mcp_servers: Vec::new(),
```

`crates/config/src/lib.rs` 添加 `pub mod mcp;`

- [ ] **Step 3: 编译验证 + 提交**

```bash
cargo build -p config
git add crates/config/src/mcp.rs crates/config/src/config.rs crates/config/src/lib.rs
git commit -m "feat(config): add MCP server configuration type"
```

---

### Task 3: McpToolHandler（tools crate）

**文件：** Modify 2

- [ ] **Step 1: 加依赖**

`crates/tools/Cargo.toml` 添加 `mcp = { path = "../mcp" }`

- [ ] **Step 2: 替换 `crates/tools/src/mcp.rs`**

```rust
//! MCP tool handler — wraps an MCP tool as a clawcode [`Tool`].

use std::sync::Arc;
use async_trait::async_trait;
use mcp::{McpConnectionManager, types::McpToolInfo};
use crate::{Tool, ToolContext};

/// Adapts a single MCP tool to the [`Tool`] trait.
pub struct McpToolHandler {
    tool_info: McpToolInfo,
    manager: Arc<McpConnectionManager>,
}

impl McpToolHandler {
    pub fn new(tool_info: McpToolInfo, manager: Arc<McpConnectionManager>) -> Self {
        Self { tool_info, manager }
    }
}

#[async_trait]
impl Tool for McpToolHandler {
    fn name(&self) -> &str { &self.tool_info.callable_name }
    fn description(&self) -> &str { &self.tool_info.description }
    fn parameters(&self) -> serde_json::Value { self.tool_info.input_schema.clone() }

    fn needs_approval(&self, _arguments: &serde_json::Value, _ctx: &ToolContext) -> bool {
        true
    }

    async fn execute(
        &self, arguments: serde_json::Value, _ctx: &ToolContext,
    ) -> Result<String, String> {
        self.manager
            .call_tool(&self.tool_info.server_name, &self.tool_info.raw_name, arguments)
            .await
    }
}

/// Register all tools from a [`McpConnectionManager`] into a [`ToolRegistry`].
pub fn register_mcp_tools(
    registry: &crate::ToolRegistry,
    manager: Arc<McpConnectionManager>,
) {
    for tool_info in manager.list_all_tools() {
        registry.register(Arc::new(McpToolHandler::new(tool_info, Arc::clone(&manager))));
    }
}
```

- [ ] **Step 3: 编译验证 + 提交**

```bash
cargo build -p tools
git add crates/tools/Cargo.toml crates/tools/src/mcp.rs
git commit -m "refactor(tools): replace MCP stub with McpToolHandler"
```

---

### Task 4: Session 集成

**文件：** Modify 2

- [ ] **Step 1: kernel 加依赖**

`crates/kernel/Cargo.toml` 添加 `mcp = { path = "../mcp" }`

- [ ] **Step 2: `Session` 加字段**

`crates/kernel/src/session.rs`，在 `skill_registry` 后：
```rust
#[builder(default)]
pub mcp_manager: Option<Arc<mcp::McpConnectionManager>>,
```

- [ ] **Step 3: `spawn_thread` 启动 MCP 并注册**

在 `tools.register_skill_tools(...)` 之后：
```rust
// Start MCP servers and register their tools.
let mcp_manager = {
    let configs: Vec<mcp::McpServerConfig> = app_config
        .mcp_servers
        .iter()
        .filter(|c| c.enabled)
        .filter_map(|c| {
            let transport = if let Some(cmd) = &c.command {
                mcp::types::McpTransportConfig::Stdio {
                    command: cmd.clone(),
                    args: c.args.clone().unwrap_or_default(),
                    env: c.env.clone().unwrap_or_default(),
                }
            } else if let Some(url) = &c.url {
                mcp::types::McpTransportConfig::StreamableHttp {
                    url: url.clone(),
                    bearer_token_env: c.bearer_token_env.clone(),
                    http_headers: c.http_headers.clone().unwrap_or_default(),
                }
            } else {
                return None;
            };
            Some(mcp::McpServerConfig {
                name: c.name.clone(),
                transport,
                enabled: c.enabled,
                startup_timeout_secs: c.startup_timeout_sec,
                tool_timeout_secs: c.tool_timeout_sec,
            })
        })
        .collect();

    let manager = mcp::McpConnectionManager::start_all(&configs).await;
    let manager = Arc::new(manager);

    for (name, status) in manager.startup_status() {
        match status {
            mcp::McpStartupStatus::Ready => tracing::info!(server = %name, "MCP connected"),
            mcp::McpStartupStatus::Failed { reason } => tracing::warn!(server = %name, %reason, "MCP failed"),
        }
    }

    tools::mcp::register_mcp_tools(&tools, Arc::clone(&manager));

    manager
};
```

- [ ] **Step 4: Session builder 加字段**

在 `Session::builder()` 调用处添加 `.mcp_manager(Some(mcp_manager))`

- [ ] **Step 5: 编译验证 + 提交**

```bash
cargo build && cargo test
git add crates/kernel/Cargo.toml crates/kernel/src/session.rs
git commit -m "feat(kernel): wire MCP connection manager into session"
```

---

### Task 5: `tokio-util` workspace 依赖（如缺失）

如果 `Cargo.toml`（根）的 `[workspace.dependencies]` 中尚无 `tokio-util`：

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

- [ ] **Step 1: 编译验证**

```bash
cargo build
```

- [ ] **Step 2: 提交**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(deps): add tokio-util workspace dependency"
```

---

### Task 6: 端到端验证

- [ ] **Step 1: 全量编译**

```bash
cargo build
```

- [ ] **Step 2: 全量测试**

```bash
cargo test
```

期望：所有已有测试通过，无新增 warning。

- [ ] **Step 3: 验证 `mcp` crate 可被外部引用**

```bash
cargo test -p mcp
```

- [ ] **Step 4: 提交**

```bash
git add Cargo.lock
git commit -m "chore: update Cargo.lock after MCP integration"
```

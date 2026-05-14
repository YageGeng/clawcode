# MCP 集成设计规格

**日期**: 2026-05-14
**状态**: 待审核
**参考**: Codex `codex-rmcp-client` / `codex-mcp`、OpenCode `packages/opencode/src/mcp/`

---

## 1. 目标

为 clawcode 实现 MCP（Model Context Protocol）集成，使 LLM 能够发现并调用外部 MCP 服务器的工具。支持两种传输方式：Stdio（子进程）和 StreamableHTTP（远程）。

---

## 2. 架构总览

```
crates/mcp/                    独立 MCP client crate（不依赖 tools/kernel）
  ├── lib.rs                   McpConnectionManager + ManagedClient
  ├── types.rs                  McpServerConfig、McpToolInfo、McpError、normalize_tool_name
  ├── transport.rs              Stdio 进程管理、StreamableHTTP 传输构建
  └── oauth.rs                 OAuth 2.0 授权流程

crates/config/src/mcp.rs       用户配置类型（claw.toml deserialize）

crates/tools/src/mcp.rs         McpToolHandler（Tool trait 适配）+ register_mcp_tools

crates/kernel/src/session.rs   Session 持有 McpConnectionManager，spawn_thread 中启动
```

**依赖方向**：`config` ← `mcp` → `tools` → `kernel`

**与 Codex 的对应**：

| Codex | clawcode |
|-------|----------|
| `codex-rmcp-client` + `codex-mcp` | `crates/mcp/`（合并为单 crate） |
| `McpConnectionManager` | `mcp::McpConnectionManager` |
| `AsyncManagedClient` / `ManagedClient` | `mcp::ManagedClient` |
| `McpHandler` | `tools::mcp::McpToolHandler` |
| `normalize_tools_for_model` | `mcp::types::normalize_tool_name` |
| `McpOAuthProvider` | `mcp::oauth::OAuthProvider` |

---

## 3. 配置格式

### 3.1 claw.toml

```toml
# Stdio 传输
[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@anthropic-ai/mcp-server-filesystem", "/tmp"]
enabled = true
startup_timeout_sec = 30
tool_timeout_sec = 120

# StreamableHTTP 传输
[[mcp_servers]]
name = "github"
url = "https://mcp.github.com/stream"
bearer_token_env = "GITHUB_TOKEN"
enabled = true

# 带 OAuth 的远程服务器
[[mcp_servers]]
name = "enterprise-api"
url = "https://api.example.com/mcp"
enabled = true
oauth.client_id = "clawcode"
oauth.scopes = ["read", "write"]
```

### 3.2 配置类型

`config::mcp::McpServerConfig` — 用户配置的 flat TOML 结构：

```rust
pub struct McpServerConfig {
    pub name: String,                           // 唯一标识
    pub enabled: bool,                          // 默认 true
    pub startup_timeout_sec: u64,               // 默认 30
    pub tool_timeout_sec: u64,                  // 默认 120

    // Stdio
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,

    // StreamableHTTP
    pub url: Option<String>,
    pub bearer_token_env: Option<String>,
    pub http_headers: Option<HashMap<String, String>>,

    // OAuth
    pub oauth: Option<McpOAuthConfig>,
}
```

`mcp::types::McpServerConfig` — 运行时配置（扁平化，不含 `Option` 歧义）：

```rust
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransportConfig,  // 明确的 enum，无 Option
    pub enabled: bool,
    pub startup_timeout_secs: u64,
    pub tool_timeout_secs: u64,
    pub oauth: Option<McpOAuthParams>,
}

pub enum McpTransportConfig {
    Stdio { command: String, args: Vec<String>, env: HashMap<String, String> },
    StreamableHttp { url: String, bearer_token_env: Option<String>, http_headers: HashMap<String, String> },
}
```

两种配置之间的转换在 `kernel::spawn_thread` 中完成。

---

## 4. 核心类型

### 4.1 McpConnectionManager

管理一个 session 内所有 MCP 服务器连接的生命周期。

```rust
pub struct McpConnectionManager {
    clients: HashMap<String, ManagedClient>,
    startup_status: HashMap<String, McpStartupStatus>,
    cancel_token: CancellationToken,
}
```

**行为**：
- `start_all(configs) -> Self` — 并行启动所有 enabled 服务器，失败不阻塞
- `list_all_tools() -> Vec<McpToolInfo>` — 从所有已连接服务器收集工具，名称加 `mcp__<server>__` 前缀
- `call_tool(server, tool_name, args) -> Result<String, String>` — 调用指定工具，错误以 `Err(String)` 返回，不阻断 turn
- `startup_status() -> &HashMap<String, McpStartupStatus>` — 查询各服务器启动结果
- `shutdown()` — 取消所有进行中操作，清理子进程

### 4.2 ManagedClient

单个 MCP 连接，持有 `RunningService` 引用和缓存工具列表。

```rust
struct ManagedClient {
    server_name: String,
    service: Option<Arc<tokio::sync::Mutex<RunningService<RoleClient>>>>,
    tools: Vec<McpToolInfo>,
    tool_timeout_secs: u64,
    _process: Option<StdioProcess>,  // 保持进程存活
}
```

**连接流程**：
1. `build_transport(config)` → rmcp `Transport`
2. `serve_client(RoleClient, transport)` — MCP 握手（带 startup_timeout）
3. `InitializeRequest` / `InitializedNotification`
4. `list_tools(None)` → 缓存工具列表

**工具调用**：
1. 从 `service` 获取 `RunningService` 引用
2. 构造 `CallToolRequest`
3. `peer().send_request(request)` — 带 tool_timeout
4. 提取 `ContentBlock::Text` 内容返回

### 4.3 McpToolInfo

```rust
pub struct McpToolInfo {
    pub server_name: String,          // "filesystem"
    pub raw_name: String,             // "read_file"（MCP 原始名称）
    pub callable_name: String,        // "mcp__filesystem__read_file"（LLM 可见）
    pub description: String,
    pub input_schema: serde_json::Value,  // JSON Schema
}
```

### 4.4 工具命名标准化

`normalize_tool_name(raw: &str) -> String`：
- 将 `[^a-zA-Z0-9_-]` 替换为 `_`
- 截断到 64 字符
- 最终格式：`mcp__<normalized_server>__<normalized_tool>`

### 4.5 McpToolHandler

位于 `crates/tools/src/mcp.rs`，实现 `Tool` trait：

```rust
pub struct McpToolHandler {
    tool_info: McpToolInfo,
    manager: Arc<McpConnectionManager>,
}
```

- `name()` → `callable_name`
- `description()` → `description`
- `parameters()` → `input_schema`
- `needs_approval()` → `true`（外部工具默认需审批）
- `execute(args)` → `manager.call_tool(server, raw_name, args)`

### 4.6 McpStartupStatus

```rust
pub enum McpStartupStatus {
    Ready,
    Failed { reason: String },
}
```

### 4.7 错误类型

```rust
pub enum McpError {
    Startup { server: String, reason: String },
    ToolTimeout { server: String, tool: String, timeout_secs: u64 },
    ServerNotFound { server: String },
    Protocol { server: String, msg: String },
    Transport(String),
}
```

---

## 5. 传输层

### 5.1 Stdio

`transport::StdioProcess` 封装 `tokio::process::Child`：
- `spawn(command, args, env)` — 启动子进程，stdin/stdout/stderr 管道
- `kill_on_drop(true)` — Drop 时自动终止
- `into_io()` — 取出 stdin/stdout 用于 rmcp 的 `TokioChildProcess` transport

### 5.2 StreamableHTTP

使用 rmcp 的 `ReqwestClient`：
- 支持自定义 HTTP headers
- 支持 Bearer token（从环境变量读取）
- 支持 OAuth（见 §6）

### 5.3 transport::build_transport

```rust
async fn build_transport(config: &McpTransportConfig)
    -> Result<(Transport, Option<StdioProcess>), McpError>
```

根据 config 分发到 Stdio 或 StreamableHTTP，返回 rmcp `Transport` 和可选的进程句柄。

---

## 6. OAuth 2.0 授权

参考 Codex `codex-rmcp-client/src/oauth.rs` 和 OpenCode `mcp/oauth-provider.ts`。

### 6.1 配置

```toml
[[mcp_servers]]
name = "enterprise-api"
url = "https://api.example.com/mcp"
oauth.client_id = "clawcode"
oauth.client_secret = "${ENV:ENTERPRISE_CLIENT_SECRET}"
oauth.scopes = ["read", "write"]
oauth.redirect_uri = "http://localhost:19876/callback"
oauth.token_store = "keyring"   # "keyring" | "file"
```

### 6.2 类型

```rust
pub struct McpOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub redirect_uri: Option<String>,
    pub token_store: OAuthTokenStore,
}

pub enum OAuthTokenStore {
    Keyring,                        // 系统钥匙链（安全）
    File(PathBuf),                  // 文件存储
}

pub struct McpOAuthParams {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}
```

### 6.3 授权流程

```
1. 用户执行 codex mcp login <server-name>
         │
2. McpOAuthProvider.authorize()
    ├─ 生成 state（防 CSRF）、code_verifier（PKCE）
    ├─ 启动本地 HTTP 回调服务器（127.0.0.1:19876）
    ├─ 打开浏览器到 authorization_url
    │
3. 用户在浏览器完成授权
         │
4. 回调服务器接收 authorization_code
    ├─ 校验 state
    ├─ 用 code + code_verifier 交换 token
    ├─ 存储 token（keyring 或文件）
    └─ 关闭回调服务器
         │
5. McpConnectionManager 读 token → 创建认证的 StreamableHTTP 连接
```

### 6.4 Token 存储

- **Keyring（推荐）**: 使用系统钥匙链（Linux: `secret-service`、macOS: Keychain）
- **File**: `<data-dir>/mcp-auth/<server-name>.json`，需确保文件权限为 0600

### 6.5 Token 刷新

每次工具调用前检查 `expires_at`：
- 未过期 → 直接使用
- 已过期 + 有 refresh_token → 自动刷新
- 已过期 + 无 refresh_token → 返回 `needs_auth` 状态，提示用户重新登录

### 6.6 McpOAuthProvider

实现 rmcp 的 OAuth 接口：

```rust
impl rmcp::transport::auth::OAuthClientProvider for McpOAuthProvider {
    async fn access_token(&self) -> Result<String>;
    async fn refresh_token(&self) -> Result<String>;
    async fn authorization_url(&self) -> Result<Url>;
    async fn finish_auth(&self, code: &str) -> Result<()>;
}
```

### 6.7 安全约束

- Token 不出现在日志中
- `redirect_uri` 严格限定为 `127.0.0.1`（禁止远程回调）
- State 参数使用 `rand` 生成，60 秒过期
- 文件存储的 token 文件权限必须为 0600，否则报错

---

## 7. 会话集成

### 7.1 Session 结构

```rust
pub(crate) struct Session {
    // ...现有字段...
    pub mcp_manager: Option<Arc<mcp::McpConnectionManager>>,
}
```

### 7.2 spawn_thread 流程

```
spawn_thread()
  │
  ├─ skill_registry = SkillRegistry::discover()
  ├─ tools.register_skill_tools(skill_registry)
  │
  ├─ configs: Vec<McpServerConfig> = app_config.mcp_servers → 运行时配置
  ├─ mcp_manager = McpConnectionManager::start_all(&configs).await
  │     ├── 并行启动所有 enabled 服务器
  │     └── 记录每个服务器的 startup_status
  │
  ├─ tools::mcp::register_mcp_tools(&tools, mcp_manager)
  │     └── McpToolHandler 注册到 ToolRegistry（名称 = mcp__<server>__<tool>）
  │
  └─ Session { ..., mcp_manager: Some(mcp_manager) }
```

### 7.3 工具注册

```rust
pub fn register_mcp_tools(
    registry: &ToolRegistry,
    manager: Arc<McpConnectionManager>,
) {
    for tool_info in manager.list_all_tools() {
        let handler = McpToolHandler::new(tool_info, Arc::clone(&manager));
        registry.register(Arc::new(handler));
    }
}
```

### 7.4 命名与冲突

| 前缀 | 来源 |
|------|------|
| `shell` | builtin |
| `read` / `write` / `edit` | builtin FS |
| `skill` | skill 系统 |
| `mcp__<server>__` | MCP（双下划线分隔，避免冲突） |

内置工具名称与 MCP 工具名称理论上不会冲突（`mcp__` 前缀不会出现在内置工具中）。如果未来出现冲突，`McpToolHandler` 注册时同名工具会覆盖旧工具（`HashMap::insert` 语义），MCP 工具后注册优先级最高。

---

## 8. 错误处理与超时

### 8.1 超时

| 阶段 | 默认值 | 可配置 |
|------|--------|--------|
| MCP 握手 | 30s | `startup_timeout_sec` |
| 单个工具调用 | 120s | `tool_timeout_sec` |
| OAuth 回调等待 | 300s | 不可配（用户操作时间） |

### 8.2 错误传播

- **启动失败** → `McpStartupStatus::Failed`，记录日志，不影响其他服务器
- **工具调用超时** → `Err("tool 'X' on 'Y' timed out after 120s")` 返回给 LLM
- **工具调用失败** → `Err("tool call failed on 'Y': <protocol error>")` 返回给 LLM
- **服务器已断开** → `Err("MCP server 'Y' is not connected")` 返回给 LLM

所有错误以 `Result<String, String>` 中的 `Err` 返回给 LLM，LLM 可以阅读错误信息并采取替代方案。Turn 不因此中断。

### 8.3 关闭

- `McpConnectionManager::shutdown()` — cancel_token + 清空 clients
- `StdioProcess::Drop` — `start_kill()` 终止子进程
- `CancellationToken` 传播给所有 `tokio::spawn` 任务

---

## 9. 工具刷新（P2）

当前 P1 实现为一次性工具发现（session 启动时）。P2 计划通过 `/mcp-refresh` 命令支持：

1. 接收 MCP 服务器的 `ToolListChangedNotification`
2. 重新调用 `list_tools` 并更新 `ManagedClient.tools`
3. 重建 `ToolRegistry` 中的 MCP 工具（需 ToolRegistry 支持 unregister）

---

## 10. 不变式与错误语义

- **启动不阻塞**：单个 MCP 服务器启动失败不影响其他服务器或 session 创建
- **调用不阻断**：MCP 工具调用失败返回 `Err(String)`，由 LLM 决定如何处理
- **进程不泄露**：`StdioProcess` 在 Drop 时自动 `kill_on_drop`，`shutdown()` 主动终止
- **命名不冲突**：`mcp__` 前缀确保 MCP 工具名与内置工具不冲突
- **配置不崩溃**：`claw.toml` MCP 配置缺失或格式错误 → 使用默认值（空列表）
- **OAuth token 安全**：不出现在日志中，文件存储权限 0600

---

## 11. 依赖关系

```
mcp ────────→ rmcp, tokio, reqwest, serde_json
config ─────→ serde
tools ──────→ mcp, protocol
kernel ─────→ tools, mcp, config, protocol, skills
```

---

## 12. 与 Codex 的关键差异

| 维度 | Codex | clawcode |
|------|-------|----------|
| Crate 拆分 | 3 层（rmcp-client / codex-mcp / core） | 1 层（`crates/mcp/`） |
| 工具注册 | ToolRouter → McpHandler（多层路由） | ToolRegistry → McpToolHandler（直接注册） |
| 审批 | Guardian / elicitation 全链路 | `needs_approval() = true`，走现有审批机制 |
| 沙箱状态 meta | `SandboxState` 注入工具调用 | P1 不做，无沙箱需求 |
| 工具延期 | Direct / Deferred 分级暴露 | P1 全部 Direct |
| Codex Apps 工具 | connector_id / namespace 特殊处理 | 不做 |

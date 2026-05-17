# ACP 外部 MCP 注入设计规格

**日期**: 2026-05-17
**状态**: 待审核
**参考**: `agent-client-protocol-schema 0.12.0`、`zed-industries/codex-acp@156cb0d`

---

## 1. 目标

让 ACP 客户端可以在创建或加载会话时注入外部 MCP server，并让这些 MCP 工具进入该会话的 tool registry，后续 prompt 可以直接调用。

本设计只覆盖 ACP 侧的外部 MCP 注入路径，不改变 `claw.toml` 中静态 MCP server 的配置语义。

---

## 2. ACP 协议结论

ACP 没有单独的 `mcp/register` 或 `session/registerMcpServer` 方法。外部 MCP 注入是 session request 的一部分：

- `session/new`: `NewSessionRequest { cwd, mcp_servers, ... }`
- `session/load`: `LoadSessionRequest { session_id, cwd, mcp_servers, ... }`
- unstable `session/fork` 和 `session/resume` 也有 `mcp_servers`，但当前 clawcode ACP agent 尚未实现这些方法。

`mcp_servers` 的元素类型是 ACP schema 的 `McpServer`：

| ACP variant | 可用条件 | clawcode 当前运行时映射 |
| --- | --- | --- |
| `Stdio` | 所有 Agent 必须支持 | `protocol::mcp::McpTransportConfig::Stdio` |
| `Http` | `InitializeResponse.agent_capabilities.mcp_capabilities.http = true` | `protocol::mcp::McpTransportConfig::StreamableHttp` |
| `Sse` | `InitializeResponse.agent_capabilities.mcp_capabilities.sse = true` | 暂不支持，应拒绝或不声明 capability |

当前 `crates/acp/src/agent.rs::handle_initialize` 已声明：

```rust
McpCapabilities::new().http(true)
```

这表示客户端可以发送 Stdio 和 HTTP MCP server，但不应该发送 SSE。

---

## 3. codex-acp 对照

`zed-industries/codex-acp@156cb0d` 的关键实现位于 `/tmp/codex-acp/src/codex_agent.rs`：

- `initialize()` 声明 `McpCapabilities::new().http(true)`，与 clawcode 当前声明一致。
- `build_session_config(cwd, mcp_servers)` 会复制基础 config，然后把 ACP 传入的 MCP server 合并到本次 session config。
- `new_session()` 从 `NewSessionRequest` 解构 `cwd, mcp_servers`，调用 `build_session_config()` 后再启动 thread。
- `load_session()` 从 `LoadSessionRequest` 解构 `cwd, mcp_servers`，调用 `build_session_config()` 后再 resume thread。
- `Sse` 在 codex-acp 中直接跳过，因为 codex-rs 不支持该 transport。
- Stdio server 的 command/args/env 被转换成 Codex MCP stdio config；HTTP server 的 url/headers 被转换成 StreamableHTTP config。
- Codex 会把 server name 中的空白替换成 `_`，因为其 MCP server namespace 不允许空白。

可借鉴点：外部 MCP 不作为全局配置持久化，而是并入“本次会话启动配置”。这样新建和加载会话在首个 prompt 前就拥有这些工具。

需要区别的点：clawcode 已经有 `McpConnectionManager::register_external_mcp_server()`，因此除“会话启动前合并”外，还可以支持已活跃 session 的动态补注册。

---

## 4. 当前 clawcode 状态

已有能力：

- `protocol::acp_conv` 已有 `TryFrom<acp::schema::McpServer> for protocol::mcp::McpServerConfig`。
- ACP 转换会把外部配置标记为 `enabled = true` 和 `external = true`。
- `protocol::mcp::McpServerConfig` 已包含 `external: bool`，默认 `false`。
- `mcp::McpConnectionManager` 已有 `register_external_mcp_server(config)`，可以运行时连接并记录状态。
- `tools::ToolRegistry::register()` 按工具名覆盖插入，因此重复调用 `register_mcp_tools(manager)` 不会产生重复条目。

缺口：

- `handle_new_session()` 目前解构 `let NewSessionRequest { cwd, .. } = request;`，丢弃了 `mcp_servers`。
- `handle_load_session()` 目前只读取 `session_id`，丢弃了 `request.mcp_servers` 和 `request.cwd`。
- `protocol::AgentKernel` 的 `new_session/load_session` 接口无法传入 ACP 外部 MCP config。
- `session::Thread` 当前没有暴露 `mcp_manager` 和 `tools`，因此 kernel 无法对已活跃 session 追加 MCP 并刷新 tool registry。

---

## 5. 推荐设计

推荐采用“会话创建后、后台注册”的方式。

codex-acp 的实现是启动前合并 config；clawcode 已经把 MCP 生命周期封装在 `McpConnectionManager` 中，并且已有 `register_external_mcp_server()`。因此在 clawcode 中更合适的落点是：先正常创建或恢复 `Thread`，再把 ACP 外部 MCP 注册任务提交到后台。`session/new` 和 `session/load` 不等待 MCP 进程启动、HTTP 鉴权或握手完成。

### 5.1 ACP 层

在 `crates/acp/src/agent.rs` 中：

1. `handle_new_session()` 解构 `cwd, mcp_servers`。
2. 继续保留当前相对 `cwd` 解析逻辑。
3. 使用 `protocol::acp_conv` 将 ACP `mcp_servers` 转成 `Vec<protocol::mcp::McpServerConfig>`。
4. 调用扩展后的 kernel API，把外部 MCP configs 传下去。
5. 转换失败时返回 ACP `internal_error`，错误信息应包含 server name 和不支持原因。

`handle_load_session()` 同样解构 `session_id, cwd, mcp_servers`。`cwd` 不覆盖持久化 session 的 cwd，但 ACP 注入的 stdio MCP server 应使用该 cwd 作为进程工作目录。

### 5.2 Protocol / Kernel trait

在 `crates/protocol/src/kernel.rs` 新增 `SessionLaunchOptions`，并扩展 trait：

```rust
/// Options supplied by a frontend when creating or loading a session.
#[derive(Debug, Clone, Default)]
pub struct SessionLaunchOptions {
    /// MCP servers injected by the frontend for this session only.
    pub external_mcp_servers: Vec<crate::mcp::McpServerConfig>,
}

async fn new_session(
    &self,
    cwd: PathBuf,
    options: SessionLaunchOptions,
) -> Result<SessionCreated, KernelError>;

async fn load_session(
    &self,
    session_id: &SessionId,
    cwd: PathBuf,
    options: SessionLaunchOptions,
) -> Result<SessionCreated, KernelError>;
```

`SessionLaunchOptions` 作为 frontend-to-kernel 的会话启动参数容器。第一版只放 `external_mcp_servers`，后续如果 ACP 的 additional directories、环境覆盖或其他 session-scoped 能力接入，可以继续扩展该 struct，而不需要反复改 trait 签名。

### 5.3 Kernel session 启动路径

在 `Kernel::new_session()` 和非活跃 `Kernel::load_session()` 中：

1. 按现有逻辑创建或恢复 `Thread`。
2. 对该 `Thread` 调用外部 MCP 注册 helper。
3. helper 为每个外部 MCP server 提交后台 task，task 注册成功后刷新 tool registry。
4. 如果外部 MCP 注册失败，记录失败状态并打日志；不要阻塞或回滚 session 创建。

静态 MCP 仍然沿用当前 `spawn_thread()` 的后台连接流程；ACP 外部 MCP 也采用后台连接，避免慢 MCP server 阻塞 `session/new` 或 `session/load`。

### 5.4 活跃 session 的 load 场景

当前 `Kernel::load_session()` 如果 session 已在内存中，会直接返回 session state。为了让新传入的 ACP `mcp_servers` 对活跃 session 生效，需要给 `Thread` 增加：

- `tools: Arc<ToolRegistry>`
- `mcp_manager: Arc<mcp::McpConnectionManager>`

然后实现一个 kernel 内部方法：

```rust
async fn register_external_mcp_servers_for_thread(
    &self,
    thread: &Thread,
    external_mcp_servers: Vec<mcp::McpServerConfig>,
) -> Result<(), KernelError>
```

逻辑：

1. 为每个 config `tokio::spawn` 一个注册任务。
2. task 内调用 `thread.mcp_manager.register_external_mcp_server(config).await`。
3. 注册成功后调用 `thread.tools.register_mcp_tools(Arc::clone(&thread.mcp_manager))` 刷新 registry。
4. 注册失败只记录 warning；失败状态由 `McpConnectionManager` 保存。

`ToolRegistry::register()` 是覆盖插入，因此刷新整个 manager 的工具列表是幂等的。

### 5.5 错误策略

推荐同步校验、异步连接：

- ACP 到 runtime config 转换失败：整个 `session/new` 或 `session/load` 失败。
- 声明之外的 `Sse`：转换失败，因为 clawcode 没有声明 `sse = true`。
- MCP 连接失败：不让 `session/new` 或 `session/load` 失败；后台记录失败状态并输出日志。

原因：schema/transport 转换错误是请求格式问题，应同步返回；MCP 连接本身可能慢或依赖外部服务，不应该阻塞 session 创建。

---

## 6. 实现步骤

1. 在 `protocol::AgentKernel` 所在模块新增 `SessionLaunchOptions`，第一版包含 `external_mcp_servers`。
2. 更新 `Kernel` trait 实现和所有调用点。
3. 给 `Thread` 增加 `tools` 和 `mcp_manager` 字段，用于 session-scoped MCP 动态注册。
4. 在 kernel 中实现外部 MCP 注册 helper，通过 `tokio::spawn` 后台注册并刷新 tool registry。
5. 为 ACP 注入的 stdio MCP server 设置 session cwd，确保子进程在 ACP 请求 cwd 下启动。
6. 更新 ACP `handle_new_session()` 和 `handle_load_session()`，读取并转换 `mcp_servers`。
7. 保持 `initialize()` 只声明 `http(true)`，不要声明 `sse(true)`。

---

## 7. 测试计划

### 7.1 Protocol conversion

已有转换测试应覆盖：

- ACP Stdio -> runtime Stdio，`external = true`
- ACP HTTP -> runtime StreamableHTTP，headers 正确转换，`external = true`
- ACP SSE -> error

### 7.2 ACP handler

新增 fake kernel 测试：

- `handle_new_session()` 会把 ACP `mcp_servers` 转成 runtime config 并传给 kernel。
- `handle_load_session()` 会把 ACP `mcp_servers` 转成 runtime config 并传给 kernel。
- ACP 注入的 Stdio MCP config 会带上 session cwd。
- 转换失败时 handler 返回 ACP error。

### 7.3 Kernel/session

新增 kernel 或 session 测试：

- 新 session 不等待外部 MCP 启动完成即可返回。
- load 活跃 session 不等待外部 MCP 启动完成即可返回。
- 外部 MCP 后台注册成功后刷新 tool registry。

### 7.4 MCP manager

已有 `register_external_mcp_server` 测试应保持在 `#[cfg(test)] mod tests` 或集成测试中，不在生产 impl 上暴露 test-only helper。

---

## 8. 非目标

- 不实现 ACP `Sse` transport。
- 不把 ACP 注入的外部 MCP server 写回 `claw.toml`。
- 不实现 `session/fork` / `session/resume` 的 MCP 注入，除非后续同时实现这些 ACP 方法。
- 不改变静态 MCP server 的加载、OAuth、工具命名策略。

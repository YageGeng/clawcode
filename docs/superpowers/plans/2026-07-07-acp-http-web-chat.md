# ACP HTTP Web Chat 实施计划

> **给 agentic workers：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans` 按任务执行。本计划用 checkbox（`- [ ]`）追踪步骤。

**目标:** 在 `crates/acp` 中新增 HTTP/Web 模式，用官方 ACP HTTP/WebSocket transport 暴露现有 agent，并内置支持聊天、历史、恢复、权限请求、取消和模型切换的网页。

**架构:** 后端复用 `ClawcodeAgent`，为其实现 `ConnectTo<Client>`，stdio 和 HTTP 共用同一套 ACP handler。HTTP 模式用 Axum 合并 `/acp` ACP router 和内置静态网页，网页作为最小 ACP client 直接通过 WebSocket JSON-RPC 调用 ACP 方法。

**技术栈:** Rust 2024、agent-client-protocol、agent-client-protocol-http、axum、tokio、clap、原生 HTML/CSS/JavaScript、现有 kernel/provider/tools/config。

---

## 文件结构

新增：

- `crates/acp/src/http.rs`：HTTP server options、Axum router、静态资源 handler、启动函数。
- `crates/acp/web/index.html`：网页根页面。
- `crates/acp/web/app.js`：浏览器 ACP client 和聊天 UI 状态。
- `crates/acp/web/styles.css`：网页样式。

修改：

- `Cargo.toml`：新增 workspace 依赖 `agent-client-protocol-http`、`axum`。
- `crates/acp/Cargo.toml`：启用 HTTP server 依赖和 tokio net feature。
- `crates/acp/src/lib.rs`：导出 `http` 模块。
- `crates/acp/src/agent.rs`：为 `ClawcodeAgent` 实现 `ConnectTo<Client>`，让 stdio 和 HTTP 共用 handler。
- `crates/acp/src/main.rs`：新增 `--http --host --port` CLI。
- `.gitignore`：忽略 `.superpowers/` 本地可视化伴侣目录。

验证：

- `rtk cargo fmt --check`
- `rtk cargo test -p acp`
- `rtk cargo check --workspace`

---

### Task 1: 后端依赖和 HTTP options 测试

**Files:**
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/Cargo.toml`
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/Cargo.toml`
- Create: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/http.rs`
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/lib.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/acp/src/http.rs` 先加入测试，断言默认绑定地址、ACP path 和静态资源关键内容：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the HTTP server defaults stay local-only.
    #[test]
    fn http_options_default_to_localhost_and_acp_path() {
        let options = HttpServerOptions::default();

        assert_eq!(options.bind.ip().to_string(), "127.0.0.1");
        assert_eq!(options.bind.port(), 0);
        assert_eq!(options.acp_path, "/acp");
    }

    /// Verifies embedded web assets contain the required ACP entry points.
    #[test]
    fn embedded_web_assets_reference_required_acp_methods() {
        assert!(INDEX_HTML.contains("app.js"));
        assert!(APP_JS.contains("session/list"));
        assert!(APP_JS.contains("session/load"));
        assert!(APP_JS.contains("session/set_config_option"));
        assert!(APP_JS.contains("session/request_permission"));
    }
}
```

- [ ] **Step 2: 运行测试并确认失败**

Run:

```bash
rtk cargo test -p acp http_options_default_to_localhost_and_acp_path embedded_web_assets_reference_required_acp_methods
```

Expected: FAIL，因为 `http.rs`、`HttpServerOptions` 或静态资源尚未实现。

- [ ] **Step 3: 增加最小依赖和模块**

新增 workspace dependencies：

```toml
agent-client-protocol-http = { version = "1.2", default-features = false }
axum = "0.8"
```

`crates/acp/Cargo.toml`：

```toml
agent-client-protocol-http = { workspace = true, features = ["server"] }
axum = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std", "net"] }
```

`crates/acp/src/lib.rs`：

```rust
pub mod http;
```

- [ ] **Step 4: 实现最小 `HttpServerOptions` 和资源常量**

`HttpServerOptions` 使用 2 个字段，不触发 typed-builder 规则：

```rust
/// Options used when serving the ACP agent over HTTP.
#[derive(Debug, Clone)]
pub struct HttpServerOptions {
    /// Socket address used by the HTTP listener.
    pub bind: SocketAddr,
    /// ACP endpoint path mounted into the Axum router.
    pub acp_path: String,
}
```

- [ ] **Step 5: 确认测试通过**

Run:

```bash
rtk cargo test -p acp http_options_default_to_localhost_and_acp_path embedded_web_assets_reference_required_acp_methods
```

Expected: PASS。

---

### Task 2: 让 `ClawcodeAgent` 可被 HTTP transport 创建

**Files:**
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/agent.rs`

- [ ] **Step 1: 写失败的编译测试**

在 `agent.rs` 测试模块中加入一个 compile-only helper，要求 `ClawcodeAgent: ConnectTo<Client>`：

```rust
/// Verifies the ACP agent can be returned by HTTP transport factories.
#[test]
fn clawcode_agent_implements_connect_to_client() {
    fn assert_connect_to_client<T: ConnectTo<Client>>() {}

    assert_connect_to_client::<ClawcodeAgent>();
}
```

- [ ] **Step 2: 运行测试并确认失败**

Run:

```bash
rtk cargo test -p acp clawcode_agent_implements_connect_to_client
```

Expected: FAIL，因为 `ClawcodeAgent` 尚未实现 `ConnectTo<Client>`。

- [ ] **Step 3: 实现 `ConnectTo<Client>`**

把当前 `serve()` 中的 `Agent.builder()` handler 注册逻辑移动到 `impl ConnectTo<Client> for ClawcodeAgent`，并让 `serve()` 调用该实现：

```rust
impl ConnectTo<Client> for ClawcodeAgent {
    /// Connects the clawcode ACP agent to an ACP client transport.
    async fn connect_to(self, transport: impl ConnectTo<Agent>) -> acp::Result<()> {
        let agent = Arc::new(self);

        Agent
            .builder()
            .name("claw-acp")
            // existing handler registrations stay here
            .connect_to(transport)
            .await
    }
}
```

- [ ] **Step 4: 确认 stdio 仍编译**

Run:

```bash
rtk cargo test -p acp clawcode_agent_implements_connect_to_client
```

Expected: PASS。

---

### Task 3: HTTP server router 和 CLI

**Files:**
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/http.rs`
- Modify: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/main.rs`

- [ ] **Step 1: 写 router 测试**

测试 `build_router` 能创建 `/`、`/app.js` 和 `/health` 响应：

```rust
/// Verifies the HTTP router serves the embedded web shell.
#[tokio::test]
async fn router_serves_web_assets() {
    let router = build_web_router();

    let response = router
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: 运行测试并确认失败**

Run:

```bash
rtk cargo test -p acp router_serves_web_assets
```

Expected: FAIL，因为 router 尚未实现或缺少测试依赖。

- [ ] **Step 3: 实现 web router 和 server 启动函数**

实现：

```rust
/// Builds the static web router mounted next to the ACP endpoint.
fn build_web_router() -> Router

/// Runs the ACP HTTP server until the listener fails or the process exits.
pub async fn run_with_routers(...) -> std::io::Result<()>
```

`run_with_routers` 使用 `AcpHttpServer::new(move || ClawcodeAgent::with_routers(...))`。

- [ ] **Step 4: 更新 CLI**

`main.rs` 新增：

```rust
#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    http: bool,
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, default_value_t = 0)]
    port: u16,
}
```

未传 `--http` 时保持 stdio 行为；传入 `--http` 时调用 `acp::http::run_with_routers(...)`。

- [ ] **Step 5: 确认后端测试通过**

Run:

```bash
rtk cargo test -p acp router_serves_web_assets
```

Expected: PASS。

---

### Task 4: Web ACP client

**Files:**
- Create: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/web/index.html`
- Create: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/web/app.js`
- Create: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/web/styles.css`

- [ ] **Step 1: 创建无构建链页面**

页面包含这些稳定 id：

```html
<aside id="sessions"></aside>
<select id="modelSelect"></select>
<main id="transcript"></main>
<textarea id="promptInput"></textarea>
<button id="sendButton"></button>
<button id="cancelButton"></button>
<div id="permissionModal"></div>
```

- [ ] **Step 2: 实现 JSON-RPC request map**

`app.js` 提供：

```javascript
function sendRequest(method, params) { /* stores pending resolver by id */ }
function sendNotification(method, params) { /* sends JSON-RPC notification */ }
function handleMessage(message) { /* routes response, request, notification */ }
```

- [ ] **Step 3: 实现首版 ACP flows**

必须调用并处理：

```javascript
initialize
session/list
session/new
session/load
session/prompt
session/cancel
session/set_config_option
session/request_permission
session/update
```

- [ ] **Step 4: 权限请求响应**

`session/request_permission` 的按钮必须来自 `params.options`，选择后返回：

```json
{
  "outcome": {
    "outcome": "selected",
    "optionId": "allow_once"
  }
}
```

取消时返回：

```json
{
  "outcome": {
    "outcome": "cancelled"
  }
}
```

- [ ] **Step 5: 模型切换**

从 `config_options` 中找到 `id == "model"` 或 `category == "model"` 的 select option，切换时发送：

```javascript
sendRequest("session/set_config_option", {
  sessionId: currentSessionId,
  configId: "model",
  value: selectedModelId
});
```

---

### Task 5: 验证和修复

**Files:**
- Modify as needed: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/src/http.rs`
- Modify as needed: `/home/isbest/Documents/WorkSpace/clawcode/crates/acp/web/app.js`

- [ ] **Step 1: 格式化**

Run:

```bash
rtk cargo fmt --check
```

Expected: PASS。

- [ ] **Step 2: 后端测试**

Run:

```bash
rtk cargo test -p acp
```

Expected: PASS。

- [ ] **Step 3: 工作区编译**

Run:

```bash
rtk cargo check --workspace
```

Expected: PASS。

- [ ] **Step 4: 手动启动检查**

Run:

```bash
rtk cargo run -p acp -- --http --port 0
```

Expected: stderr 或 log 输出本机 URL；浏览器可打开页面并连接 `/acp`。

# ACP HTTP Web Chat 设计方案

**日期**: 2026-07-07  
**状态**: 已确认，待实现

## 1. 背景

当前 `crates/acp` 已经把 `protocol::AgentKernel` 适配成 ACP agent，并通过 stdio 暴露给编辑器或 TUI。用户希望新增一种 HTTP 暴露方式：把同一个 agent 暴露为 HTTP server，并内置一个网页来完成首版聊天体验。

官方 `agent-client-protocol-http` crate 已提供 ACP HTTP/WebSocket transport。服务端应复用该 transport，而不是自定义一套 ACP over HTTP 协议。网页端作为最小 ACP client，通过 WebSocket 连接 `/acp`，直接发送和接收 JSON-RPC ACP 消息。

## 2. 目标

1. `crates/acp` 新增 HTTP server 模式，复用现有 `ClawcodeAgent`、`Kernel`、`ToolRegistry`、`AcpClientFsRouter` 和 `AcpClientTerminalRouter`。
2. HTTP 模式默认监听 `127.0.0.1`，避免默认暴露本地 agent 能力到局域网。
3. 同一个 Axum 应用同时提供：
   - `/acp`: 官方 ACP HTTP/WebSocket endpoint。
   - `/`: 内置网页聊天入口。
   - `/app.js`、`/styles.css`: 内置静态资源。
4. 网页首版支持：
   - 基础聊天：`initialize`、`session/new`、`session/prompt`、`session/update`。
   - 历史列表：`session/list`。
   - 恢复会话：用现有 `session/load` 实现 resume。
   - 权限请求：`session/request_permission`，按 agent 返回的 `options` 动态渲染按钮并回传选择。
   - 取消：`session/cancel`。
   - 模型切换：通过 ACP v1.4 `session/set_config_option` 修改 `model` 配置项。
5. 网页首版不声明 filesystem 或 terminal client capability，避免浏览器成为本地文件或终端代理。

## 3. 非目标

1. 不新增独立 `/api/chat` 或 `/api/events` 简化协议。
2. 不实现多用户、鉴权、远程公网部署能力。
3. 不实现浏览器端文件系统代理、终端代理、图片输入、文件上传。
4. 不新增 ACP `session/resume` handler；首版 resume 使用当前已实现的 `session/load`。
5. 不替换 TUI 的 in-process ACP client/server 流程。

## 4. 架构

```text
Browser Web Chat
  <-> WebSocket JSON-RPC ACP
  <-> /acp via agent-client-protocol-http
  <-> ClawcodeAgent
  <-> AgentKernel
  <-> provider/tools/store
```

HTTP server 与 stdio server 共享同一个 agent 适配层。为了让官方 HTTP transport 可以按连接创建 agent 组件，`ClawcodeAgent` 需要实现 `ConnectTo<Client>`。现有 stdio `serve()` 继续保留，但内部复用同一个 `ConnectTo<Client>` 实现，避免 stdio 与 HTTP 出现两套 handler 注册逻辑。

## 5. 后端设计

新增 `crates/acp/src/http.rs`：

1. 定义 `HttpServerOptions`，包含绑定地址和 ACP path。
2. 提供 `run_with_routers(kernel, fs_router, terminal_router, options)`。
3. 使用 `agent_client_protocol_http::AcpHttpServer::new(factory)` 创建 ACP router。
4. `factory` 每次返回一个新的 `ClawcodeAgent`，但共享同一个 kernel 和 routers。
5. 用 `include_str!` 嵌入网页资源，避免引入构建脚本或额外静态文件服务器。
6. server 启动后输出实际监听 URL，端口为 `0` 时显示 OS 分配端口。

`crates/acp/src/main.rs` 新增 CLI 参数：

```text
--http                 启用 HTTP/Web 模式；未指定时保持 stdio 行为
--host 127.0.0.1       HTTP 监听 host
--port 0               HTTP 监听端口，0 表示自动分配
```

## 6. 网页设计

网页为单页应用，不引入前端构建链。

主要状态：

1. WebSocket 连接状态。
2. 当前 session id。
3. `session/list` 返回的历史会话列表和分页 cursor。
4. `session/new` 或 `session/load` 返回的 config options。
5. 当前模型 config option 和可选模型列表。
6. 聊天消息列表。
7. 运行中 prompt 状态。
8. 当前待处理权限请求。

ACP 消息处理：

1. `session/update`:
   - `agent_message_chunk` 追加到当前 assistant 消息。
   - `agent_thought_chunk` 追加为 thought 消息。
   - `tool_call`、`tool_call_update` 渲染为工具状态消息。
   - `usage_update` 更新状态栏。
   - 其他 update 以简短系统消息或 JSON 摘要显示。
2. `session/request_permission`:
   - 阻塞当前交互，展示 tool call 标题和内容。
   - 每个 ACP `PermissionOption` 渲染为一个按钮。
   - 点击按钮返回 `RequestPermissionOutcome::Selected`。
   - 关闭或取消返回 `RequestPermissionOutcome::Cancelled`。
3. `session/cancel`:
   - 用户点击 Cancel 后发送 notification。
   - UI 标记为 cancelling，等待 prompt response 或错误返回。
4. `session/set_config_option`:
   - 选择模型后发送 request。
   - 成功后更新当前模型显示；失败则显示错误。

## 7. 错误处理

1. WebSocket 断开时禁用输入区，并显示 reconnect 按钮。
2. `initialize`、`session/new`、`session/load` 失败时显示系统错误。
3. `session/prompt` 失败时结束 running 状态，保留用户输入上下文。
4. 权限请求超出首版可渲染能力时显示 JSON 摘要，但仍提供 agent 返回的可选操作。
5. 模型切换失败时保留原模型，并显示错误。

## 8. 测试策略

1. Rust 单元测试覆盖 HTTP options 默认值、静态资源路由和 ACP router 挂载。
2. Rust 编译测试通过 `cargo check -p acp` 确认 `ClawcodeAgent` 可作为 HTTP transport factory 返回值。
3. 前端首版为无构建链静态 JS，通过 Rust 测试校验嵌入资源存在关键 DOM id 和 ACP method 字符串。
4. 完整验证运行：
   - `rtk cargo fmt --check`
   - `rtk cargo test -p acp`
   - `rtk cargo check --workspace`

## 9. 风险

1. **浏览器直接说 ACP 的复杂度**：需要处理 request、response、notification 三类 JSON-RPC 消息。缓解方式是前端只实现首版必要方法，并把未知消息显示为系统摘要。
2. **权限请求安全风险**：用户可能允许执行命令。缓解方式是默认本机监听，并在弹窗中展示 agent 给出的 tool call 内容和全部 option。
3. **浏览器 capability 风险**：首版不声明 filesystem/terminal capability，因此 agent 请求这些 client-side 能力会在后端 router 中得到“不支持”错误。
4. **多连接共享状态风险**：kernel 可共享，但每个 HTTP 连接需要独立 `ClawcodeAgent` 存储 client capabilities。factory 返回新 agent，routers 共享。
5. **协议版本漂移**：当前代码基于 ACP v1.4 config option 模型切换。实现时通过现有 Rust schema 生成 JSON 形状，避免手写错误。

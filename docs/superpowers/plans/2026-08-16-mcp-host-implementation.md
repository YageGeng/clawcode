# MCP Host 实施计划

> **执行约束：** 实施时使用 `superpowers:executing-plans` 在当前会话逐任务执行。项目禁止 SubAgent 和 worktree；未经用户明确要求不得创建 commit，因此本计划不包含 commit 步骤。

**目标：** 将现有一次性 MCP Tool 适配器重构为 Session 级、精确协议版本、支持动态能力和完整 Host 生命周期的 MCP Host，同时把能力通过 Kernel、ACP 2.0 WebSocket 和 WebUI 暴露。

**架构：** `protocol` 保存唯一公共 MCP 类型；`config` 只负责 TOML 解析和启动期校验；`mcp` 负责 transport、精确协议生命周期、Server 状态机、不可变 Catalog、Host 回调、MRTR、Tasks 和认证；`kernel` 负责 Session 所有权、动态 Tool 分区、Prompt/Resource 语义接入和 Turn 关联；`store` 只持久化正常消息与安全凭据；`acp` 提供产品命名空间扩展方法；WebUI 只消费 ACP Snapshot，不自行实现 MCP。

**设计依据：** `docs/superpowers/specs/2026-08-16-mcp-host-design.md`

**技术栈：** Rust 2024、rmcp 3、Tokio、ArcSwap、Serde、typed-builder、Reqwest、ACP 2.0 Rust SDK、React 19、TypeScript 6、Zustand、Vite。

## 全局约束

- 新增函数和方法必须写英文作用注释；修改旧逻辑必须写英文变动原因注释。
- 后端按 TDD 执行：先写失败的集成测试并确认失败原因，再实现最小代码，最后重构。
- 测试只能放在 crate 的 `tests/` 目录或精确的 `#[cfg(test)] mod tests` 中；正文不得增加测试支撑字段、函数、实现或 cfg 分支。
- 前端不要求 TDD 或单元测试，但必须通过 TypeScript、ESLint、构建和真实后端端到端验证。
- 避免大量短小 helper；只有一个自定义类型参数的行为优先实现为关联方法、`From`、`TryFrom`、`FromStr` 或 trait 实现。
- 超过三个字段的新增 Rust 结构体使用 `typed-builder`；共享 `Arc` 必须显式使用 `Arc::clone`。
- 跨 crate MCP 类型只定义在 `protocol`；`config`、`mcp`、`kernel`、`acp` 和 `store` 不重复定义同义结构体，也不向 Kernel 暴露 rmcp 类型。
- Cargo 相对路径依赖放在依赖列表前部；新依赖先加入 workspace，再由子 crate 使用 workspace 依赖。
- 项目名称和 ACP 命名空间只使用 `protocol::ProductIdentity`，不得新增散落的项目名字符串。
- 日志使用完整 `tracing::level!()` 路径和格式化字符串，不导入 tracing 宏，不使用 tracing field 风格；不得记录凭据、Header、完整 Tool 参数或结果。
- 每个 Turn 内的 MCP 消息必须携带 TurnId、TraceId 和 `TimestampMs`；后者在 JSON 中保持字符串毫秒时间戳。
- 不实现自动协议探测或回退、配置热更新、旧 HTTP+SSE 双端点、MCP Apps/UI Extension、ACP Client FS/Terminal callback、自动 Prompt/Resource 注入和 Tasks list。
- Session 恢复和 fork 重新建立独立 MCP 连接，不持久化连接、Catalog、状态机、watcher 或缓存，也不增加 `index.jsonl`。
- 所有 shell 命令使用 `rtk` 前缀。

---

### 任务 1：建立唯一 MCP 公共协议模型

**文件：**

- 删除：`crates/protocol/src/mcp.rs`
- 新增：`crates/protocol/src/mcp/mod.rs`
- 新增：`crates/protocol/src/mcp/identity.rs`
- 新增：`crates/protocol/src/mcp/catalog.rs`
- 新增：`crates/protocol/src/mcp/status.rs`
- 新增：`crates/protocol/src/mcp/request.rs`
- 新增：`crates/protocol/src/mcp/auth.rs`
- 新增：`crates/protocol/src/mcp/task.rs`
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/protocol/src/lib.rs`
- 新增：`crates/protocol/tests/mcp.rs`

**公共接口：**

```rust
pub enum McpProtocolVersion { V2025_11_25, V2026_07_28 }
pub enum McpTransportKind { Stdio, StreamableHttp }
pub enum McpServerState {
    Disabled, Starting, Negotiating, Discovering, Ready,
    Degraded, Failed, Stopping, Stopped,
}
pub enum McpFailureStage {
    Transport, Negotiation, Discovery, Synchronization,
    Authentication, Invocation, Shutdown,
}

pub struct McpServerId(String);
pub struct McpToolRef { pub server_id: McpServerId, pub remote_name: String }
pub struct McpPromptRef { pub server_id: McpServerId, pub remote_name: String }
pub struct McpResourceRef { pub server_id: McpServerId, pub remote_uri: String }

pub struct McpArgumentInfo {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}
pub struct McpToolInfo {
    pub reference: McpToolRef,
    pub public_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}
pub struct McpPromptInfo {
    pub reference: McpPromptRef,
    pub title: Option<String>,
    pub description: Option<String>,
    pub arguments: Vec<McpArgumentInfo>,
}
pub struct McpResourceInfo {
    pub reference: McpResourceRef,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
}
pub struct McpResourceTemplateInfo {
    pub server_id: McpServerId,
    pub uri_template: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}
pub struct McpCatalogRevisions {
    pub tools: u64,
    pub prompts: u64,
    pub resources: u64,
    pub resource_templates: u64,
}
pub struct McpCapabilityCounts {
    pub tools: usize,
    pub prompts: usize,
    pub resources: usize,
    pub resource_templates: usize,
}
pub struct McpServerImplementation { pub name: String, pub version: String }
pub struct McpServerFailure {
    pub stage: McpFailureStage,
    pub message: String,
    pub occurred_at_ms: TimestampMs,
}
pub struct McpServerStatus {
    pub server_id: McpServerId,
    pub protocol: McpProtocolVersion,
    pub transport: McpTransportKind,
    pub state: McpServerState,
    pub implementation: Option<McpServerImplementation>,
    pub revisions: McpCatalogRevisions,
    pub counts: McpCapabilityCounts,
    pub failure: Option<McpServerFailure>,
    pub oauth: McpOAuthStatus,
    pub updated_at_ms: TimestampMs,
}
pub struct McpSessionSnapshot { pub revision: u64, pub servers: Vec<McpServerStatus> }

pub enum McpHostRequest {
    Sampling(McpSamplingRequest),
    Elicitation(McpElicitationRequest),
    Roots(McpRootsRequest),
    Authorization(McpAuthorizationRequest),
}
pub enum McpHostResponse {
    Sampling(McpSamplingResult),
    Elicitation(McpElicitationResult),
    Roots(McpRootsResult),
    Authorization(McpAuthorizationResult),
}
pub enum McpCompletionTarget {
    Prompt(McpPromptRef),
    ResourceTemplate { server_id: McpServerId, uri_template: String },
}
pub struct McpCompletionRequest {
    pub target: McpCompletionTarget,
    pub argument_name: String,
    pub argument_value: String,
    pub context: BTreeMap<String, String>,
}
pub struct McpCompletionResult {
    pub values: Vec<String>,
    pub total: Option<u64>,
    pub has_more: bool,
}
pub struct McpTaskStatus {
    pub task_id: String,
    pub state: McpTaskState,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub updated_at_ms: TimestampMs,
}
```

- [ ] **步骤 1：先写类型和 wire 失败测试**

在 `crates/protocol/tests/mcp.rs` 验证：两个精确协议字符串、非法 `McpServerId`、结构化引用 round-trip、九种状态、failure stage、Catalog revision、OAuth 状态、Task 状态以及所有 `TimestampMs` 的字符串 JSON 表示。

- [ ] **步骤 2：确认测试因公共类型缺失失败**

```bash
rtk cargo test -p protocol --test mcp
```

- [ ] **步骤 3：实现分文件公共模型并清除旧类型**

加入上述唯一新模型。`McpServerId` 使用 `TryFrom<String>` 校验空白、重复分隔语义和 Tool namespace 安全字符；公开名称只由 `McpToolRef::public_name()` 生成，调用路由不反向解析字符串。由于当前 mcp/kernel 仍直接依赖 `McpConnectionState`、旧 `McpServerInfo`、旧 `McpToolInfo`、`McpToolDescriptor` 和字符串化 `McpCallResult`，这些旧类型仅保留到任务 5 的 Session Snapshot 切换检查点，并在同一次可编译迁移中删除；最终产物不保留兼容层或重复模型。

- [ ] **步骤 4：运行协议测试和静态检查**

```bash
rtk cargo test -p protocol --test mcp
rtk cargo test -p protocol
rtk cargo clippy -p protocol --all-targets -- -D warnings
rtk cargo fmt --all -- --check
```

---

### 任务 2：扩展无损 ContentBlock 并统一 Provider、Store、ACP 映射

**文件：**

- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/protocol/src/tool.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/acp/src/mapping.rs`
- 新增：`crates/protocol/tests/content_block.rs`
- 新增：`crates/acp/tests/mcp_content.rs`
- 修改：`crates/kernel/tests/provider_runtime.rs`

**公共内容类型：**

```rust
pub enum ContentBlock {
    Text { text: String },
    Image { data: String, mime_type: String },
    Audio { data: String, mime_type: String },
    EmbeddedResource { uri: String, mime_type: Option<String>, content: Box<ContentBlock> },
    ResourceLink { uri: String, name: Option<String>, mime_type: Option<String> },
    Structured { value: serde_json::Value },
    Reasoning { text: String },
    ToolCall {
        tool_call_id: ToolCallId,
        name: String,
        arguments: serde_json::Value,
    },
}
```

- [ ] **步骤 1：先写内容 round-trip 和 ACP 降级规则失败测试**

覆盖 Text、Image、Audio、Embedded Resource、Resource Link 和 Structured Content 的 Serde round-trip；ACP 原生支持的块使用原生类型，不支持的块使用 ACP 2.0 产品扩展更新并保留原始字段，不能转为伪文本。

- [ ] **步骤 2：确认当前枚举和字符串化行为导致测试失败**

```bash
rtk cargo test -p protocol --test content_block
rtk cargo test -p acp --test mcp_content
```

- [ ] **步骤 3：实现唯一内容模型和显式适配策略**

扩展公共枚举后，逐一修复 Kernel Provider adapter、持久化和 ACP 的穷尽匹配，不改动成熟 Provider crate 的公共内容模型。Provider 不支持直接发送的 MCP 内容由 Kernel adapter 使用稳定文本描述加来源引用，原始无损块仍保留在持久化和 ACP 扩展消息中；不能调用 `serde_json::to_string` 冒充 Text。

- [ ] **步骤 4：运行跨 crate 检查**

```bash
rtk cargo test -p protocol --test content_block
rtk cargo test -p acp --test mcp_content
rtk cargo test -p kernel --test provider_runtime
rtk cargo check -p kernel -p store -p acp
rtk cargo clippy -p protocol -p kernel -p acp --all-targets -- -D warnings
```

---

### 任务 3：实现显式 TOML 协议、transport、timeout 和认证校验

**文件：**

- 修改：`crates/config/src/mcp.rs`
- 修改：`crates/config/tests/loading.rs`
- 新增：`crates/config/tests/mcp.rs`
- 新增：`crates/mcp/src/config.rs`
- 修改：`crates/mcp/src/lib.rs`
- 新增：`crates/mcp/tests/config.rs`
- 修改：`claw.toml`

**配置形状：**

```toml
[[mcp_servers]]
name = "filesystem"
protocol = "2025-11-25" # 或 "2026-07-28"，必填
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
startup_timeout_sec = 30
request_timeout_sec = 120
mrtr_max_rounds = 8
mrtr_total_timeout_sec = 300
```

- [ ] **步骤 1：先写配置失败测试**

覆盖缺失/未知协议、重复/非法 name、command 与 url 互斥、空 command/url、零 timeout、OAuth+stdio、Bearer+OAuth 冲突、Header 非法、MRTR 限制非法。验证配置错误发生在应用启动期，不产生部分 Runtime。

- [ ] **步骤 2：确认当前配置允许缺失协议和非法组合**

```bash
rtk cargo test -p config --test mcp
rtk cargo test -p mcp --test config
```

- [ ] **步骤 3：实现 `TryFrom<McpServerConfig> for RuntimeMcpServer`**

`RuntimeMcpServer` 移入 `mcp::config`，保存 `McpServerId`、精确 `McpProtocolVersion`、transport、timeout、MRTR 策略和认证策略。删除通用 `present_string` helper，把单个配置值的校验实现为相应值类型的 `TryFrom`；配置列表重复检查由 `Config::validate` 统一处理。

- [ ] **步骤 4：更新示例配置并运行检查**

```bash
rtk cargo test -p config --test mcp
rtk cargo test -p mcp --test config
rtk cargo check -p app
rtk cargo fmt --all -- --check
```

---

### 任务 4：重构 MCP Client 边界并实现两个精确协议生命周期

**文件：**

- 删除：`crates/mcp/src/client.rs`
- 新增：`crates/mcp/src/client/mod.rs`
- 新增：`crates/mcp/src/client/legacy.rs`
- 新增：`crates/mcp/src/client/modern.rs`
- 新增：`crates/mcp/src/transport.rs`
- 新增：`crates/mcp/src/error.rs`
- 修改：`crates/mcp/src/lib.rs`
- 修改：`crates/mcp/Cargo.toml`
- 修改：`Cargo.toml`
- 新增：`crates/mcp/tests/lifecycle_legacy.rs`
- 新增：`crates/mcp/tests/lifecycle_modern.rs`
- 新增：`crates/mcp/tests/support/mod.rs`

**内部接口：**

```rust
#[async_trait]
pub trait McpClient: Send + Sync {
    fn server_info(&self) -> &McpServerImplementation;
    fn capabilities(&self) -> &McpServerCapabilities;
    async fn discover(&self) -> Result<McpCatalogSnapshot, McpError>;
    async fn call_tool(&self, request: McpToolRequest) -> Result<McpToolResult, McpError>;
    async fn get_prompt(&self, request: McpPromptRequest) -> Result<McpPromptResult, McpError>;
    async fn read_resource(&self, request: McpResourceRequest) -> Result<McpResourceResult, McpError>;
    async fn complete(&self, request: McpCompletionRequest) -> Result<McpCompletionResult, McpError>;
    async fn shutdown(&self) -> Result<(), McpError>;
}
```

- [ ] **步骤 1：用可观测 reference server 写失败测试**

Legacy 测试断言顺序只能是 connect → initialize(`2025-11-25`) → initialized → list；Modern 测试断言只能是 transport ready → `server/discover(preferred=2026-07-28)` → list，并检查后续请求元数据。两组都测试服务端声明其他协议时进入 `ProtocolMismatch`，且绝不发送另一种生命周期请求。

- [ ] **步骤 2：确认当前 `.serve()` 默认初始化无法满足两套断言**

```bash
rtk cargo test -p mcp --test lifecycle_legacy
rtk cargo test -p mcp --test lifecycle_modern
```

- [ ] **步骤 3：实现 transport 与生命周期策略对象**

复用 rmcp 3 的 v1/v2 生命周期 API和 transport，`LegacyClient` 与 `ModernClient` 分别持有强类型 service；共同操作通过 `McpClient` 归一化。stdio 将 stdout 仅用于 JSON-RPC、stderr 进入受控日志；HTTP 应用 Session/Protocol Header。禁止在同一函数中用方法名字符串分支协议。

- [ ] **步骤 4：实现完整分页发现和 capability gate**

Tools、Prompts、Resources、Resource Templates 使用 rmcp helper 或显式 cursor 循环拉取完整目录；未声明能力时返回空 Catalog，不发送请求；任何一页失败均不发布初始目录。

- [ ] **步骤 5：验证两个生命周期和无回退行为**

```bash
rtk cargo test -p mcp --test lifecycle_legacy
rtk cargo test -p mcp --test lifecycle_modern
rtk cargo clippy -p mcp --all-targets -- -D warnings
```

---

### 任务 5：实现 Session/Server 状态机与原子 Snapshot

**文件：**

- 删除：`crates/mcp/src/factory.rs`
- 新增：`crates/mcp/src/factory.rs`
- 新增：`crates/mcp/src/session.rs`
- 新增：`crates/mcp/src/server.rs`
- 新增：`crates/mcp/src/catalog.rs`
- 新增：`crates/mcp/src/event.rs`
- 修改：`crates/mcp/src/lib.rs`
- 新增：`crates/mcp/tests/session_lifecycle.rs`
- 新增：`crates/mcp/tests/snapshot.rs`

**Factory 边界：**

```rust
#[async_trait]
pub trait McpFactory: Send + Sync {
    async fn create(&self, request: McpSessionRequest) -> Result<McpSession, McpError>;
}

pub struct McpSessionRequest {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub host: Arc<dyn McpHost>,
    pub shutdown: CancellationToken,
}
```

- [ ] **步骤 1：先写并发 bootstrap 和状态迁移失败测试**

覆盖 Disabled、healthy、transport failed、protocol mismatch、discovery failed 混合 Server；验证并发启动、配置顺序稳定、单 Server 故障隔离、状态迁移合法、Session revision 单调递增和初始目录原子发布。

- [ ] **步骤 2：确认旧 Factory 顺序连接和不可变状态不满足测试**

```bash
rtk cargo test -p mcp --test session_lifecycle
rtk cargo test -p mcp --test snapshot
```

- [ ] **步骤 3：实现类型驱动状态机和不可变快照**

`McpServerRuntime` 拥有 client、状态、Catalog、watcher、在途调用与 shutdown token；`McpSession` 使用 `ArcSwap<McpSessionSnapshot>` 发布一致读视图，并通过 broadcast 发送只含 revision/ServerId/变化种类的 `McpSessionEvent`。状态转换由 `McpServerStateMachine` 方法控制，不能在各调用点直接写枚举。

- [ ] **步骤 4：实现 Factory 故障隔离**

所有 enabled Server 并发 bootstrap，settle 后返回 Session；只有 Session 基础设施构造失败才返回整体错误。单 Server 错误写入安全 `McpServerFailure`，保留 ServerId、协议、transport 和 stage。

- [ ] **步骤 5：运行生命周期检查**

```bash
rtk cargo test -p mcp --test session_lifecycle
rtk cargo test -p mcp --test snapshot
rtk cargo clippy -p mcp --all-targets -- -D warnings
```

---

### 任务 6：实现动态目录同步、订阅、缓存提示和显式重连

**文件：**

- 新增：`crates/mcp/src/sync/mod.rs`
- 新增：`crates/mcp/src/sync/legacy.rs`
- 新增：`crates/mcp/src/sync/modern.rs`
- 修改：`crates/mcp/src/server.rs`
- 修改：`crates/mcp/src/session.rs`
- 修改：`crates/mcp/src/catalog.rs`
- 新增：`crates/mcp/tests/dynamic_catalog.rs`
- 新增：`crates/mcp/tests/reconnect.rs`

- [ ] **步骤 1：先写动态同步失败测试**

Legacy 覆盖 tools/prompts/resources list_changed、resource subscribe/update；Modern 覆盖 `subscriptions/listen`、cache hint/TTL 和 stream 终止。验证每次变化都重新获取受影响能力的全部分页、原子替换、增加对应 Catalog 和 Session revision。

- [ ] **步骤 2：写 Degraded 与 last-good Snapshot 测试**

让刷新中间页失败，断言旧目录仍可读、Server 进入 Degraded、错误包含能力种类和时间；后续通知刷新成功后恢复 Ready。任何错误都不得触发自动 reconnect。

- [ ] **步骤 3：实现协议专属 watcher 和统一同步事件**

`LegacySynchronizer` 与 `ModernSynchronizer` 只产生 `CatalogInvalidated`/`ResourceUpdated` 等内部事件；`McpServerRuntime` 执行整目录事务。Modern cache TTL 到期只触发 revalidation，不逐项原地改写。

- [ ] **步骤 4：实现显式 reconnect**

`McpSession::reconnect` 先发布 Stopping 并撤下该 Server 目录，取消 watcher、Tasks 和在途调用，等待 transport 关闭，再按原精确协议 bootstrap；成功发布新 Snapshot，失败进入 Failed。测试确认重连期间新调用被拒绝且不存在自动回退。

- [ ] **步骤 5：运行动态能力和重连测试**

```bash
rtk cargo test -p mcp --test dynamic_catalog
rtk cargo test -p mcp --test reconnect
rtk cargo clippy -p mcp --all-targets -- -D warnings
```

---

### 任务 7：实现 MCP Tool 适配、Schema、progress、取消和完整内容

**文件：**

- 新增：`crates/mcp/src/tool.rs`
- 新增：`crates/mcp/src/content.rs`
- 修改：`crates/mcp/src/client/mod.rs`
- 修改：`crates/mcp/src/server.rs`
- 修改：`crates/tools/src/contract.rs`
- 新增：`crates/mcp/tests/tool.rs`
- 新增：`crates/kernel/tests/mcp_tool_pipeline.rs`

- [ ] **步骤 1：先写 Tool 调用失败测试**

覆盖结构化 `McpToolRef` 路由、公开名称、对象参数、JSON Schema 错误、Text/Image/Audio/Embedded Resource/Resource Link/Structured Content、远端 `isError`、transport error、progress、Turn cancel、request timeout 和不可突破的总时限。

- [ ] **步骤 2：确认旧 Tool 适配器会把非文本内容序列化成字符串**

```bash
rtk cargo test -p mcp --test tool
rtk cargo test -p kernel --test mcp_tool_pipeline
```

- [ ] **步骤 3：实现 `McpAgentTool` 和无损转换**

适配器保存 `McpToolRef`、Server runtime handle、定义 revision 和 timeout policy。`execute` 使用公共 `ToolExecutionContext` 的 SessionId、TurnId、取消 token 和 update sink，不解析公开名称。远端业务错误映射 `ToolResult.is_error`；transport/协议/timeout 映射类型化 `ToolError::Execution`。

- [ ] **步骤 4：实现协议取消与 progress 行为**

Legacy 超时/取消发送 cancellation notification；Modern 关闭请求流。Progress 可刷新活动 deadline，但不能超过总 deadline；日志不打印参数和结果。确保 Extension 的 tool_call/tool_result hooks、持久化和 ACP 更新仍包裹 MCP Tool。

- [ ] **步骤 5：运行 Tool pipeline 检查**

```bash
rtk cargo test -p mcp --test tool
rtk cargo test -p kernel --test mcp_tool_pipeline
rtk cargo clippy -p mcp -p kernel -p tools --all-targets -- -D warnings
```

---

### 任务 8：实现 Host Sampling、Elicitation、Roots、MRTR 和 Tasks

**文件：**

- 新增：`crates/mcp/src/host.rs`
- 新增：`crates/mcp/src/mrtr.rs`
- 新增：`crates/mcp/src/task.rs`
- 修改：`crates/mcp/src/client/legacy.rs`
- 修改：`crates/mcp/src/client/modern.rs`
- 新增：`crates/kernel/src/runtime/mcp_host.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 新增：`crates/mcp/tests/host.rs`
- 新增：`crates/mcp/tests/mrtr.rs`
- 新增：`crates/mcp/tests/tasks.rs`
- 新增：`crates/kernel/tests/mcp_host.rs`

**Host 边界：**

```rust
#[async_trait]
pub trait McpHost: Send + Sync {
    async fn sample(&self, request: McpSamplingRequest) -> Result<McpSamplingResult, McpHostError>;
    async fn elicit(&self, request: McpElicitationRequest) -> Result<McpElicitationResult, McpHostError>;
    async fn roots(&self, request: McpRootsRequest) -> Result<McpRootsResult, McpHostError>;
    async fn authorize(&self, request: McpAuthorizationRequest) -> Result<McpAuthorizationResult, McpHostError>;
}
```

- [ ] **步骤 1：先写 Legacy Host 请求失败测试**

验证 Sampling 通过 Kernel Model/Provider 路径并继承 SessionId、TurnId、TraceId；Elicitation 产生可由 ACP 完成的 Turn 关联请求；Roots 仅返回 Kernel 允许的 cwd/root；Server logging 只进入受控 tracing，不能修改日志过滤器。

- [ ] **步骤 2：先写 Modern MRTR 失败测试**

覆盖单轮、多轮、opaque `requestState` 原样返回、缺少 capability、用户取消、单轮 timeout、总 timeout 和最大轮次。断言无限循环被明确终止。

- [ ] **步骤 3：先写 Tasks 扩展失败测试**

覆盖 capability gate、`tasks/get`、`tasks/update`、`tasks/cancel`、progress 到 ToolUpdate、最终 ToolCallId/TurnId 保持以及取消/shutdown 清理；明确断言不调用已删除的 `tasks/list`。

- [ ] **步骤 4：实现 Kernel Host 和协议适配**

`KernelMcpHost` 只使用公共请求类型和 Kernel 正常运行路径，不向 mcp crate 暴露 Kernel。MRTR 由有界 `McpMrtrDriver` 驱动；Task 由 `McpTaskDriver` 驱动并注册到 Server runtime 的在途集合。

- [ ] **步骤 5：运行 Host、MRTR 和 Tasks 测试**

```bash
rtk cargo test -p mcp --test host
rtk cargo test -p mcp --test mrtr
rtk cargo test -p mcp --test tasks
rtk cargo test -p kernel --test mcp_host
rtk cargo clippy -p mcp -p kernel --all-targets -- -D warnings
```

---

### 任务 9：实现 Secret Store 与 HTTP OAuth 生命周期

**文件：**

- 新增：`crates/store/src/secret.rs`
- 新增：`crates/store/src/file_secret.rs`
- 修改：`crates/store/src/lib.rs`
- 修改：`crates/store/Cargo.toml`
- 新增：`crates/store/tests/secret.rs`
- 新增：`crates/mcp/src/auth/mod.rs`
- 新增：`crates/mcp/src/auth/metadata.rs`
- 新增：`crates/mcp/src/auth/oauth.rs`
- 修改：`crates/mcp/src/transport.rs`
- 修改：`crates/mcp/src/factory.rs`
- 修改：`crates/app/src/lib.rs`
- 新增：`crates/mcp/tests/oauth.rs`

**Secret 边界：**

```rust
pub trait SecretStore: Send + Sync {
    fn load(&self, key: &SecretKey) -> Result<Option<SecretValue>, SecretStoreError>;
    fn store(&self, key: &SecretKey, value: &SecretValue) -> Result<(), SecretStoreError>;
    fn delete(&self, key: &SecretKey) -> Result<(), SecretStoreError>;
}
```

- [ ] **步骤 1：先写文件 Secret Store 失败测试**

验证稳定 ServerId/subject key、目录和文件权限、原子替换、并发读取、损坏文件错误，以及 Debug/Display/Serde 均不会泄露 SecretValue。确认 Session JSONL 不出现 token、refresh token、client secret 或 authorization code。

- [ ] **步骤 2：实现 Secret Store 抽象和文件后端**

使用独立凭据目录、限制权限和同目录临时文件原子 rename。`store` 不理解 OAuth；`mcp` 只依赖 trait。Secret Store 构造失败是应用启动 fatal error。

- [ ] **步骤 3：先写 OAuth 状态机失败测试**

使用本地 HTTP reference server 覆盖 Protected Resource Metadata、Authorization Server Metadata、PKCE、resource 参数、issuer 校验、scope step-up、refresh、过期 token 和显式重新授权。断言日志与 ACP status 只包含安全状态，不包含 Header/token。

- [ ] **步骤 4：实现 OAuth client 与 transport 注入**

OAuth 状态机发布 `Unauthenticated/Authorizing/Ready/RefreshRequired/Failed`；Host `authorize` 负责用户交互。静态 Bearer、custom Header 和 OAuth 保持互斥类型，不在请求路径拼接可泄露字符串。

- [ ] **步骤 5：运行认证和泄露检查**

```bash
rtk cargo test -p store --test secret
rtk cargo test -p mcp --test oauth
rtk cargo clippy -p store -p mcp -p app --all-targets -- -D warnings
```

---

### 任务 10：重构 Kernel 动态 Tool 分区和 MCP Session 所有权

**文件：**

- 修改：`crates/kernel/src/runtime/tool_state.rs`
- 新增：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/lifecycle.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/lib.rs`
- 新增：`crates/kernel/tests/mcp_dynamic_tools.rs`
- 新增：`crates/kernel/tests/mcp_lifecycle.rs`

本任务在完成 Session Snapshot 切换后，同时删除任务 1 暂留的旧 MCP status、descriptor 和字符串结果类型及全部重导出。

**Tool State 结构：**

```rust
pub struct SessionToolSnapshot {
    builtins: ToolRegistry,
    extensions: ToolRegistry,
    mcp: ToolRegistry,
    active: BTreeSet<String>,
    disabled_preferences: BTreeSet<String>,
    revision: u64,
}
```

- [ ] **步骤 1：先写动态 Tool 投影失败测试**

覆盖 MCP Tool 新增、修改、删除；新 Tool 默认启用；用户禁用偏好在 Tool 消失/恢复后保持；MCP 变化不覆盖 builtin/extension 分区；Provider 请求开始后继续使用原快照，下一次请求使用新快照；已开始调用持有旧 `Arc` 可完成。

- [ ] **步骤 2：先写 Session 生命周期失败测试**

验证 create/resume/fork 都构造独立 `McpSession`；close/delete/shutdown 都等待 MCP async shutdown；显式 reconnect 取消对应在途调用；不存在 watcher、Task、HTTP 请求或 stdio 子进程泄漏。

- [ ] **步骤 3：实现分区 Tool State 与 MCP projection task**

`SessionToolState` 提供 `replace_mcp_partition(snapshot)`，在一次写锁内构造并发布不可变快照。`runtime/mcp.rs` 订阅 `McpSessionEvent`，读取最新 Session Snapshot 后更新 Tool 分区；`session.rs` 只负责装配，不承载同步细节。

- [ ] **步骤 4：替换不可变 `mcp_servers` 字段**

`SessionRuntime` 保存 `Arc<McpSession>` 和 projection task handle；`Kernel::mcp_status` 调用 `McpSession::snapshot()`。应用关闭路径显式 await `shutdown`，不依赖 Drop。

- [ ] **步骤 5：运行 Kernel 动态与生命周期测试**

```bash
rtk cargo test -p kernel --test mcp_dynamic_tools
rtk cargo test -p kernel --test mcp_lifecycle
rtk cargo clippy -p kernel -p mcp --all-targets -- -D warnings
```

---

### 任务 11：接入 MCP Prompts、Resources 和 Completion

**文件：**

- 新增：`crates/prompt/src/mcp.rs`
- 修改：`crates/prompt/src/session.rs`
- 修改：`crates/prompt/src/template/catalog.rs`
- 修改：`crates/prompt/src/lib.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/src/runtime/prompt.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 新增：`crates/prompt/tests/mcp.rs`
- 新增：`crates/kernel/tests/mcp_resources.rs`

- [ ] **步骤 1：先写 Prompt Catalog 组合失败测试**

验证本地 Prompt 与 MCP Prompt 并存、本地名称不可被覆盖、UI 显示 `server:name` 但内部使用 `McpPromptRef`、动态变化只影响当前 Catalog、`prompts/get` 的多消息结果进入正常输入展开与持久化路径。

- [ ] **步骤 2：先写 Resource/Completion 失败测试**

覆盖 Resource 和 Template 列表、原 URI 保留、按需 read、MIME/来源无损、Prompt/Resource 参数 Completion、调用失败不删除 Catalog 项。验证未显式请求时不注入 System Prompt 或 Turn。

- [ ] **步骤 3：实现语义组合层**

`PromptSession` 持有本地不可变目录和 MCP Snapshot provider，不复制第二份 MCP Catalog。`McpPromptCatalogView` 只做引用/展示组合；Resource 保持独立 Kernel API。实际读取或展开后转换为公共 `AgentMessage`/`ContentBlock`，再由现有 Store 持久化。

- [ ] **步骤 4：运行 Prompt/Resource 检查**

```bash
rtk cargo test -p prompt --test mcp
rtk cargo test -p kernel --test mcp_resources
rtk cargo clippy -p prompt -p kernel --all-targets -- -D warnings
```

---

### 任务 12：扩展 ACP 2.0 方法、消息和实时状态

**文件：**

- 修改：`crates/protocol/src/identity.rs`
- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/src/mapping.rs`
- 新增：`crates/acp/tests/mcp_extension.rs`
- 修改：`crates/acp/tests/replay.rs`

**新增 ACP 扩展方法：**

```text
_{namespace}/mcp/status
_{namespace}/mcp/reconnect
_{namespace}/mcp/prompt/list
_{namespace}/mcp/prompt/get
_{namespace}/mcp/resource/list
_{namespace}/mcp/resource/read
_{namespace}/mcp/completion
_{namespace}/mcp/oauth/continue
```

- [ ] **步骤 1：先写方法注册和参数失败测试**

验证所有方法由 `AcpExtensionMethod` 集中生成和解析，参数直接复用 `protocol` 类型，未知字段拒绝，ACP dispatcher 不重复定义 MCP 状态结构体。`status` 返回最新 Snapshot 而非 Session 启动快照。

- [ ] **步骤 2：先写 Turn 消息与回放失败测试**

验证 Tool、Prompt、Resource、Sampling、Elicitation、MRTR 和 Task 相关更新都有 TurnId、TraceId、字符串毫秒时间戳和关联 ID；批量回放顺序使用持久化 sequence。后台 capability 变化只通过 status/revision 查询，不伪造无 TurnId Transcript 消息。

- [ ] **步骤 3：实现 ACP dispatcher 和扩展内容映射**

扩展 `AcpExtensionMethod`，按请求类型调用 Kernel。ACP 原生无对应内容块或状态时使用 ACP 2.0 产品命名空间扩展类型，完整保存公共 payload；所有 ACP 请求日志打印脱敏参数。

- [ ] **步骤 4：运行 ACP 集成测试**

```bash
rtk cargo test -p acp --test mcp_extension
rtk cargo test -p acp --test replay
rtk cargo clippy -p acp -p protocol --all-targets -- -D warnings
```

---

### 任务 13：升级现代亮色 WebUI MCP 面板

**文件：**

- 修改：`web/src/acp/extensions.ts`
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/features/mcp/McpPanel.tsx`
- 新增：`web/src/features/mcp/McpServerCard.tsx`
- 新增：`web/src/features/mcp/McpCatalog.tsx`
- 新增：`web/src/features/mcp/McpOAuthAction.tsx`
- 修改：`web/src/theme/workbench.css`

- [ ] **步骤 1：更新前端唯一类型投影**

增加九种状态、协议标签、transport、implementation、revision、Tools/Prompts/Resources/Templates 数量、failure stage、安全错误和 OAuth 状态。禁止前端自行推断协议或维护独立 Catalog 真相。

- [ ] **步骤 2：实现 MCP 面板逻辑拆分**

`McpPanel` 只负责布局；`McpServerCard` 展示 `Legacy · 2025-11-25` 或 `Modern · 2026-07-28`、状态和操作；`McpCatalog` 展示目录；`McpOAuthAction` 处理继续授权。显式 reconnect 显示进行态并在成功后重新查询 Snapshot。

- [ ] **步骤 3：实现实时 revision 更新**

Workspace controller 在 Session 切换、MCP 面板打开、reconnect/OAuth 完成以及后端 revision 通知后读取 status；旧 Session 的异步响应不得覆盖当前 Session。动态 Tool/Prompt/Resource 更新保持服务端顺序。

- [ ] **步骤 4：运行前端检查**

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
```

前端不新增单元测试；交互在任务 15 使用真实后端验证。

---

### 任务 14：实现幂等 shutdown、stdio 退出升级与诊断日志

**文件：**

- 修改：`crates/mcp/src/session.rs`
- 修改：`crates/mcp/src/server.rs`
- 修改：`crates/mcp/src/transport.rs`
- 修改：`crates/mcp/src/task.rs`
- 修改：`crates/kernel/src/runtime/lifecycle.rs`
- 新增：`crates/mcp/tests/shutdown.rs`
- 新增：`crates/kernel/tests/mcp_shutdown.rs`

- [ ] **步骤 1：先写资源清理失败测试**

验证 shutdown 幂等、拒绝新调用/重连、并发关闭 Server、取消 watcher/MRTR/Task/请求；stdio 先关闭 stdin 并等待，再 TERM，最后 KILL；HTTP 关闭活动请求流。测试结束后确认子进程和 Tokio task 均退出。

- [ ] **步骤 2：实现可等待 shutdown coordinator**

`McpSessionShutdown` 统一管理状态和 JoinHandle；`Drop` 只触发 cancellation 作为最后防线。显式 reconnect 复用单 Server shutdown 流程，避免两套资源清理逻辑。

- [ ] **步骤 3：增加有意义的诊断日志**

仅记录 bootstrap、协议协商/不匹配、状态迁移、同步 revision、reconnect、OAuth 状态、shutdown 升级和调用错误/timeout/cancel。Turn 内沿用 trace span；后台日志包含 SessionId 和 ServerId，使用格式化字符串，不记录敏感 payload。

- [ ] **步骤 4：运行清理和日志检查**

```bash
rtk cargo test -p mcp --test shutdown
rtk cargo test -p kernel --test mcp_shutdown
rtk cargo clippy -p mcp -p kernel --all-targets -- -D warnings
```

---

### 任务 15：标准 Server 互操作与真实 WebUI 端到端验证

**文件：**

- 新增：`crates/mcp/tests/interop_legacy.rs`
- 新增：`crates/mcp/tests/interop_modern.rs`
- 修改：`claw.toml`（只保留可运行的本地测试配置，敏感值使用环境变量）

- [ ] **步骤 1：运行 Rust 全量质量门**

```bash
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace
```

- [ ] **步骤 2：运行两个协议的真实互操作测试**

至少启动两个不同的真实标准 MCP Server，固定最低验收矩阵为：Legacy Server 使用 `2025-11-25 + stdio`，Modern Server 使用 `2026-07-28 + Streamable HTTP`。额外协议/transport 组合可以增加，但不能用同一 fixture 代替这两个真实 Server。验证完整分页、动态能力、Tool、Prompt、Resource、Completion、Host、MRTR、Tasks、显式 reconnect 和 shutdown。协议配置错误必须失败且不回退。

```bash
rtk cargo test -p mcp --test interop_legacy -- --ignored --nocapture
rtk cargo test -p mcp --test interop_modern -- --ignored --nocapture
```

- [ ] **步骤 3：启动正式 app 后端和真实 Provider**

使用修复后的 `claw.toml`、真实 Provider 和标准 MCP Server 启动 app；不得使用 fixture backend、mock ACP server 或 WebUI fixture。确认日志包含文件行号、Session/Turn trace 关联和脱敏 ACP 请求参数。

- [ ] **步骤 4：运行真实浏览器 E2E**

使用项目现有 app 启动方式和应用内浏览器完成：创建 Session、观察两个协议标签和状态、调用动态 MCP Tool、显示 thinking、获取 Prompt、读取 Resource、使用 Completion、完成 Elicitation/MRTR、观察 Task progress、触发显式 reconnect、刷新页面并回放消息、fork Session 后确认独立连接、删除 Session 后确认 MCP shutdown。项目当前没有前端 E2E harness，因此不新增只为测试存在的 fixture 或 mock；保留浏览器截图、后端日志和标准 Server 请求日志作为验证证据。

- [ ] **步骤 5：最终代码组织审查**

检查所有 Rust 正文文件；排除 provider 后，对过长文件重新判断独立语义。重点确认：`kernel/runtime/session.rs` 不含 MCP watcher/状态机细节；`mcp/server.rs` 不混入 OAuth/内容转换；没有重复公共结构体、散落协议字符串、单调用短 helper、生产测试支撑代码或无意义日志。

- [ ] **步骤 6：记录验证证据并等待用户验收**

汇总实际执行命令、通过结果、真实 Server 版本、浏览器操作和剩余限制。未经用户另行明确授权，不创建 commit 或 push。

---

## 规格覆盖自检

- [ ] 精确支持 `2025-11-25` Legacy 和 `2026-07-28` Modern，缺失/未知值 fatal，禁止自动探测和回退。
- [ ] stdio 与 Streamable HTTP 都有真实互操作覆盖；不实现旧 HTTP+SSE 双端点。
- [ ] Tools、Prompts、Resources、Templates、Completion 全分页发现并动态同步，原子 Snapshot 保留 revision。
- [ ] Legacy list_changed/resource subscribe 与 Modern subscriptions/cache hints 都映射为统一内部事件。
- [ ] Kernel Tool 分区使动态变化从下一次 Provider 请求生效，保留用户禁用偏好和在途调用语义。
- [ ] 内容支持 Text、Image、Audio、Embedded Resource、Resource Link、Structured Content，不字符串化降级。
- [ ] Sampling、Elicitation、Roots、MRTR、Tasks get/update/cancel 使用类型化 Host 边界并继承 Turn/Trace。
- [ ] OAuth 支持 metadata、PKCE、resource、issuer、scope step-up、refresh；Secret 不进入日志、ACP 或 JSONL。
- [ ] 单 Server 启动/运行错误隔离；同步失败保留 last-good Snapshot 并进入 Degraded；仅显式 reconnect。
- [ ] Session create/resume/fork 独立连接；shutdown 可等待且无 watcher、Task、请求或子进程泄漏。
- [ ] ACP 复用 protocol 唯一类型；Turn 消息带 TurnId、TraceId、字符串毫秒时间戳；后台变化不伪造消息。
- [ ] WebUI 展示协议、transport、状态、implementation、数量/revision、错误、OAuth 和 reconnect，且使用正式后端 E2E。
- [ ] 不实现 MCP Apps/UI Extension、自动 Prompt/Resource 注入、配置热更新、ACP Client FS/Terminal callback 或 Tasks list。

## 执行检查点

每完成一个任务都必须：

1. 展示先失败后通过的目标测试证据。
2. 运行该任务列出的 `fmt/check/clippy`。
3. 检查 `rtk git diff --check` 和 `rtk git status --short`，确保不覆盖用户现有 `Cargo.lock` 修改。
4. 对照本计划和设计规格确认没有新增兼容层或范围外能力。
5. 不创建 commit；只有用户明确要求时才单独执行提交规则。

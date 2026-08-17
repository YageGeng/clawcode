# MCP Host 模块设计

## 1. 目标

本阶段重构项目的 MCP 模块，使其成为完整、类型化、Session 隔离的 MCP Host，而不是只支持 `tools/list` 和 `tools/call` 的薄适配器。

完成后，系统应能够：

- 由配置显式选择 MCP `2025-11-25` 或 `2026-07-28`，不探测、不自动回退。
- 为每个 Agent Session 创建独立 MCP Host、连接、stdio 子进程和动态能力目录。
- 管理 Server bootstrap、协议生命周期、能力发现、动态同步、重连和关闭。
- 将 MCP Tools、Prompts、Resources、Resource Templates 和 Completion 分别接入现有语义模块。
- 支持 Legacy Host 请求以及 Modern MRTR、Subscriptions、缓存提示和 Tasks 扩展。
- 隔离单个 Server 的运行时故障，同时保留结构化状态、日志和 WebUI 诊断。
- 保留 MCP 返回内容的文本、图片、音频、嵌入资源和结构化数据语义。

## 2. 设计基准

### 2.1 Pi 边界

Pi 没有内置 MCP 子系统。Pi 将 MCP 定位为 Extension 可提供的外部能力：Extension 负责连接和发现，再把远端工具注册为统一 `ToolDefinition`。本设计沿用这一核心边界，但不把 MCP 本身实现成 Extension：

- MCP 协议、transport、认证和连接生命周期由独立 `mcp` crate 管理。
- MCP Tool 必须转换为项目公共 `AgentTool`，不能绕开 `ToolRegistry`。
- MCP Prompt 和 Resource 必须进入对应语义目录，不能全部伪装成 Tool。
- Kernel 只消费协议无关的 Factory、Snapshot、Event 和 Host trait。

### 2.2 MCP 规范

第一版同时支持两个明确的协议修订：

- `2025-11-25`：Legacy 生命周期，使用 `initialize`、版本与能力协商、`notifications/initialized`。
- `2026-07-28`：Modern 生命周期，使用 `server/discover`、每请求协议元数据、无协议 Session 的 HTTP 请求模型。

两种修订必须通过配置精确选择。服务端不支持配置版本时，连接失败；不得切换协议继续运行。

### 2.3 Rust SDK

实现以 workspace 中的官方 Rust SDK `rmcp 3.x` 为协议引擎。项目负责：

- 选择精确生命周期模式。
- 验证协商结果与配置一致。
- 管理 Session、Server、Catalog、Kernel 投影和应用状态。
- 将 rmcp 类型转换为 `protocol` 中稳定的项目公共类型。

项目不重新实现 JSON-RPC、MCP wire schema 或 HTTP framing。

## 3. 已确认的核心决策

1. 采用类型化 Session MCP Host 方案。
2. 配置使用精确协议版本字符串，不使用 `auto`、`legacy` 或 `modern` 模糊值。
3. 不允许自动协议探测或回退。
4. Tools、Prompts、Resources 和 Completion 按语义分别接入。
5. 每个 Agent Session 独立持有一套 MCP 连接和动态状态。
6. 配置错误阻止应用启动；单 Server 运行时错误被隔离。
7. 支持显式 reconnect，不进行后台自动重连。
8. 不实现配置热更新。
9. 不实现 MCP Apps 或其他 Server 驱动的 WebUI Extension Point。
10. 不支持已经淘汰的 `2024-11-05` 双端点 HTTP+SSE transport。

## 4. 范围

### 4.1 第一版包含

- stdio 和 Streamable HTTP transport。
- 精确协议版本选择与验证。
- Legacy initialize 生命周期。
- Modern discover 生命周期和每请求元数据。
- Tools list、call、分页、动态列表变化、取消和进度。
- Prompts list、get、分页和动态列表变化。
- Resources list、templates、read、分页、订阅和更新。
- Completion。
- Legacy sampling、elicitation、roots 和 logging 兼容。
- Modern MRTR、transport-neutral subscriptions 和列表缓存提示。
- MCP Tasks 标准扩展的协商、查询、更新和取消。
- HTTP OAuth 标准授权、token 刷新和安全持久化。
- Session 级状态查询、显式重连和幂等关闭。
- ACP 2.0 扩展方法和 WebUI 状态展示。
- Rust 集成测试和真实后端 WebUI 端到端验证。

### 4.2 第一版不包含

- 自动协议回退。
- 自动重连或无限重试。
- MCP Server 配置热更新。
- `2024-11-05` 双端点 HTTP+SSE。
- MCP Apps 和 Server 自定义 WebUI。
- 将 MCP Prompt、Resource 自动注入每个模型上下文。
- 在 Session JSONL 中持久化活动连接、能力缓存或 OAuth 密钥。

## 5. 总体架构

### 5.1 依赖方向

```text
                    protocol
               ┌────────┼────────┐
               ↓        ↓        ↓
             config    tools    prompt
               └────────┬────────┘
                        ↓
                       mcp
                        ↓
                      kernel
                        ↓
                       acp
                        ↓
                       app
                        ↓
                      webui
```

实际 Cargo 依赖必须保持无环。若 Prompt 动态投影由 Kernel 完成，则 `mcp` 只依赖 `protocol` 和 `tools`，不直接依赖 `prompt`；上图表达领域数据流，不强制每条箭头成为直接 Cargo 依赖。

### 5.2 所有权

```text
Application
  └─ SessionMcpFactory
       ├─ validated server configs
       ├─ McpConnector
       └─ credential storage

Kernel SessionRuntime
  └─ McpSession
       ├─ McpServerRuntime[filesystem]
       ├─ McpServerRuntime[context7]
       ├─ atomic McpSessionSnapshot
       └─ capability event stream
```

- `SessionMcpFactory` 是应用生命周期对象，只保存不可变配置和依赖。
- `McpSession` 由一个 Agent Session 独占。
- `McpServerRuntime` 由一个 `McpSession` 独占。
- stdio 子进程、HTTP client、watcher 和在途调用都由对应 `McpServerRuntime` 管理。
- Kernel 关闭或替换 Session 时，必须显式等待 `McpSession::shutdown()`。

## 6. mcp crate 模块组织

建议目录如下：

```text
crates/mcp/src/
├── lib.rs
├── error.rs
├── factory.rs
├── session.rs
├── client.rs
├── transport.rs
├── catalog.rs
├── tool.rs
├── host.rs
└── server/
    ├── mod.rs
    ├── bootstrap.rs
    ├── lifecycle.rs
    └── sync.rs
```

职责如下：

- `lib.rs`：只重导出稳定公共接口。
- `error.rs`：类型化配置、连接、协议、发现、调用、认证和关闭错误。
- `factory.rs`：`McpFactory` 和生产 `SessionMcpFactory`。
- `session.rs`：多 Server 编排、Session Snapshot、订阅、重连和关闭。
- `client.rs`：协议无关 `McpClient` 接口与 rmcp 适配。
- `transport.rs`：stdio、Streamable HTTP、认证头和 transport 关闭。
- `catalog.rs`：单 Server 和 Session 能力 Snapshot 聚合。
- `tool.rs`：MCP Tool 到公共 `AgentTool` 的适配。
- `host.rs`：Kernel 实现的 `McpHost` 接口。
- `server/bootstrap.rs`：单 Server 启动事务。
- `server/lifecycle.rs`：Server Runtime 状态和精确协议生命周期。
- `server/sync.rs`：Legacy 通知、Modern Subscription 和能力刷新。

模块按领域语义拆分，不为单次调用的小逻辑创建大量 helper 文件。只接收一个项目自定义类型的行为应优先实现为关联方法或标准 Trait。

## 7. 配置

### 7.1 协议字段

每个 Server 必须显式配置精确协议版本：

```toml
[[mcp_servers]]
name = "filesystem"
enabled = true
protocol = "2025-11-25"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]

[[mcp_servers]]
name = "context7"
enabled = true
protocol = "2026-07-28"
url = "https://mcp.context7.com/mcp"
```

`protocol` 不提供隐式默认值。旧配置缺少字段时返回明确配置错误，避免用户误以为系统自动选择了协议。

### 7.2 验证

应用启动时验证：

- Server 名称非空、全局唯一，并满足可用于 Tool 名称空间的受限字符集。
- `protocol` 只能是已支持的精确版本。
- stdio 只能配置 command、args 和 env。
- Streamable HTTP 只能配置 URL、headers、bearer token 和 OAuth。
- 一个 Server 只能配置一个 transport。
- OAuth 只能用于 HTTP。
- 启动、调用、关闭和认证超时必须大于零并有合理上限。
- 认证配置不能同时声明相互冲突的静态 token 和 OAuth 流程。

配置错误属于 fatal error，阻止应用构造 Kernel。

### 7.3 配置与运行时分离

`config::McpServerConfig` 只负责 TOML 反序列化。`RuntimeMcpServer::try_from` 完成 parse-don't-validate 转换，把字符串和可选字段转为：

- `McpProtocolVersion`
- `McpTransportConfig`
- `McpAuthenticationConfig`
- 非零 timeout 值

Kernel 和连接代码只接收已验证类型，不重复检查 TOML 字段组合。

## 8. protocol 公共类型

跨 crate 使用的 MCP 类型只允许在 `protocol` 定义。其他 crate 不得创建同义镜像结构体。

### 8.1 协议与身份

- `McpProtocolVersion`
- `McpProtocolEra`
- `McpServerId`
- `McpToolRef`
- `McpPromptRef`
- `McpResourceRef`

`McpProtocolEra` 由 `McpProtocolVersion` 的关联方法推导，不在状态中重复保存。引用类型保留 `server_id` 和远端原始身份，避免通过字符串拆分恢复路由信息。

### 8.2 状态

`McpServerState` 使用枚举表达：

```text
Disabled
Starting
Negotiating
Discovering
Ready
Degraded
Failed
Stopping
Stopped
```

状态详情由 `McpServerStatus` 保存，包括：

- Server 身份和显示名。
- 配置协议版本与推导出的 era。
- transport 类型。
- 当前状态。
- Server implementation 信息。
- 协商 capabilities。
- capability revision。
- 最近成功同步时间。
- 类型化失败阶段、错误代码和安全消息。

所有对外序列化时间继续复用 `protocol::TimestampMs`，以十进制字符串保存毫秒时间戳，不能引入 JSON number 时间字段。

禁止通过 `connected`、`failed`、`disabled` 等多个 bool 拼装状态。

### 8.3 能力 Snapshot

- `McpToolDescriptor`
- `McpPromptDescriptor`
- `McpResourceDescriptor`
- `McpResourceTemplateDescriptor`
- `McpServerCapabilities`
- `McpCapabilitySnapshot`
- `McpSessionSnapshot`

Snapshot 必须不可变，并携带单调递增 revision。Server Snapshot 和 Session Snapshot 分别维护 revision，动态消费者不能看到半更新数据。

### 8.4 Host 请求

- `McpHostRequest`
- `McpHostResponse`
- `McpSamplingRequest`
- `McpElicitationRequest`
- `McpRootsRequest`
- `McpMrtrRequest`
- `McpTaskStatus`

这些类型是 mcp 与 kernel 的稳定边界，不直接暴露 rmcp model 类型。

### 8.5 内部事件与 ACP 消息

动态同步使用类型化 `McpSessionEvent`，表达状态变化、能力变化、资源更新、Task 进度和认证需求。该事件首先是 Kernel 内部控制事件，不自动成为 Transcript 消息。

只有与 Agent Turn 相关的 MCP 调用、Host 交互和结果才投影为带 TurnId、字符串毫秒时间戳和 trace_id 的 ACP 消息。后台状态变化通过 Session MCP 状态接口查询，避免制造没有合法 TurnId 的伪 Transcript 消息。

## 9. Factory 与公开接口

### 9.1 McpFactory

```rust
#[async_trait]
pub trait McpFactory: Send + Sync {
    async fn create(
        &self,
        request: McpSessionRequest,
    ) -> Result<McpSession, McpError>;
}
```

`McpSessionRequest` 至少包含 SessionId、cwd、`Arc<dyn McpHost>` 和 Session 关闭 token。

Factory 只在以下情况返回整体错误：

- 请求本身无效。
- 无法建立 Session 级运行时基础设施。
- 无法创建事件通道或其他所有 Server 共享的必要资源。

单 Server 启动失败转换为 `McpServerStatus::Failed`，不让 Factory 整体失败。

### 9.2 McpSession

最小公开接口：

- `snapshot()`：返回当前不可变一致视图。
- `subscribe()`：订阅状态和能力 revision 变化。
- `reconnect(server_id)`：显式重新建立一个 Server。
- `get_prompt(reference, arguments)`：获取 MCP Prompt。
- `read_resource(reference)`：读取 MCP Resource。
- `complete(request)`：执行 MCP Completion。
- `shutdown()`：幂等关闭并等待所有资源退出。

Tool 调用仍通过公共 `AgentTool` 和 `ToolRegistry` 完成，不额外提供绕过 Kernel Hook 的公开调用路径。

## 10. Server bootstrap 与协议生命周期

### 10.1 Session bootstrap

创建 `McpSession` 时：

1. 为每个配置项创建初始状态。
2. Disabled Server 直接进入 `Disabled`。
3. Enabled Server 并发执行独立 bootstrap。
4. 每个 Server 完成后提交完整初始 Snapshot 或 Failed 状态。
5. 所有 Server 启动任务 settle 后，Session 返回可运行状态。

并发启动不能改变配置顺序。状态查询和 WebUI 始终按配置顺序展示 Server。

### 10.2 Legacy `2025-11-25`

严格执行：

```text
transport connect
  → initialize(protocolVersion = 2025-11-25)
  → 验证 response.protocolVersion == 2025-11-25
  → 保存 serverInfo 与 capabilities
  → notifications/initialized
  → initial capability discovery
```

不得先发送 `server/discover`，也不得接受服务端返回其他协议后继续运行。

### 10.3 Modern `2026-07-28`

严格执行：

```text
transport ready
  → server/discover(preferred = 2026-07-28)
  → 验证支持并选中 2026-07-28
  → 保存 serverInfo 与 capabilities
  → initial capability discovery
```

后续每个请求由 rmcp 附带规范要求的 protocol version、client information、client capabilities 和标准 HTTP 路由头。不得回退 initialize。

### 10.4 协议不匹配

协议不匹配进入 `Failed`，错误必须包含：

- 配置协议。
- 服务端返回或声明的协议。
- 失败阶段。
- Server 身份和 transport。

日志和 WebUI 应明确显示“协议不匹配”，不能泛化成“连接失败”。

## 11. 初始能力发现

Ready 前根据协商 capabilities 获取：

- Tools 全部分页。
- Prompts 全部分页。
- Resources 全部分页。
- Resource Templates 全部分页。

Server 未声明某项能力时，对应 Catalog 为空，不发送违规请求。Completion 是调用能力，不需要预取实体列表。

分页必须通过 rmcp 的规范 helper 或显式 cursor 循环获取完整结果。任何一页失败都视为该 Server 初始发现失败，不能发布不完整初始 Catalog。

## 12. 动态能力同步

### 12.1 变化来源

Legacy 处理：

- `notifications/tools/list_changed`
- `notifications/prompts/list_changed`
- `notifications/resources/list_changed`
- Resource updated 通知与订阅

Modern 处理：

- transport-neutral `subscriptions/listen`
- 规范列表缓存提示与 TTL
- 资源和能力变化事件

协议特有消息必须归一化为类型化变化事件，Kernel 不根据方法名字符串区分协议。

### 12.2 刷新事务

收到变化后：

1. 识别受影响能力种类。
2. 重新获取该能力的全部分页。
3. 构造新的不可变 Catalog。
4. 原子替换单 Server Snapshot。
5. 增加 Server revision。
6. 重建并原子替换 Session Snapshot。
7. 增加 Session revision，并通知 Kernel。

不能在共享 Catalog 上逐项原地增删。

### 12.3 同步失败

动态刷新失败时：

- 保留最后一个成功 Snapshot。
- Server 进入 `Degraded`。
- 保存失败能力、失败时间和安全错误。
- 不静默清空现有 Tools、Prompts 或 Resources。
- 不自动重连。

下一次规范变化通知可以再次触发刷新；用户也可以显式 reconnect。

## 13. Kernel 动态投影

### 13.1 Tools

每个 MCP Tool 适配为公共 `AgentTool`。公开名称固定为：

```text
mcp__{server_id}__{remote_tool_name}
```

适配器内部保存结构化 `McpToolRef`，调用时不解析公开名称恢复 Server 和远端名称。

Kernel 的 `SessionToolState` 需要支持原子替换 MCP 动态 Tool 分区：

- Builtin、Extension 和 MCP Tool 保持独立来源分区。
- MCP Snapshot 变化只替换 MCP 分区。
- Provider 每次请求前获取一份一致 Tool Snapshot。
- Provider 请求已经开始后，动态变化从下一次模型请求生效。
- 删除 Tool 后拒绝新调用；已经开始的调用持有旧 `Arc`，允许正常完成，Session shutdown 和显式 reconnect 除外。

新发现 Tool 默认启用。用户显式禁用的 Tool 偏好按完整公开名称保留；Tool 暂时消失后重新出现时恢复此前偏好。

### 13.2 Prompts

MCP Prompt 使用 `McpPromptRef { server_id, remote_name }` 标识。UI 显示 `server:name`，但路由不解析显示字符串。

Prompt 模块组合本地 Prompt Catalog 与 Session MCP Prompt Catalog：

- 不允许 MCP Prompt 覆盖本地 Prompt。
- 用户必须显式选择或调用 MCP Prompt。
- `prompts/get` 返回的消息经过公共消息转换后进入正常输入展开流程。
- Prompt 参数可调用 MCP Completion。
- 动态 Prompt 变化不修改已持久化的历史消息。

### 13.3 Resources

MCP Resource 使用 `McpResourceRef { server_id, remote_uri }`。系统保留服务端原始 URI，不把 URI 重写成 Tool 名称或本地路径。

- Resource Catalog 独立于 ToolRegistry。
- Resource 只在用户、Prompt 或 Agent 明确请求时读取。
- 不自动把全部 Resource 注入 System Prompt 或每个 Turn。
- Resource Templates 保留模板定义和参数 Completion。
- 读取结果转换为公共内容块并保留 MIME 类型、URI 和来源。

### 13.4 Completion

Completion 由 `McpSession` 按结构化 Prompt 或 Resource 引用路由。Completion 失败不删除对应 Prompt 或 Resource，只返回可诊断调用错误。

## 14. Tool 调用与内容映射

### 14.1 调用流程

```text
Provider ToolCall
  → ToolRegistry lookup
  → Extension tool_call hooks
  → McpAgentTool schema validation
  → McpServerRuntime call
  → progress / MRTR / task handling
  → MCP result conversion
  → Extension tool_result hooks
  → persistence + ACP/WebUI
```

MCP 不得绕开现有 Extension Tool Hook、Turn 取消、ToolUpdate、trace_id 和持久化流程。

### 14.2 超时与取消

- 每次调用使用 Server 配置的 request timeout。
- Turn 取消必须传播到 MCP 请求。
- Legacy 超时后发送规范 cancellation notification，然后停止等待。
- Modern HTTP 取消关闭对应请求流；MRTR 和 Task 同时执行对应取消语义。
- progress 可以更新活动超时，但必须保留不可突破的总时限。
- Session shutdown 和显式 reconnect 取消对应 Runtime 的全部在途调用。

### 14.3 内容

MCP 结果转换必须支持：

- Text。
- Image。
- Audio。
- Embedded Resource。
- Resource Link。
- Structured Content。

不能把所有非文本内容 `serde_json::to_string` 后伪装为文本。公共 `ContentBlock` 缺少对应类型时，先扩展 `protocol` 的唯一内容模型，再由 Provider、Store、ACP 和 WebUI 统一映射。

MCP `isError` 是远端工具业务错误，应转换为 `ToolResult.is_error = true`；transport、协议和超时错误转换为类型化 `ToolError::Execution`。

## 15. Host 能力、MRTR 与 Tasks

### 15.1 McpHost

`mcp` crate 定义协议无关 Host trait，Kernel 提供实现：

```text
McpHost
  ├─ sample
  ├─ elicit
  ├─ roots
  └─ authorize
```

trait 请求和结果使用 `protocol` 类型，不暴露 Kernel、ACP 或 WebUI 类型。

### 15.2 Legacy Host 请求

- Sampling 通过现有 Model/Provider 路径执行，继承当前 Turn trace_id，并遵守模型和 token 策略。
- Elicitation 通过 Kernel 产生 Turn 关联的交互请求，由 ACP/WebUI 完成。
- Roots 只返回应用允许的 cwd/root，不提供 ACP Client 文件系统回调。
- Server logging 转换为受控 tracing 日志，不接受 Server 动态改变应用日志过滤配置。

### 15.3 Modern MRTR

当 Tool、Prompt 或 Resource 返回 input-required 结果时：

1. 校验 MRTR 类型和 required client capabilities。
2. 将 input requests 交给 `McpHost`。
3. 保留服务端 opaque `requestState`，不得解析或修改。
4. 收集 Host 响应并重新提交请求。
5. 限制最大轮次、单轮超时和总超时。
6. 用户取消时终止整个 MRTR。

超过轮次或总时限返回明确错误，不能形成无限交互循环。

### 15.4 Tasks 扩展

仅在 Modern Server 声明标准 Tasks 扩展时启用：

- Tool 调用返回 Task handle 后进入 Task 驱动流程。
- 支持 `tasks/get`、`tasks/update` 和 `tasks/cancel`。
- Task progress 映射为 ToolUpdate。
- Task 最终结果保持原 ToolCallId 和 TurnId。
- Session shutdown 和 Turn 取消必须取消未完成 Task。
- 不实现规范已删除的 `tasks/list`。

## 16. 认证与凭据

### 16.1 静态认证

- stdio 环境变量按配置注入子进程。
- HTTP bearer token 从指定环境变量读取。
- 自定义 HTTP header 在启动时验证名称和值。
- 认证 header 和 token 永不写入日志、状态响应或 Transcript。

### 16.2 OAuth

第一版 HTTP OAuth 支持：

- Protected Resource Metadata。
- Authorization Server Metadata。
- PKCE。
- 规范 resource 参数。
- issuer 校验。
- scope step-up。
- access token 刷新。
- 显式认证失败和重新授权状态。

OAuth token 通过 `store` 提供的安全 Secret Store 接口持久化。Session JSONL 只保存认证交互产生的普通用户可见消息，不保存 token、refresh token、client secret 或 authorization code。

凭据以 ServerId 和授权主体建立稳定键；文件实现必须使用限制权限并支持原子替换。MCP 模块依赖抽象 Secret Store，不自行散落文件写入逻辑。

## 17. 重连与关闭

### 17.1 显式 reconnect

`reconnect(server_id)` 执行：

1. 将 Server 状态切换为 Stopping。
2. 从 Session 动态投影撤下该 Server 能力。
3. 取消旧 Runtime watcher、Task 和在途调用。
4. 关闭 transport 并等待退出。
5. 使用原配置和同一精确协议重新 bootstrap。
6. 成功后发布全新 Snapshot，失败后进入 Failed。

重连期间不保留旧能力继续接受新调用，避免请求落到正在关闭的 Runtime。

### 17.2 shutdown

`McpSession::shutdown()` 必须幂等：

1. 停止接受新调用和重连。
2. 并发关闭所有 Server Runtime。
3. 取消 watcher、MRTR、Task 和在途请求。
4. Legacy stdio 先关闭输入并等待进程退出。
5. 超时后发送 TERM，再超时后发送 KILL。
6. HTTP 关闭活动请求流和客户端资源。
7. 等待所有后台任务结束。
8. 发布最终 Stopped 状态。

Rust `Drop` 只能作为最后防线，不能替代可等待的 async shutdown。

## 18. 错误策略

### 18.1 Fatal 错误

- 未知协议版本。
- 重复或非法 ServerId。
- transport 配置歧义。
- 无效 timeout。
- 冲突认证配置。
- Secret Store 无法构造。

Fatal 错误发生在应用启动和 Runtime 配置转换阶段。

### 18.2 Server 隔离错误

- 子进程启动失败。
- HTTP 连接失败。
- 协议不匹配。
- initialize 或 discover 失败。
- 初始能力发现失败。
- OAuth 授权或刷新失败。

这些错误只使对应 Server 进入 Failed；其他 Server 和 Session 继续运行。

### 18.3 Degraded 错误

- 动态列表刷新失败。
- Subscription 临时终止。
- Cache revalidation 失败但存在最后成功 Snapshot。

Degraded 保留最后成功能力。系统不自动重连；显式 reconnect 可恢复。

### 18.4 调用错误

- Tool 不存在或已经撤下。
- 参数不满足 schema。
- Completion、Prompt 或 Resource 请求失败。
- timeout、取消、MRTR 或 Task 失败。

错误类型必须携带 ServerId、操作、失败阶段和安全消息。调用方不得解析展示文本判断错误种类。

## 19. 持久化与恢复

### 19.1 持久化

- MCP ToolCall 和 ToolResult 使用现有消息类型持久化。
- MCP Prompt 展开后的消息使用正常用户输入历史持久化。
- MCP Resource 被实际读取并加入上下文时，持久化转换后的内容与来源引用。
- MRTR/Elicitation 的用户交互使用正常 Turn 消息持久化。
- OAuth token 只进入 Secret Store。

### 19.2 不持久化

- 活动 transport。
- Server Runtime 状态机对象。
- watcher 和 Task handle。
- Tool、Prompt、Resource Catalog 缓存。
- list cache TTL。
- 连接失败状态。

### 19.3 Session resume 和 fork

恢复或 fork Session 时，根据当前配置重新创建独立 `McpSession` 并重新发现能力。历史消息保持原内容，不根据当前 Server Catalog 重写。

WebUI 回放 MCP Tool 事件依赖已持久化的标准消息；当前连接状态通过 MCP Status 接口实时查询，不增加独立 `index.jsonl`。

## 20. ACP 与 WebUI

### 20.1 ACP

ACP 扩展方法至少提供：

- Session MCP status。
- MCP Prompt list/get。
- MCP Resource list/read。
- MCP Completion。
- 显式 reconnect。
- OAuth 授权状态和继续授权入口。

跨协议类型直接复用 `protocol`，`acp` crate 不重复定义同义 MCP 状态结构体。

Turn 内产生的 MCP 消息继续包含：

- TurnId。
- 字符串毫秒时间戳。
- trace_id。
- ToolCallId 或对应交互关联 ID。

后台 capability 变化不伪造 Turn 消息。ACP 客户端通过 status/capability 查询获得最新 Snapshot。

### 20.2 WebUI

MCP 面板展示：

- Server 名称。
- `Legacy · 2025-11-25` 或 `Modern · 2026-07-28`。
- transport。
- Disabled、Starting、Ready、Degraded、Failed、Stopping、Stopped 状态。
- Server implementation 信息。
- Tools、Prompts、Resources 数量和 revision。
- 失败阶段和安全错误。
- OAuth 授权状态。
- 显式 reconnect 操作。

WebUI 不实现 MCP 协议、不自行连接 Server、不缓存第二份能力真相，也不允许 MCP Server 注入 UI Extension。

## 21. 日志与安全

日志只添加在：

- Server bootstrap 开始和结束。
- 协议选择与不匹配。
- 状态转换。
- 动态同步成功、失败和 revision 变化。
- 显式 reconnect。
- OAuth 状态转换。
- Session shutdown 和进程强制终止。
- 调用错误、超时和取消。

日志遵守项目既有规则：

- 使用 `tracing::info!()` 等完整路径。
- 使用可读格式化参数，不使用 tracing event field 风格。
- Turn 内调用继承 trace span。
- 后台事件记录 SessionId 和 ServerId。
- 不记录 token、authorization header、cookie、password、client secret、完整敏感 Tool 参数或结果。
- 不为每个分页项、每个 progress tick 或每个缓存读取打印日志。

HTTP 必须验证 URL、header 和 OAuth issuer。stdio 必须区分协议 stdout 与日志 stderr，Server 输出到 stdout 的非协议文本应形成明确协议错误。

## 22. 测试与验证

测试代码只能位于 crate `tests/` 集成测试目录或精确的 `#[cfg(test)] mod tests`。生产正文不得增加只为测试存在的字段、函数、实现或 cfg 分支。

### 22.1 配置测试

- 两个精确协议值。
- 缺失和未知协议。
- 重复与非法 ServerId。
- stdio/HTTP transport 互斥。
- timeout 和认证组合。

### 22.2 生命周期测试

- Legacy 只发送 initialize 生命周期。
- Modern 只发送 discover 生命周期。
- 两种协议均不自动回退。
- 协议不匹配进入 Failed。
- mixed healthy、failed、disabled Server 故障隔离。
- 多 Server 并发 bootstrap 和稳定展示顺序。

### 22.3 能力测试

- Tools、Prompts、Resources 和 Templates 完整分页。
- Legacy list_changed 和资源订阅。
- Modern subscriptions/listen 和 cache hints。
- 原子 Snapshot revision。
- 动态增加、删除和修改 Tool。
- 刷新失败保留最后成功 Snapshot 并进入 Degraded。
- Tool 用户启用偏好在消失和恢复后保持。

### 22.4 调用测试

- 参数 schema 验证。
- 文本、图片、音频、嵌入资源、资源链接和 structured content。
- Tool progress、取消、timeout 和总时限。
- Prompt get、Resource read 和 Completion。
- Legacy Sampling、Elicitation 和 Roots。
- Modern MRTR 正常、多轮、取消、超时和轮次上限。
- Tasks get、update、cancel 和最终 ToolResult 关联。

### 22.5 生命周期资源测试

- Session 独立连接与状态隔离。
- 显式 reconnect 撤下旧能力并重建同一协议。
- shutdown 幂等。
- stdio 正常退出、TERM 和 KILL 升级。
- watcher、Task 和子进程无泄漏。

### 22.6 集成与端到端

- 使用标准 MCP everything/reference Server 分别验证两个协议。
- 验证真实 stdio 和 Streamable HTTP transport。
- 运行 Rust 格式化、静态检查和相关集成测试。
- 运行 Web 前端检查。
- 启动生产 app 后端、真实配置 Provider 和标准 MCP Server。
- 在 WebUI 验证协议标签、动态 Tool、状态变化、Prompt/Resource、OAuth 和显式 reconnect。

WebUI 不使用 fixture 后端。后端遵循 TDD；前端不要求 TDD 或单元测试。

## 23. 迁移原则

现有 MCP 模块可以直接重构，不保留内部 API 兼容层。保留的只有用户已确认的行为：

- TOML 配置入口。
- stdio 和 Streamable HTTP。
- Session 级隔离。
- Tool 名称空间。
- 单 Server 故障隔离。

现有 `McpConnection`、`McpFactory`、`McpSession` 等类型若不满足新所有权和能力模型，可以直接替换。实现中不得保留第二套重复公共类型，也不得让 Kernel 直接依赖 rmcp。

## 24. 完成标准

只有同时满足以下条件才视为完成：

1. 两个配置协议均按精确生命周期连接，且无自动回退。
2. Tools、Prompts、Resources、Completion 和 Host 能力均通过类型化边界工作。
3. 动态能力变化原子更新 Kernel 投影。
4. 单 Server 故障不影响健康 Server 和 Agent Session。
5. 显式 reconnect 和 Session shutdown 无任务、连接或子进程泄漏。
6. MCP 内容类型不得有损降级为字符串。
7. ACP/WebUI 能明确展示协议、状态、能力和错误。
8. OAuth 凭据不进入 Transcript 或日志。
9. 相关 Rust 集成测试、静态检查和真实 WebUI 端到端验证通过。

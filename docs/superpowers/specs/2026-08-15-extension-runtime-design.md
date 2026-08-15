# Extension 运行时设计

## 1. 目标

本阶段重构 clawcode 的 Extension 子系统，使其具备与 Pi 当前非 UI Extension 模型一致的生命周期接入能力，同时遵守 clawcode 的强类型协议、Factory 注入、Session 隔离和持久化约束。

完成后，Extension 应能够：

- 订阅 Pi 的全部非 UI 生命周期接入点。
- 按接入点返回类型化结果，链式修改输入、上下文、Provider 请求、消息和工具结果。
- 注册 Tools、Commands、Flags 和构建期静态 Provider 声明。
- 在 Session 运行期间动态增删 Tools 与 Commands，并切换 Active Tools。
- 通过受控 Host API 发送消息、追加持久化状态、管理 Session、执行本地命令和控制当前 Run。
- 在普通 Handler 失败时隔离错误，在 `tool_call` Handler 失败时阻止对应工具。
- 通过 ACP 2.0 扩展消息暴露没有原生 ACP 表示的 Extension 消息、错误和注册信息。

本设计不移植 Pi 的 TypeScript 类结构。Rust API 使用类型化 Registrar、Marker 类型和关联类型表达事件与结果关系。

## 2. 行为基准

本设计以本地 Pi 0.84.1 后续提交中的以下实现和最新官方 Extension 文档为行为基准：

- `packages/coding-agent/src/core/extensions/types.ts`
- `packages/coding-agent/src/core/extensions/runner.ts`
- `packages/coding-agent/src/core/extensions/loader.ts`
- `packages/coding-agent/src/core/agent-session.ts`
- `packages/coding-agent/docs/extensions.md`
- `https://pi.dev/docs/latest/extensions`

对 Pi 行为的保留包括：

- Handler 按 Extension 加载顺序和 Extension 内注册顺序执行。
- `context`、`before_agent_start`、`before_provider_request`、`message_end`、`tool_result` 和 `input` 使用链式转换。
- Session 前置事件可以取消操作。
- `tool_call` 可以修改参数或阻止执行，并在 Handler 异常时 fail-safe。
- 普通 Extension Handler 的异常被记录，Agent 继续运行。
- 并行工具先按源码顺序完成预检，再并行执行；更新和完成事件允许交错。
- Extension 自定义 Entry 不进入 LLM 上下文，但参与 Session Tree、Fork 和回放。

## 3. 范围

### 3.1 本阶段包含

- 进程内 Rust Extension Module 与 Factory。
- 类型化 Event Point、Handler、Registrar、Registry 和 Session Runtime。
- 全部非 UI Pi 生命周期事件。
- 构建期 Tools、Commands、Flags 和静态 Provider 声明。
- Session 级动态 Tools、Commands 和 Active Tools。
- 受控 Extension Host API 和只读 Session 快照。
- Extension 自定义消息和自定义 Entry 持久化。
- Extension 错误事件、ACP 映射与批量回放。
- Server 侧 `user_bash`，不依赖 ACP Client Terminal callback。
- Rust 集成测试和真实后端 WebUI 端到端验证。

### 3.2 本阶段不包含

- WASM、动态库、脚本或进程外 Extension 加载。
- TypeScript Extension 兼容层。
- Extension 自动发现、热更新或 reload。
- 运行期 Provider 注册和注销。
- Extension UI Context。
- 快捷键、主题、消息 Renderer、Markdown Transformer 和 Entry Renderer。
- ACP Client 文件系统或 Terminal callback。
- 配置热更新。
- Extension 沙箱和权限声明系统。

动态加载边界由 `ExtensionFactory` 和 `ExtensionModule` 保留。未来增加加载器时，不改变 Kernel 与 Extension Runtime 的接口。

## 4. 总体架构

### 4.1 依赖方向

```text
protocol
   ↑
tools ← extension → store capability traits
             ↑
          kernel
        ↙        ↘
      acp         app
```

- `protocol` 定义所有跨 crate 的 Extension 公共类型。
- `extension` 定义注册、调度、组合和 Host capability 契约，不依赖 Kernel。
- `kernel` 实现 Host capability，并在真实生命周期位置调用 Extension Runtime。
- `tools` 提供 Extension 注册工具时使用的公共 `AgentTool` 接口。
- `store` 持久化最终消息和 Extension Entry，不执行 Extension 逻辑。
- `acp` 只做协议映射，不重新解释 Extension 组合规则。
- `app` 负责组装 Factory，不直接调度 Handler。

### 4.2 生命周期层级

Extension 系统分为三个层级：

1. `ExtensionFactory` 提供构建期静态声明，并能为每个 Session 创建一组全新的、有序的进程内 `ExtensionModule`。
2. 每个 Session 的 Module 通过新的 `ExtensionRegistrar` 注册 Handler 和 Session 初始贡献，注册结束后冻结为该 Session 独占的 `ExtensionRegistry`。
3. Session 使用自己的 Registry 创建 `ExtensionRuntime`，持有 Session 级动态 Tools、Commands、Flags 值和 Extension 状态。

Handler Registry 在 Session 生命周期内不可变。动态 Tools 和 Commands 使用独立的 Session 级注册表，因此不会改变 Handler 的顺序。

### 4.3 Session 隔离

当前全局 `ExtensionPipeline` 移入 `SessionRuntime`。每个 Session 拥有：

- 独立 Extension Runtime。
- 独立动态 Tool Registry。
- 独立动态 Command Registry。
- 独立 Active Tools 集合。
- 独立 Event Bus 订阅和 Extension 状态。

Extension 不能通过全局可变集合影响其他 Session。只有 Factory 和构建期静态声明可以共享；Module、Handler、Registry、Event Bus 订阅和可变状态都必须按 Session 重新创建。

## 5. 类型化 Registrar

### 5.1 Extension Point

每个接入点由零大小 Marker 类型表示，并通过关联类型绑定事件和结果：

```rust
pub trait ExtensionPoint: Send + Sync + 'static {
    type Event: Send + Sync;
    type Output: Send;
    const NAME: &'static str;
}
```

例如 `ContextPoint::Event = ContextEvent`，`ContextPoint::Output = ContextResult`。`TurnStartPoint::Output = ()`。因此观察事件无法返回阻止、注入或修改结果。

### 5.2 Handler

```rust
#[async_trait]
pub trait ExtensionHandler<P: ExtensionPoint>: Send + Sync {
    async fn handle(
        &self,
        event: &P::Event,
        context: &ExtensionContext,
    ) -> Result<P::Output, ExtensionError>;
}
```

Handler 使用具体 Point 的 Event 和 Output。公共 API 不接受 `Any`、通用 JSON Event 或运行时向下转型。

### 5.3 Registrar

Extension Module 使用以下形式注册：

```rust
registrar.on::<ContextPoint>(context_handler)?;
registrar.on::<ToolCallPoint>(permission_handler)?;
```

`RegisterPoint` 由 Extension crate 内的受控 Point 类型实现，将 Handler 写入对应领域的强类型注册项。Registry 内部按 startup、session、agent、provider、model、tool 和 input 分组，不使用 `TypeId` 或 `Any` 容器。

### 5.4 Module 与 Factory

`ExtensionModule` 提供稳定 Descriptor，并在构建期执行同步注册。网络连接、进程、文件监听器和其他长生命周期资源不得在注册阶段启动；它们应在 `session_start` 或首次使用时创建，并在 `session_shutdown` 中幂等释放。

`ExtensionFactory` 提供纯构建期静态声明，并为每个 Session 创建全新的有序 Module。Factory 不得缓存带 Session 状态的 Module 或 Handler。构建失败、重复 ExtensionId、非法静态注册和不可解析的 Provider 声明会阻止 Kernel 启动。

## 6. 公共协议类型

### 6.1 文件边界

`crates/protocol/src/extension/` 按以下语义拆分：

- `mod.rs`：重导出。
- `events/startup.rs`：信任和资源发现。
- `events/session.rs`：Session 生命周期。
- `events/agent.rs`：输入、Agent、Turn、消息和上下文。
- `events/provider.rs`：Provider 请求和响应。
- `events/model.rs`：Model 与 Thinking 变更。
- `events/tool.rs`：Tool 生命周期和 User Bash。
- `result.rs`：类型化 Patch、取消、阻止和替换结果。
- `context.rs`：调用身份和只读快照。
- `registration.rs`：Descriptor、Command、Flag 和静态 Provider 声明。

其他 crate 不重复定义这些结构体。现有通用 `ExtensionEvent`、`ExtensionDirective` 和 `ExtensionEffects` 删除。

### 6.2 调用身份

每次 Handler 调用都携带 `ExtensionInvocation`：

- ExtensionId。
- SessionId。
- 可选 RunId。
- 可选 TurnId。
- 字符串表示的毫秒级 TimestampMs。
- Session 工作目录。

Session 外事件没有 RunId 或 TurnId。所有由 Extension 产生并进入消息流或 ACP 更新的消息仍必须由 Kernel 分配 TurnId，并使用字符串毫秒时间戳。

### 6.3 Extension 消息草稿

Extension 只能创建 `ExtensionMessageDraft`，不能自行指定 MessageId、TurnId、开始时间或结束时间。草稿包含：

- `extension_id`
- `custom_type`
- 内容块
- 是否显示
- 是否参与 LLM Context
- 可选 JSON details

Kernel 将草稿转换为完整 `AgentMessage`，统一生成身份和时间。参与 LLM Context 的 Extension 消息通过明确的 Provider 投影转换；不参与 Context 的消息只用于持久化和客户端展示。

## 7. 接入点

### 7.1 Startup 与资源

- `project_trust`：首个 `yes/no` 决定生效，`undecided` 继续后续 Handler。
- `resources_discover`：累积 Skill 和 Prompt 资源路径。Theme 路径属于 UI 扩展，本阶段排除。

项目资源在服务器端读取，不调用 ACP Client FS callback。

### 7.2 Session

- `session_start`
- `session_info_changed`
- `session_before_switch`
- `session_before_fork`
- `session_before_compact`
- `session_compact`
- `session_before_tree`
- `session_tree`
- `session_shutdown`

所有 `session_before_*` Handler 按顺序执行。任一 Handler 返回取消后停止后续 Handler，原操作不改变 Store 或当前 Session。

`session_before_compact` 可以提供完整自定义 Compaction 结果。`session_before_tree` 可以提供 Branch Summary，并覆盖本次导航的摘要指令和 Label。

### 7.3 Input、Agent、Turn 与消息

- `input`
- `before_agent_start`
- `agent_start`
- `agent_end`
- `agent_settled`
- `turn_start`
- `turn_end`
- `message_start`
- `message_update`
- `message_end`
- `context`

`input` 支持 continue、transform 和 handled。Transform 按 Handler 顺序链式应用；handled 立即结束输入处理，不进入 Skill、Prompt 或 Agent 流程。

`before_agent_start` 可以累积 Extension 消息草稿，并链式替换本次 Run 的 System Prompt。

`context` 接收消息深拷贝并返回本次 Provider 请求的替换消息。修改不直接覆盖 Session 历史。

`message_end` 可以链式替换最终消息，但不允许改变消息角色和身份。替换发生在持久化和 ACP 最终消息事件之前。

### 7.4 Provider 与 Model

- `before_provider_headers`
- `before_provider_request`
- `after_provider_response`
- `model_select`
- `thinking_level_select`

Header Handler 返回类型化 Header Patch；值为字符串表示增加或替换，删除操作使用明确的 Remove 变体，不使用 `null` 表达业务状态。

Provider payload 使用 `serde_json::Value` 表示 Provider 私有结构。Handler 返回替换值后，后续 Handler 看到最新 payload。

Provider 响应事件只暴露状态码和规范化 Header，不暴露认证信息。Model 与 Thinking 事件为观察事件。

### 7.5 Tool 与 User Bash

- `tool_execution_start`
- `tool_call`
- `tool_execution_update`
- `tool_result`
- `tool_execution_end`
- `user_bash`

`tool_call` 接收当前 ToolCall，并可返回参数替换或 Block。替换参数按顺序传给后续 Handler，并在执行前重新通过 Tool 参数 Schema 校验。替换后无效或 Handler 报错时，工具不会执行，并形成错误 ToolResult。

`tool_result` 可以链式修改内容、details、is_error 和 usage。修改发生在 `tool_execution_end`、最终 ToolResult 消息持久化和 ACP 输出之前。

并行 Tool 的 `tool_call` 预检按 Assistant 源码顺序串行执行。预检完成后工具并行执行，update、result 和 execution_end 可按实际完成顺序交错；最终 ToolResult 消息仍按 Assistant 源码顺序持久化。

`user_bash` 允许 Extension 提供服务器端执行操作或完整替代结果，不调用 ACP Client Terminal callback。

### 7.6 Extension Commands

Extension Command 使用独立的类型化 Handler，不再表示为通用 Extension Event。Command Handler 接收字符串参数、结构化 ACP 参数和 `ExtensionCommandContext`。

重名 Command 使用稳定的 ExtensionId 限定调用名。仅当短名称唯一时，ACP 和客户端才可使用短名称；完整限定名始终可用。

## 8. 组合规则

### 8.1 观察事件

所有 Handler 顺序执行，Output 固定为 `()`。普通错误记录后继续后续 Handler。

### 8.2 链式转换

以下事件将前一个 Handler 的结果作为后一个 Handler 的输入：

- input
- before_agent_start System Prompt
- context
- before_provider_headers
- before_provider_request
- message_end
- tool_call 参数
- tool_result

链式转换使用不可变 Patch。Extension 不获得 Kernel 内部对象的可变引用。

### 8.3 首个决定与取消

- project_trust：首个明确 yes/no 生效。
- input：首个 handled 生效。
- tool_call：首个 block 生效。
- session_before_*：首个 cancel 生效。

### 8.4 累积结果

资源路径、before_agent_start 注入消息和诊断信息按 Handler 顺序累积。累积结果保留来源 ExtensionId。

## 9. Extension Context 与 Host API

### 9.1 只读快照

`ExtensionContext` 提供：

- `ExtensionInvocation`
- 当前 Model 和 Thinking 快照
- 可用 Model Catalog 快照
- Session 消息与 Tree 的只读快照
- Context usage
- Pending Queue 摘要
- 当前 Active Tools 和全部可用 Tools
- 当前取消令牌

快照在调用前构建。Extension 不持有 Kernel、SessionStore、ToolRegistry 或内部锁，也不会跨 `await` 持有 Kernel 锁。

### 9.2 Event Context Actions

普通 Handler 可以：

- 判断 Session 是否 idle。
- 判断是否存在 Pending Messages。
- 中止当前 Run。
- 请求优雅关闭。
- 触发 Compaction。
- 获取当前 System Prompt。
- 发送 Extension 消息。
- 发送或排队 User 消息。
- 追加 Extension Entry。
- 设置 Session 名称和 Entry Label。
- 执行服务器本地命令。
- 查询和修改 Active Tools。
- 查询当前 Commands 和 Flags。
- 动态注册或注销当前 Session 的 Tools 和 Commands。
- 从已配置 Model Catalog 中切换当前 Session 的 Model。
- 获取或设置 Thinking Level；设置值必须按 Model 能力裁剪。
- 向 Session 级 Extension Event Bus 发布或订阅 `channel + serde_json::Value` 事件。

不提供运行期 Provider 注册、注销或配置修改。

### 9.3 Command Context Actions

`ExtensionCommandContext` 包含普通 Event Context 的能力，并额外提供：

- `wait_for_idle`
- 创建 Session
- 恢复或切换 Session
- Fork Session
- Session Tree 导航

这些操作不出现在普通 Handler Context 中，避免 Handler 在持有 Run Gate 的生命周期位置等待自身释放。

不提供 reload。未来 reload 必须销毁旧 Session Extension Runtime、使旧 Context 失效，再创建新 Runtime。

## 10. 注册与冲突

### 10.1 Extension 身份

ExtensionId 是协议层强类型字符串。重复 ExtensionId 在构建期报错。Descriptor 至少包含 ID、显示名称和版本。

产品名称继续来自公共 Product Identity 常量，不在 Extension 代码中重复硬编码项目名称。

### 10.2 Tools

- 同一 Module 在注册期重复 Tool 名称时，后一次定义替换该 Module 的前一次定义并产生诊断。
- Extension Tool 可以覆盖同名 Built-in Tool，并产生可观察诊断。
- 不同 Extension 注册同名 Tool 时，Factory 顺序更靠前的 Extension 生效，后续定义保留诊断但不进入有效 Registry。
- 动态注册只影响当前 Session。
- 每个 Turn 在开始时固定 Tool 快照；Turn 运行中发生的动态变化从下一 Turn 生效。

### 10.3 Commands 与 Flags

- Command 使用 ExtensionId 限定名消除歧义。
- 同一 Extension 内重复 Command 名称属于注册错误。
- Flag 名称全局唯一；重复 Flag 属于构建错误。
- Flag 值在 Session Runtime 创建前解析，运行期间只读。

### 10.4 静态 Provider

Extension 可以在构建期声明 Provider 配置。KernelFactory 必须先创建 Extension Registry，再把静态 Provider 声明交给 ModelFactory，最后创建 Model。

Provider 冲突、认证和模型配置校验由保留的 Provider/Config 模块负责。Session 启动后不存在 Provider 注册或注销入口。

静态 Provider 声明只扩展构建期 Model Catalog。Model 切换只能选择该 Catalog 中已经完成配置和认证校验的 Model，不能通过 Extension 临时创建 Provider 或 Model。

## 11. 动态 Tool 与 Command

动态注册表使用版本化不可变快照：

- 写操作在短生命周期写锁内创建新快照。
- Turn 开始时读取一次 Tool 快照。
- System Prompt、Provider Tool Definitions、Tool 参数校验和 Tool 执行使用同一个快照版本。
- Command 调用在分发开始时读取一次 Command 快照。
- Handler 执行期间不持有 Registry 锁。

注销 Active Tool 时，同时从 Active Tools 集合移除。设置 Active Tools 时，未知名称返回类型化错误，不静默忽略。

## 12. 持久化

### 12.1 Extension Entry

Store 增加 Pi v4 风格的 Extension 自定义 Entry：

- ExtensionId
- custom_type
- JSON data
- EntryId
- parent EntryId
- 字符串毫秒时间戳

Entry 追加到当前 Session 分支，不进入 LLM Context。Fork 和 Tree Navigation 按现有 parent 链自然继承；回放按持久化顺序恢复，不增加 `index.jsonl`。

### 12.2 Extension 消息

Extension 消息属于完整 AgentMessage，必须持久化：

- MessageId
- TurnId
- 字符串毫秒创建、首内容和结束时间
- ExtensionId 和 custom_type
- 内容、display、context behavior 和 details

流式 Extension 消息的首内容时间为第一段内容到达时间，结束时间为最后一段内容完成时间。非流式消息的三个时间使用同一个毫秒时间值。

### 12.3 错误与诊断

Extension Handler 错误以结构化 Agent Event 持久关联当前 Session、可选 Run/Turn 和 ExtensionId。错误不得包含 Provider 密钥、Authorization Header 或未脱敏配置。

## 13. 错误处理

### 13.1 构建期错误

以下错误阻止 Kernel 启动：

- Factory 创建失败。
- 重复 ExtensionId。
- 非法 Handler 或静态注册。
- 重复 Flag。
- 静态 Provider 校验失败。

### 13.2 普通 Handler 错误

普通 Handler 错误转换为结构化 `ExtensionHandlerFailed` 事件，包含 ExtensionId、Point 名称和脱敏消息。后续 Handler 和 Agent 继续执行。

### 13.3 Tool Call 错误

`tool_call` Handler 错误、参数转换错误和转换后 Schema 校验错误都阻止对应工具。Kernel 生成 `is_error = true` 的 ToolResult，并允许 Agent 根据结果继续。

Extension Tool 自身执行失败同样转换为错误 ToolResult，不使整个 Kernel Run 直接失败。

### 13.4 Host Action 错误

Host Action 返回类型化错误。Extension 可以处理错误；未处理并向 Handler 返回时，按该 Point 的错误策略处理。旧 Session Runtime 销毁后，所有 Context Action 返回 Stale Runtime 错误。

## 14. ACP 2.0 映射

ACP 有原生表示时继续使用原生消息。以下信息通过项目命名空间下的 ACP 2.0 Extension Update 表达：

- Extension 自定义消息和 details。
- Extension Handler 错误。
- Extension 注册的 Command、Flag 和 Tool 来源信息。
- Event Bus 不对客户端公开，除非 Extension 主动发送消息。

每条 ACP 消息携带 TurnId 和字符串毫秒时间戳。Session 级事件若没有业务 Turn，Kernel 分配独立的系统 TurnId，不复用其他 Turn。

消息回放使用 ACP 2.0 批量发送能力，按 Store 顺序输出。流式 delta 不在回放时重新模拟；回放发送已持久化的最终消息、最终 ToolResult、Extension Entry 可见投影和错误事件。

## 15. 并发与取消

- 同一 Point 的 Handler 严格串行。
- 不同并行 Tool 的 update/result/end 调用允许并发。
- Handler 必须是 `Send + Sync`；Extension 内部可变状态由实现自行同步。
- Runtime 不为整个 Extension 增加全局 Mutex。
- Context 携带当前 Run 的取消令牌。Session 外事件使用不可取消上下文。
- 取消信号不会跳过已经开始的同步状态提交，但会阻止新的 Provider、Tool 和 Host Exec 工作。
- `session_shutdown` 幂等执行，并在 Session Runtime 被丢弃前完成。

## 16. Kernel 集成位置

### 16.1 Session

`runtime/session.rs` 负责 trust、resources、start、info、switch、fork、tree 和 shutdown Point。任何前置取消都发生在 Store 变更前。

### 16.2 Agent Run

`runtime/run.rs` 只在真实生命周期边界调用 input、before_agent_start、agent、turn、message、context 和 Provider Point。组合逻辑由 Extension Runtime 提供，不留在 `run.rs`。

### 16.3 Tools

`runtime/tool_batch.rs` 负责 tool execution 生命周期。预检和最终结果转换委托给 Extension Runtime，工具并发与最终消息源码顺序不变。

### 16.4 Compaction

`runtime/compaction.rs` 负责 before compact 和 compact Point。Extension 自定义摘要必须经过与 Kernel 生成摘要相同的协议和 Store 校验。

### 16.5 Host 实现

新增 `runtime/extension.rs` 实现 Extension Host capability，负责：

- 在不跨 `await` 持锁的情况下创建快照。
- 将 Message Draft 转换为完整消息。
- 把动态注册变更提交到 Session Registry。
- 调用 Store、Queue、Compaction 和 Session 管理能力。
- 发布 Extension 错误和诊断事件。

### 16.6 Provider 请求拦截

Provider payload、Header 和响应状态只在 Provider adapter 内真实存在。Kernel 为每次 Model 调用创建请求级 `ModelRequestHooks`，其中封装当前 Session Extension Runtime 和调用身份；Provider adapter 在以下准确位置调用中立 Hook 接口：

1. Provider 私有 payload 完成序列化后调用 payload Hook。
2. 最终 Header 组装后、网络请求前调用 Header Hook。
3. 收到 HTTP 或 WebSocket 握手响应后、消费响应流前调用 response Hook。

`provider` crate 不依赖 `extension` crate，也不认识 Extension Event。Kernel 中的 adapter 将中立 Hook 数据与 Extension Point 相互转换。没有底层响应 Header 能力的 Provider 明确返回 unavailable，不伪造响应数据。

### 16.7 Model 与 Thinking

Active Model 和 Thinking Level 移入 `SessionRuntime`。Kernel 持有由构建期 ModelFactory 创建的只读 Model Catalog，Session 只保存当前选择和对应 Model handle。

Extension 切换 Model 时，Kernel 先完成 Catalog 查找和认证预检，再原子替换 Session 当前 Model，并按实际变化发送 `thinking_level_select` 和 `model_select`。正在执行的 Turn 固定使用开始时捕获的 Model/Thinking 快照，变化从下一 Turn 生效。

## 17. 测试策略

### 17.1 Extension crate 集成测试

测试放在 `crates/extension/tests/`：

- Point 的 Event/Output 关联和类型化注册。
- Module 与 Handler 顺序。
- 链式转换。
- 首个决定、取消和累积结果。
- 普通错误隔离。
- Tool Call fail-safe。
- Registry 冻结和 Session Runtime 隔离。
- 动态 Tool/Command 与 Active Tools。
- 并行 Handler 调用不被全局串行化。

### 17.2 Kernel 集成测试

测试放在 `crates/kernel/tests/`：

- 每个 Point 在真实生命周期位置触发。
- Context 和 System Prompt 转换影响真实 ModelRequest。
- Provider payload/header 转换到达 Provider adapter。
- Extension 切换 Model/Thinking 只影响下一 Turn，并产生正确事件。
- MessageEnd 和 ToolResult 转换发生在持久化前。
- Tool Call 错误、非法替换参数和主动 Block 都不会执行工具。
- 一个 Turn 使用单一 Tool Registry 快照。
- Extension 注入消息由 Kernel 分配身份和字符串毫秒时间。
- Session Runtime 相互隔离。

### 17.3 Store 与 ACP 集成测试

`crates/store/tests/` 验证 Extension Entry 的写入、回放、Fork 和 Tree 行为。`crates/acp/tests/` 验证 ACP 原生映射、Extension Update、批量回放、TurnId 和字符串毫秒时间戳。

### 17.4 测试执行规则

- Rust 行为按 TDD 实现，先观察测试因缺失行为失败，再实现最小生产代码。
- 测试代码只出现在 crate 的 `tests/` 集成测试目录。
- 正文不增加 `#[cfg(test)]` 测试支撑代码。
- 前端不要求 TDD，也不新增单元测试。
- 最终运行 workspace tests、Rustfmt、Clippy、WebUI check/build。
- 最终启动正式 `app` 后端，使用配置中的真实 Provider 完成 WebUI 端到端测试，不使用 fixture backend。

## 18. 文件改动边界

主要新增或修改：

- `crates/protocol/src/extension/`
- `crates/extension/src/point.rs`
- `crates/extension/src/handler.rs`
- `crates/extension/src/registrar.rs`
- `crates/extension/src/registry.rs`
- `crates/extension/src/runtime/`
- `crates/extension/src/host.rs`
- `crates/extension/src/factory.rs`
- `crates/extension/src/error.rs`
- `crates/kernel/src/runtime/extension.rs`
- `crates/kernel/src/model.rs`
- `crates/kernel/src/provider.rs`
- `crates/kernel/src/runtime/run.rs`
- `crates/kernel/src/runtime/session.rs`
- `crates/kernel/src/runtime/tool_batch.rs`
- `crates/kernel/src/runtime/compaction.rs`
- `crates/store/src/`
- `crates/acp/src/`
- 对应 crate 的 `tests/` 集成测试

`provider` 和 `config` 保持既有职责，只为构建期静态 Provider 声明增加必要的 Factory 接口，不把 Extension Runtime 逻辑下沉到这两个 crate。

## 19. 验收标准

- 不再存在通用无载荷 `ExtensionEvent` 和通用 `ExtensionDirective`。
- 每个非 UI Pi Point 都有独立 Event、Output 和组合策略。
- Event Handler 注册在编译期约束 Event/Output 类型。
- Extension Handler 顺序稳定，普通错误隔离，Tool Call 错误 fail-safe。
- 每个 Session 拥有独立 Extension Runtime。
- 动态 Tool/Command 不影响正在执行 Turn 的快照。
- Extension Message 和 Entry 可持久化、Fork、Tree 和回放。
- ACP 2.0 回放保留顺序、TurnId 和字符串毫秒时间戳。
- 不引入 UI Extension、Client FS/Terminal callback、热更新或动态 Provider。
- 所有 Rust、Clippy、格式、Web 检查和真实后端 WebUI E2E 通过。

# Extension 审查修复与 Hook 示例设计

## 1. 目标

本轮修复 Extension Runtime 审查发现的生命周期、注册事务、静态声明、Host 能力和持久化问题，并以 Pi 最新 Extension 文档和本地 Pi 源码为行为基准，最终保留 33 个非 UI Hook Point。

本轮同时为每个 Hook Point 提供可编译的真实 Rust 示例和索引文档。示例必须使用公开 API，不依赖测试专用支撑代码。

## 2. Hook 边界

保留以下 33 个 Pi 非 UI Hook：

- Startup：`project_trust`、`resources_discover`。
- Session：`session_start`、`session_info_changed`、`session_before_switch`、`session_before_fork`、`session_before_compact`、`session_compact`、`session_before_tree`、`session_tree`、`session_shutdown`。
- Input 与 Agent：`input`、`before_agent_start`、`agent_start`、`agent_end`、`agent_settled`、`turn_start`、`turn_end`、`message_start`、`message_update`、`message_end`、`context`。
- Provider：`before_provider_headers`、`before_provider_request`、`after_provider_response`。
- Model：`model_select`、`thinking_level_select`。
- Tool：`tool_execution_start`、`tool_execution_update`、`tool_execution_end`、`tool_call`、`tool_result`、`user_bash`。

删除 Pi 不存在的 `session_switch` 和 `session_fork`。不保留兼容层。

继续排除 UI、主题、快捷键、客户端文件系统回调、客户端 Terminal callback、热更新和运行期 Provider 注册。

## 3. 生命周期修复

启动顺序调整为：

1. `project_trust`
2. `session_start`
3. `resources_discover`
4. `model_select`
5. `thinking_level_select`

Agent 顺序调整为：

1. `input`
2. Prompt Template 与 Skill 展开
3. `before_agent_start`
4. `agent_start`
5. Turn 循环
6. `agent_end`
7. 自动 Compaction
8. `agent_settled`

每条消息统一执行 `message_start`、零到多个 `message_update`、`message_end`。User、Assistant、ToolResult 和 Extension Message 都不能出现 `message_end` 早于 `message_start`。

Provider 顺序调整为 `before_provider_headers`、`before_provider_request`、发送请求、`after_provider_response`。同一 Provider 请求的传输重试复用准备后的 Header 和 Payload，不重复运行前置 Hook。

`message_update` 增加 ToolCall 更新类型。当前 Provider 只能提供完整 ToolCall 时，发送一次完整 ToolCall Update；未来 Provider 提供参数增量时不需要改变 Hook 类型。

## 4. 注册事务与所有权

每个 `ExtensionModule` 先注册到独立候选 Registry。只有 Descriptor、Handler、Command、Flag 和 Tool 全部注册成功后，候选 Registry 才合并到 Session Registry。失败候选不会留下部分状态。

动态 Command 注册和删除只允许操作当前 `ExtensionInvocation.extension_id`。公开 Context API 不再接收可伪造的 ExtensionId。

Tool 冲突遵循 Pi 的 first-registration-wins 规则。冲突形成 Extension 诊断，不静默覆盖其他 Extension；Extension 显式覆盖 Built-in Tool 的既有能力保持不变。

## 5. 静态声明与 Model Catalog

`StaticExtensionRegistration` 必须先于 Model 构造解析。Kernel 不再读取后丢弃静态声明。

`ModelFactory` 接收完成校验的静态 Provider 声明并创建不可变 `ModelCatalog`。Catalog 保存已配置 Model，Session 保存当前 Model key。运行期间只能在 Catalog 内切换 Model，不提供 Provider 注册、注销或配置变更。

静态 Flag 定义和解析值进入 Session Runtime 快照。重复 Flag、非法默认值、Provider 冲突和无效 Model 在 Kernel 构造阶段返回类型化错误。

Model 切换和 Thinking Level 变更写入 Pi v4 Entry，并触发对应 Hook。Thinking Level 按目标 Model 能力裁剪；若现有 Provider 元数据无法表达能力，则在 `ModelProfile` 增加协议层能力字段，不在 Kernel 内硬编码 Provider 名称。

## 6. Host 能力修复

`send_extension_message` 在没有业务 Turn 的 Session Hook 中分配独立系统 TurnId，因此 `session_start` 等 Hook 可以发送可持久化消息。

`send_user_message` 统一进入正常 Input 流程，并使用 `InputSource::Extension`：

- Session 正在运行时，根据 delivery 进入 steer 或 follow-up 队列。
- Session 空闲时启动正常 Agent Run。
- 不持有 Run Gate 的调用路径可以等待执行；可能处于 Run Gate 内的 Hook 使用调度方式避免自锁。

`set_session_name` 必须触发 `session_info_changed`。`set_active_tools`、`set_model` 和 `set_thinking_level` 必须持久化，并从下一 Turn 的不可变快照生效。

`system_prompt` 在 Session Hook 中也可读取基础 Prompt；存在活动 Turn 时返回该 Turn 的完整 Prompt。

Event Bus 使用 Session 级广播语义，提供发布和订阅。旧 Runtime 失效后不能创建新订阅或发布事件；已创建 Receiver 在 Runtime 销毁后自然关闭。

`wait_for_idle` 使用通知机制，不进行 10ms 轮询。

## 7. Session 操作一致性

Session 启动流程在 Runtime 已临时注册但尚未对调用者宣告成功时运行。任何资源发现、Skill、Prompt 或 Hook 后处理失败，都必须从 Kernel Session Map 移除 Runtime、使 generation 失效，并保留可重试状态。

多 Session 服务端不伪造 Pi 的单活动 Session 完成事件。`session_before_switch` 用于协议层选择新建或恢复操作；成功后旧 Runtime 在确实被替换的连接工作流中收到 `session_shutdown`，新 Runtime 收到 `session_start`。Kernel 的持久化 Session 列表本身不等价于客户端当前选择。

Command Context 的 create、switch、fork 和 navigate 返回明确结果；ACP Dispatcher 将结果返回客户端，不再无条件返回 JSON `null`。

## 8. Server User Bash

Kernel 增加服务器端 User Bash 执行入口：先触发 `user_bash`，首个替代结果直接返回；没有替代结果时使用与 Built-in Bash 相同的服务端执行边界。该入口不调用 ACP Client Terminal callback。

ACP 扩展协议增加直接请求方法供未来客户端使用，但本轮 WebUI 不增加 Terminal 界面。

## 9. 示例组织

新增：

```text
crates/extension/examples/
├── hooks.rs
└── hooks/
    ├── startup.rs
    ├── session.rs
    ├── agent.rs
    ├── provider.rs
    ├── model.rs
    └── tool.rs
```

`hooks.rs` 定义一个完整 `ExtensionModule`，各领域模块注册具体 Handler。示例执行真实、无副作用或受控的行为，例如：

- 根据工作目录决定 Project Trust。
- 贡献 Skill 和 Prompt 路径。
- 保护 Session 切换、Fork、Compaction 和 Tree Navigation。
- 转换 Input、System Prompt、Context 和最终消息。
- 添加追踪 Header、调整 Provider Payload、记录响应状态。
- 记录 Model 和 Thinking 变化。
- 阻止危险 Bash、修正 Tool 参数、清理 Tool Result。
- 拦截服务器端 User Bash 并返回确定结果。

新增 `docs/extensions/hooks.md`，按 Hook 列出触发时机、Event、Output、组合规则和示例文件。文档中的代码引用必须与可编译示例一致，不复制另一套伪代码。

## 10. 测试与验收

新增或扩展 crate 级集成测试，覆盖：

- 33 个 Hook 均有真实 Kernel 或 Provider 触发路径。
- 生命周期顺序。
- Module 注册失败回滚。
- 静态声明在 Model 构造前生效。
- Session Model、Thinking 和 Active Tools 持久化恢复。
- Extension 消息的系统 TurnId。
- Extension User Message 的 idle、steer 和 follow-up 路径。
- Event Bus 发布、订阅和 Runtime 失效。
- Server User Bash 替代与默认执行。
- Session 启动失败回滚。
- Command 所有权隔离。

验证命令：

```bash
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo check -p extension --examples
rtk cargo clippy --workspace --tests --examples -- -Dwarnings
rtk cargo test --workspace --all-targets
rtk pre-commit run --all-files
```

代码变更完成后继续使用正式 `app` 后端、真实配置和真实 Provider 执行 WebUI E2E，不使用 fixture backend。

## 11. 非目标

- 不新增 Extension UI 接入点。
- 不新增客户端文件系统或 Terminal callback。
- 不实现热更新或 Extension 自动发现。
- 不实现运行期 Provider 注册。
- 不保留被删除 Hook 或旧 API 的兼容适配。
- 不创建 `index.jsonl`。

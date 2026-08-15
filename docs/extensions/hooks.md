# Extension Hook 开发指南

Extension 采用强类型注册：每个 Hook Point 固定自己的事件和返回类型，模块只能通过 `ExtensionRegistrar::on` 注册。Kernel 为每个 Session 创建独立 Runtime，因此 Handler 不应保存跨 Session 的可变状态；需要持久化时应使用 `ExtensionContext` 的 Host 能力。

完整可编译示例位于 `crates/extensions/examples/hooks.rs`，并复用 `crates/extensions/src/available/hook_examples/` 中的正式扩展源码。可用以下命令验证：

```bash
rtk cargo check -p extensions --example hooks
```

## 静态编译与加载

`crates/extensions/build.rs` 默认读取项目根目录的 `claw.toml`。配置顺序同时决定编译内容和 Hook 注册顺序：

```toml
[extensions]
enabled = ["command-guard"]
```

修改列表后必须重新执行 Cargo 构建。构建脚本只为列表中的扩展生成 Rust 模块声明，因此未列出的扩展不会进入本次 `extensions` crate 编译。空列表会生成空目录。

构建配置路径是参数而不是解析逻辑中的固定字符串。需要使用其他文件时，复用公共产品身份定义的配置环境变量：

```bash
CLAW_CONFIG=/absolute/path/to/another.toml rtk cargo build -p app
```

运行时配置只能启用该二进制已经编译的扩展，否则应用拒绝启动。默认 `command-guard` 负责拦截模型 Bash 和用户 `!`/`!!` Bash 中的 `rm -rf`。`hook-examples` 会修改 Prompt 和 Provider 请求，仅在明确加入构建配置后启用。

## 注册入口

Extension 实现 `ExtensionModule`，在 `register` 中注册 Handler。注册是事务性的：任意一步失败时，该模块的 Handler、Tool、Command 和 Flag 都不会进入最终 Registry。

```rust
impl ExtensionModule for MyExtension {
    fn descriptor(&self) -> ExtensionDescriptor { /* ... */ }

    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<InputPoint, _>(NormalizeInput)?;
        registrar.on::<ToolCallPoint, _>(CommandPolicy)
    }
}
```

## 33 个非 UI Hook

| 领域 | Hook | 行为 |
| --- | --- | --- |
| Startup | `project_trust` | 在读取项目资源前给出信任决定，首个明确决定生效。 |
| Startup | `resources_discover` | 累加服务器端 Skill 与 Prompt Template 路径。 |
| Session | `session_start` | Session Runtime 完成初始化后的观察点。 |
| Session | `session_info_changed` | Session 名称等持久化信息变化后的观察点。 |
| Session | `session_before_switch` | 新建或恢复 Session 前可取消操作。 |
| Session | `session_before_fork` | Fork 前可取消，并可跳过对话恢复。 |
| Session | `session_before_compact` | Compaction 前可取消或提供完整结果。 |
| Session | `session_compact` | Compaction 持久化完成后的观察点。 |
| Session | `session_before_tree` | Tree 导航前可取消、提供 Summary 或修改 Summary 指令。 |
| Session | `session_tree` | Tree 导航完成后的观察点。 |
| Session | `session_shutdown` | Runtime 失效前的最后观察点。 |
| Agent | `input` | Prompt/Skill 展开前继续、改写或消费输入。`!`/`!!` 由 User Bash 路径优先处理。 |
| Agent | `before_agent_start` | 修改 System Prompt，并注入 Extension Message。 |
| Agent | `agent_start` | Agent Loop 开始后的观察点。 |
| Agent | `agent_end` | Agent Loop 结束后的观察点。 |
| Agent | `agent_settled` | 自动重试、Follow-up 等工作全部结束后的观察点。 |
| Agent | `turn_start` | 每个模型 Turn 开始时触发。 |
| Agent | `turn_end` | Turn 和源顺序 Tool Result 完成后触发。 |
| Message | `message_start` | 一条流式消息开始时触发。 |
| Message | `message_update` | 接收强类型 Text、Reasoning 或 Tool Call 增量。 |
| Message | `message_end` | 持久化前可替换完整消息，但必须保留身份和角色。 |
| Context | `context` | 仅替换本次 Provider 请求的上下文，不修改持久化历史。 |
| Provider | `before_provider_request` | Provider 私有 JSON Body 序列化后可整体替换。 |
| Provider | `before_provider_headers` | 最终 Header 组装后按顺序 Set/Remove。 |
| Provider | `after_provider_response` | HTTP 响应或 WebSocket 握手元数据可用时触发。 |
| Model | `model_select` | 模型设置、循环或恢复后触发。 |
| Model | `thinking_level_select` | Thinking Level 设置并按模型能力收敛后触发。 |
| Tool | `tool_execution_start` | Tool 参数完成变换和校验、即将执行时触发。 |
| Tool | `tool_execution_update` | Tool 运行期间接收可替换的完整可见快照。 |
| Tool | `tool_execution_end` | Tool Result 完成变换后的观察点。 |
| Tool | `tool_call` | 校验前继续、替换参数或阻止 Tool Call。 |
| Tool | `tool_result` | 持久化前替换 Blocks、Details、错误状态或 Usage。 |
| Tool | `user_bash` | 拦截服务器端 `!`/`!!`；首个完整替代结果生效，否则使用 Built-in Bash 的执行器。 |

## 组合与失败规则

- 观察点按注册顺序运行，返回 `()`。
- `input`、`before_agent_start`、`context`、Provider Patch 和 Tool Patch 会按注册顺序组合，后续 Handler 看到前序结果。
- Session 取消点和 `tool_call` 的 Block 会提前停止对应操作。
- `user_bash` 使用首个 `Some(UserBashResult)`；`None` 表示继续查找，全部为 `None` 时执行本地 Shell。
- 普通 Handler 错误会写入 Extension 诊断消息；安全关键的 Tool Call 预处理失败会关闭执行。

## Host 能力

`ExtensionContext` 可读取新快照、发送 Extension/User Message、持久化 Custom Entry、修改 Session 名称和标签、管理 Tool/Command/Model/Thinking、执行服务器进程、使用 Session Event Bus，以及请求 Abort、Compact 或 Shutdown。

Session 切换、创建、Fork 和 Tree 导航只在 `ExtensionCommandContext` 中提供，因为这些操作可能替换当前 Runtime。所有 Context 都带 Generation 校验；`session_shutdown` 完成后继续调用旧 Context 会返回 `StaleRuntime`。

User Bash 始终在服务器执行，不使用 ACP Client Terminal callback。`!` 的结果持久化并进入后续模型上下文；`!!` 同样持久化和回放，但不会发送给模型。ACP 通过产品命名空间下的 `session/bash` 扩展方法调用同一 Kernel 路径。

`hook-examples` 覆盖其中 31 个观察或变换接入点；`command-guard` 独立提供 `tool_call` 和 `user_bash` 两个安全策略示例，避免危险命令规则在多个扩展中重复定义。

# Extension 审查修复与 Hook 示例实施计划

> 本计划在当前会话内直接执行。受项目规则约束，不使用 SubAgent，不创建 worktree，不创建 commit。

**目标：** 修复 Extension 运行时审查中确认的问题，使后端严格覆盖 Pi 当前 33 个非 UI hook，并补充可以编译、可以作为真实扩展起点的示例。

**架构：** `protocol` 继续承载跨 crate 公共类型；`extension` 只负责扩展注册、强类型分发和宿主接口；`kernel` 负责生命周期、状态和消息持久化；`provider` 只暴露请求边界 hook；`acp` 只做协议映射。静态扩展声明在 kernel/model 创建之前冻结，动态能力按 session 隔离。

**技术栈：** Rust、Tokio、typed-builder、serde、ACP HTTP/WebSocket、现有 provider/config/store 抽象。

## 全局实施约束

- 仅保留 Pi 当前存在的 33 个非 UI hook，不保留 `session_switch` 和 `session_fork` 完成事件。
- 新增函数必须有英文用途注释；修改非平凡逻辑必须用英文注释解释原因。
- 公共消息、事件和状态类型放在 `protocol`，禁止跨 crate 重复定义。
- 测试只放在 `tests/` 或精确的 `#[cfg(test)] mod tests` 中，正文不增加测试支撑分支。
- 避免单用途短 helper；只有一个自定义类型参数的逻辑优先实现为该类型的方法或标准 trait。
- 所有依赖优先定义在 workspace `Cargo.toml`，子 crate 使用 `workspace = true`。
- 不实现 UI hook、客户端文件系统回调、客户端终端回调、配置热更新或动态 provider。

### 任务 1：收敛公共 hook 类型到 Pi 当前集合

**文件：**

- 修改：`crates/protocol/src/extension/events/session.rs`
- 修改：`crates/protocol/src/extension/events/agent.rs`
- 修改：`crates/protocol/src/extension/events/mod.rs`
- 修改：`crates/protocol/src/extension/mod.rs`
- 修改：`crates/extension/src/point.rs`
- 修改：`crates/extension/src/registry.rs`
- 修改：`crates/extension/src/runtime/session.rs`
- 修改：`crates/extension/src/runtime/agent.rs`
- 测试：`crates/protocol/tests/extensions.rs`
- 测试：`crates/extension/tests/runtime.rs`

**步骤：**

1. 先增加或调整测试，断言公共 API 只包含 33 个非 UI hook，`MessageUpdateEvent` 能表达工具调用增量，四字段以上事件使用 builder。
2. 运行目标测试，确认旧实现不能满足新断言。
3. 删除 `SessionSwitchPoint`、`SessionForkPoint` 及对应事件、注册表字段和分发方法。
4. 为消息更新增加明确的 text、reasoning、tool-call 类型，复用 `protocol::ToolCall`，不在 kernel 重复定义。
5. 将 `SessionCompactEvent` 改为 typed-builder 构造，并更新调用点。
6. 运行：

   ```bash
   rtk cargo test -p protocol --test extensions
   rtk cargo test -p extension --test runtime
   ```

### 任务 2：使模块注册具备事务语义并约束动态能力所有权

**文件：**

- 修改：`crates/extension/src/registrar.rs`
- 修改：`crates/extension/src/registry.rs`
- 修改：`crates/extension/src/dynamic.rs`
- 修改：`crates/extension/src/context.rs`
- 修改：`crates/extension/src/host.rs`
- 测试：`crates/extension/tests/registration.rs`
- 测试：`crates/extension/tests/dynamic.rs`
- 测试：`crates/extension/tests/context.rs`

**步骤：**

1. 添加失败模块不能留下 handler、command、flag 或 tool 的回归测试。
2. 添加扩展只能修改自己 command 的所有权测试；添加工具重名时“先注册者生效”的测试。
3. 运行目标测试并观察失败。
4. 让 `ExtensionRegistrar::register_module` 在临时候选注册表中执行，成功后一次性合并；失败则丢弃候选状态。
5. 让动态 command API 从 `ExtensionContext` 的模块身份派生 owner，移除调用方传入任意 `ExtensionId` 的能力。
6. 工具目录按稳定注册顺序合并，遇到重名保留首个定义，并记录可诊断冲突。
7. 运行：

   ```bash
   rtk cargo test -p extension --test registration
   rtk cargo test -p extension --test dynamic
   rtk cargo test -p extension --test context
   ```

### 任务 3：在模型创建前应用静态扩展声明

**文件：**

- 修改：`crates/protocol/src/extension/registration.rs`
- 修改：`crates/kernel/src/model.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/provider/src/factory.rs`（仅在现有 provider factory 需要公开目录构造入口时）
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/kernel/tests/provider_runtime.rs`

**步骤：**

1. 添加测试，证明 `static_registration()` 发生在任何模型解析之前，静态 provider 声明与 flag 值进入冻结后的 kernel 配置快照。
2. 添加 provider/model 重名和非法声明的失败测试，保证 kernel 不会半初始化。
3. 运行目标测试并观察失败。
4. 新增 provider-neutral `ModelCatalog`，由 `ModelFactory` 基于 `StaticExtensionRegistration` 创建；默认活动模型只是目录中的一个选择，不再是 kernel 唯一全局模型对象。
5. `ProviderModelFactory` 将 config provider 与静态声明在内存中合并并校验；不写回 TOML，不提供运行期动态 provider。
6. `KernelFactory::build` 调整顺序为：静态注册 → 模型目录 → kernel 共享状态 → session runtime。
7. 保存冻结后的扩展描述、flag 值和模型目录，以供 session 与 ACP 查询。
8. 运行：

   ```bash
   rtk cargo test -p kernel --test extensions_host
   rtk cargo test -p kernel --test provider_runtime
   ```

### 任务 4：补齐 session 模型、thinking 和 active-tools 状态

**文件：**

- 修改：`crates/protocol/src/session.rs`
- 修改：`crates/protocol/src/extension/events/model.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/store/src/session.rs` 或现有 session 元数据实现
- 测试：`crates/kernel/tests/session_capabilities.rs`
- 测试：`crates/store/tests/session.rs` 或现有对应集成测试

**步骤：**

1. 添加 session 关闭并恢复后仍保留模型、thinking level 和 active-tools 的测试。
2. 添加模型切换必须解析目录、thinking 必须按模型能力收敛、状态变更必须发 `session_info_changed` 的测试。
3. 运行目标测试并观察失败。
4. 在共享 session 元数据中持久化三项状态，并在恢复时校验模型/工具仍可用。
5. host 的 `set_model`、`set_thinking_level`、`set_active_tools` 只更新下一次 turn 使用的原子快照；进行中的 turn 保持不变。
6. 所有成功状态变化统一发出持久化事件和 live event。
7. 运行目标测试。

### 任务 5：纠正 startup、agent、message 和 provider 生命周期顺序

**文件：**

- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/extension/provider.rs`
- 修改：`crates/provider/src/completion.rs` 或实际 hook 组装位置
- 修改：Responses WebSocket 的正式 provider 请求实现
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/kernel/tests/turn.rs`
- 测试：`crates/kernel/tests/provider_runtime.rs`
- 测试：对应 provider 集成测试

**步骤：**

1. 添加完整事件顺序测试：project trust → session start → resources discover → model/thinking；input → expansion → before-agent-start → agent-start → turn-start。
2. 添加每条消息严格满足 start → zero-or-more update → end 的测试，并覆盖模型工具调用消息。
3. 添加 provider hooks 顺序测试：headers → payload → request → response；覆盖 HTTP 与 WebSocket。
4. 添加启动失败后重试不会出现 duplicate extension 的回归测试。
5. 运行目标测试并观察失败。
6. 调整 session 启动顺序，并在任意模块启动失败时撤销该 session 的扩展运行时注册。
7. 调整 agent/turn/message 发射位置；工具调用到达时发 `MessageUpdateKind::ToolCall`。
8. provider 请求局部 hook 按固定顺序执行，并让 Responses WebSocket 走相同组合器。
9. 运行目标测试。

### 任务 6：统一 Extension host 的消息、输入与等待语义

**文件：**

- 修改：`crates/protocol/src/id.rs`
- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/kernel/tests/turn.rs`

**步骤：**

1. 添加 session hook 在无活动 turn 时发送扩展消息的测试，断言使用稳定规则生成的 system TurnId，时间戳仍为字符串毫秒值。
2. 添加空闲和运行中调用 `send_user_message` 的测试：两者都经过 `InputSource::Extension`，前者启动 turn，后者遵循 queue 策略。
3. 添加 `set_session_name` 发 info-changed、`system_prompt` 在无活动 turn 可读、`wait_for_idle` 无轮询的测试。
4. 运行目标测试并观察失败。
5. 将无 turn 的扩展消息映射到 session system turn，完整持久化 message/turn/timing。
6. `send_user_message` 复用 kernel 的标准输入入口，不直接拼接消息。
7. session name、system prompt 与状态事件复用 session runtime 的类型化操作。
8. 用 `tokio::sync::Notify` 或 watch 状态通知替换 10ms busy loop。
9. 运行目标测试。

### 任务 7：实现 Pi 语义的服务端 User Bash hook

**文件：**

- 修改：`crates/protocol/src/extension/events/tool.rs`
- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/tools/src/bash.rs`
- 修改：`crates/acp/src/extension.rs`
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/acp/tests/extension.rs`

**步骤：**

1. 添加 `!` 与 `!!` 服务端 bash 请求经过 `user_bash` hook 的测试，覆盖拦截、改写和继续执行。
2. 添加 ACP 2.0 扩展请求映射测试，确认不依赖 client terminal callback。
3. 运行目标测试并观察失败。
4. 抽取现有 bash tool 的执行器类型供 tool 与 user-bash 路径共同使用，避免复制进程管理代码。
5. kernel 在服务端识别 user-bash 输入，先调用 hook，再按结果执行、持久化并回放消息。
6. ACP 仅增加明确命名的扩展方法和公共 payload；不加入终端回调能力。
7. 运行目标测试。

### 任务 8：增加覆盖全部 hook 的真实可编译示例

**文件：**

- 新增：`crates/extension/examples/hooks.rs`
- 新增：`crates/extension/examples/hooks/startup.rs`
- 新增：`crates/extension/examples/hooks/session.rs`
- 新增：`crates/extension/examples/hooks/agent.rs`
- 新增：`crates/extension/examples/hooks/provider.rs`
- 新增：`crates/extension/examples/hooks/model.rs`
- 新增：`crates/extension/examples/hooks/tool.rs`
- 新增：`docs/extensions/hooks.md`
- 修改：`crates/extension/Cargo.toml`（仅当示例需要显式 `[[example]]`）

**步骤：**

1. 每个文件实现一个真实 `ExtensionModule`，使用正式 registrar/handler/context API，不使用 mock 或测试专用支撑。
2. 33 个 hook 各给一个最小但有实际意义的行为，例如注入上下文、阻止危险工具、增加 provider header、压缩前保存摘要、监听模型变化。
3. 示例默认只记录或做可逆改写，避免把凭据、付费调用或破坏性 shell 行为写入示例。
4. 文档建立“Pi hook → Rust point → 示例文件”的完整映射，并说明事件顺序和允许的返回值。
5. 运行：

   ```bash
   rtk cargo check -p extension --examples
   ```

### 任务 9：全量质量检查与真实后端 E2E

**文件：**

- 检查：所有本阶段修改文件
- 必要时修改：仅修复检查暴露的问题

**步骤：**

1. 检查 Rust 正式文件大小和职责；超过 1,000 行的非 provider 正式文件必须重新判断能否按语义拆分。
2. 运行格式化和静态检查：

   ```bash
   rtk cargo fmt --all -- --check
   rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
   ```

3. 运行全量 Rust 测试：

   ```bash
   rtk cargo test --workspace --all-features
   ```

4. 运行 WebUI 类型检查和正式构建：

   ```bash
   rtk npm run check
   rtk npm run build
   ```

5. 使用用户已修复的真实配置启动正式后端；不启动 fixture，不使用假的 provider。
6. 在新的浏览器标签中完成真实 WebSocket E2E：创建 session、发送真实 prompt、观察 thinking/text/tool 事件顺序、刷新回放、删除 session。
7. 关闭本阶段启动的进程和浏览器标签；保留用户原有进程、会话和文件。
8. 汇总验证命令、通过数量、真实 provider 名称及仍存在的明确限制，不创建 commit。

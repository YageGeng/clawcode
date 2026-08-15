# 最终代码审查修复实施计划

> **执行方式：** 必须使用 `superpowers:executing-plans` 在当前会话串行实施。项目禁止 SubAgent、worktree 和未经用户授权的 commit，因此计划不包含提交步骤。

**目标：** 迁移实验扩展目录，并修复最新审查确认的全部生命周期、内存、安全、协议状态、可观测性和模块组织问题。

**架构：** 以类型化 Session 生命周期、Session 工具快照、User Bash disposition 和 ACP 领域更新替代隐式状态；以有界 Channel、latest-value 更新和 Header 安全分区明确资源及安全边界。目录迁移先建立新基线，跨协议类型随后从 Protocol 向 Kernel、ACP、WebUI 单向传播。

**技术栈：** Rust 2024、Tokio、Serde、typed-builder、tracing、ACP 2、React 19、TypeScript、Zustand、Vite。

## 全局约束

- 新增函数必须提供英文函数级注释；修改非平凡逻辑时增加英文注释说明原因。
- 后端行为变更严格执行红—绿—重构；测试只能位于 crate `tests/` 或准确的 `#[cfg(test)] mod tests`。
- 正式代码不得包含测试支撑类型或 `#[cfg(test)]` 分支。
- 前端不写单元测试，通过类型检查、生产构建和真实后端 E2E 验证。
- 超过三个字段的结构体使用 `typed-builder`；`Option` builder 字段声明默认值。
- 单参数且参数为项目自定义类型的逻辑优先成为该类型的关联方法或 trait 实现。
- 不新增大量短 helper，不保留旧 API 兼容层，不增加 Pi 不存在的能力。
- 所有相对路径依赖放在 Cargo 依赖分组前部；依赖版本统一放在 workspace `Cargo.toml`。
- 所有命令使用 `rtk` 前缀；不创建 commit，不使用 SubAgent 或 worktree。

---

### 任务 1：迁移实验扩展 crate

**文件：**
- 移动：`crates/extensions/` → `extensions/`
- 修改：`Cargo.toml`
- 修改：`extensions/build.rs`

**接口：**
- 保持 package 名称 `extensions` 和 `extensions::compiled_extensions()` 不变。
- 根 workspace member 及 workspace dependency 路径改为 `extensions`。
- `ProductIdentity::CONFIG_PATH_ENV` 仍优先于默认根目录 `claw.toml`。

- [ ] **步骤 1：执行目录移动**

  使用显式路径移动完整 crate，不触碰 `crates/extension`：

  ```bash
  rtk mv crates/extensions extensions
  ```

- [ ] **步骤 2：更新 Cargo 路径**

  根 `Cargo.toml` 使用：

  ```toml
  members = [
    "crates/extension",
    "extensions",
  ]

  [workspace.dependencies]
  extensions = { path = "extensions" }
  ```

- [ ] **步骤 3：修正构建配置根目录解析**

  `extensions/build.rs` 从 `CARGO_MANIFEST_DIR` 的直接父目录取得 workspace root；失败时返回带 manifest 路径的 `std::io::Error`。显式环境变量分支保持不变。

- [ ] **步骤 4：验证迁移基线**

  运行：

  ```bash
  rtk cargo check --workspace --all-targets
  rtk cargo test -p extensions
  ```

  预期：workspace 能解析新路径，`command-guard` 构建配置测试全部通过。

### 任务 2：类型化 Session 关闭状态并消除 shutdown 重入死锁

**文件：**
- 创建：`crates/kernel/src/runtime/lifecycle.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/extension/src/host.rs`
- 测试：`crates/kernel/tests/extensions_host.rs`

**接口：**
- `SessionLifecycle::{Active, Closing, Closed}`。
- `SessionRuntime::begin_closing() -> Result<(), KernelError>`。
- `SessionRuntime::ensure_run_available() -> Result<(), ExtensionHostError>`。
- `ExtensionHostError::SessionClosing`。

- [ ] **步骤 1：写 shutdown 重入失败测试**

  在真实 Kernel 集成测试中注册 `SessionShutdownPoint` handler，handler 调用 `context.send_user_message(...)` 和 `context.compact(...)`，通过 `tokio::time::timeout` 调用 `kernel.close_session(...)`。断言关闭在期限内完成，两个 Host 操作均返回 `ExtensionHostError::SessionClosing`。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p kernel --test extensions_host shutdown_reentrant_operations_are_rejected -- --exact
  ```

  预期：当前实现超时，证明测试捕获同一 `run_gate` 重入。

- [ ] **步骤 3：实现生命周期类型**

  `SessionRuntime` 增加：

  ```rust
  lifecycle: RwLock<SessionLifecycle>,
  ```

  `close_session` 在取得 `run_gate` 后调用 `begin_closing()`，再触发 shutdown hook；完成后进入 `Closed`、invalidate 并移除 Session。`run_with_source`、User Bash、Compaction、Fork、Tree 和相关 Host 入口在获取运行门禁前调用 `ensure_run_available()`。

- [ ] **步骤 4：运行定向测试确认 GREEN 并回归 Session 测试**

  ```bash
  rtk cargo test -p kernel --test extensions_host shutdown_reentrant_operations_are_rejected -- --exact
  rtk cargo test -p kernel --test session_capabilities
  rtk cargo test -p kernel --test lifecycle_order
  ```

### 任务 3：为 Bash reader 增加有界背压

**文件：**
- 修改：`crates/tools/src/bash.rs`
- 测试：`crates/tools/src/bash.rs` 中准确的 `#[cfg(test)] mod tests`
- 回归：`crates/tools/tests/bash.rs`

**接口：**
- `BASH_OUTPUT_CHANNEL_CAPACITY: usize` 为固定有界容量。
- `BashExecutor::spawn_reader` 接收 `tokio::sync::mpsc::Sender<Vec<u8>>`，使用 `send(...).await`。

- [ ] **步骤 1：写 reader 背压失败测试和大输出回归测试**

  私有模块测试使用容量为 1 的 bounded sender，启动可产生多个 chunk 的 reader，在不消费 receiver 时断言 writer 不会完成；旧 `UnboundedSender` 签名使测试先编译失败。集成测试执行产生明显大于 Channel 容量的 stdout/stderr 命令，断言执行完成、结果发生预期截断、完整输出文件存在且包含手工确定的首尾标记。测试不读取实现常量计算预期。

- [ ] **步骤 2：运行测试确认 RED 或现有无界实现缺少容量契约**

  ```bash
  rtk cargo test -p tools --lib bash::tests::reader_applies_backpressure -- --exact
  rtk cargo test -p tools --test bash large_interleaved_output_finishes_with_complete_spool -- --exact
  ```

  预期：私有测试因旧 `spawn_reader` 只接受 `UnboundedSender` 而编译失败；不得通过 grep 源码断言 Channel 类型。

- [ ] **步骤 3：替换为有界 Channel**

  使用：

  ```rust
  let (sender, mut receiver) = tokio::sync::mpsc::channel(BASH_OUTPUT_CHANNEL_CAPACITY);
  ```

  Reader 在发送时等待背压；receiver 生命周期和进程退出后的 EOF drain 保持完整。

- [ ] **步骤 4：运行 Bash 全部集成测试**

  ```bash
  rtk cargo test -p tools --lib bash::tests::reader_applies_backpressure -- --exact
  rtk cargo test -p tools --test bash
  ```

### 任务 4：将 Tool Update 改为 latest-value 快照

**文件：**
- 创建：`crates/kernel/src/runtime/tool_updates.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/tool_batch.rs`
- 测试：`crates/kernel/tests/turn.rs`

**接口：**
- `ToolUpdatePublisher` 实现 `ToolUpdateSink`，内部使用 `watch::Sender<Option<ToolExecutionUpdate>>`。
- `ToolUpdateStream::changed()` 返回当前最新快照。
- `ToolBatchResult` 派生 `typed_builder::TypedBuilder`，通过 builder 构造。

- [ ] **步骤 1：写高频更新行为测试**

  注册一个工具，在单次 execute 中同步发布数千个带递增序号的 `ToolResult`，然后返回最终结果。断言 Kernel 能在期限内完成、最后发布的快照被发出、`ToolExecutionEnd` 位于更新之后，且事件数量不会线性等于发布次数。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p kernel --test turn tool_updates_coalesce_to_the_latest_snapshot -- --exact
  ```

  预期：旧无界队列发出全部更新，事件数量断言失败。

- [ ] **步骤 3：实现类型化 watch 发布器**

  `publish` 使用 `send_replace(Some(update))`；`execute_tool_batch` 在 pending tools、取消和 receiver.changed() 之间 select。工具完成后读取尚未发出的最后版本，再发 Tool End。不得暴露 Tokio sender 给 Tool crate。

- [ ] **步骤 4：确认 GREEN 并运行 Turn 回归**

  ```bash
  rtk cargo test -p kernel --test turn tool_updates_coalesce_to_the_latest_snapshot -- --exact
  rtk cargo test -p kernel --test turn
  ```

### 任务 5：建立 Provider Header 安全分区

**文件：**
- 修改：`crates/provider/src/completion/hooks.rs`
- 测试：`crates/provider/tests/request_hooks.rs`

**接口：**
- `ProviderHeaderPartition { visible: ProviderHeaders, protected: HeaderMap }`。
- `ProviderHeaderPartition::from_headers(&HeaderMap) -> Self`。
- `ProviderHeaderPartition::apply(self, ProviderHeaders, &mut HeaderMap) -> Result<(), CompletionError>`。

- [ ] **步骤 1：写凭证不可见且不可覆盖的失败测试**

  构造包含 `authorization`、`x-goog-api-key`、`x-auth-token`、`x-custom-credential` 和普通 `content-type` 的真实 HTTP Request。Hook 记录可见 Header，并尝试用相同名称覆盖凭证。断言 Hook 看不到四个凭证值，最终 Request 仍保留原值，而普通 Header 修改生效。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p provider --test request_hooks provider_credentials_are_hidden_and_immutable -- --exact
  ```

  预期：至少 `x-goog-api-key`、`x-auth-token` 和自定义凭证对旧实现可见或可覆盖。

- [ ] **步骤 3：实现 allowlist 可见投影与 protected 合并**

  原请求只有明确允许观察的协议 Header 进入 `visible`，其余原始 Header 全部进入 `protected`。Hook replacement 先写入请求，随后 `protected` 覆盖同名值，从而保护未知自定义凭证。响应使用同一安全投影，不公开 Cookie 或 Provider 私有 Header。

- [ ] **步骤 4：运行 Provider Hook 测试**

  ```bash
  rtk cargo test -p provider --test request_hooks
  ```

### 任务 6：原子化 Session Tool 状态并对齐 Pi 动态 API

**文件：**
- 创建：`crates/kernel/src/runtime/tool_state.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/extension/context.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/tools/src/contract.rs`
- 修改：`crates/extension/src/context.rs`
- 修改：`crates/extension/src/host.rs`
- 修改：`crates/extension/src/dynamic.rs`
- 测试：`crates/kernel/tests/extensions_host.rs`
- 测试：`crates/extension/tests/dynamic.rs`

**接口：**
- `SessionToolState` 在一个 `RwLock<Arc<SessionToolSnapshot>>` 中保存 available registry 和 active names。
- `SessionToolState::register(Arc<dyn AgentTool>)` 在同一快照更新中注册并激活工具。
- 删除 `ExtensionContext::unregister_tool`、`ExtensionContext::unregister_command` 及对应 Host trait 方法。

- [ ] **步骤 1：写动态注册立即激活测试**

  Extension handler 调用 `context.register_tool(...)` 后，下一次 Run 使用记录模型请求的真实 Model，断言发送给模型的 Tool definitions 包含该工具；另一个 Session 不包含它。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p kernel --test extensions_host dynamically_registered_tool_is_active_in_its_session -- --exact
  ```

  预期：旧实现只更新 available registry，模型工具列表不包含新工具。

- [ ] **步骤 3：实现统一 Session Tool 快照**

  用一个状态类型替换分离的 `DynamicToolRegistry` 和 `DynamicActiveTools`。Turn 开始只获取一次快照，并由该快照构造 active subset；`set_active_tools` 和 `register_tool` 都发布完整新快照。

- [ ] **步骤 4：删除 Pi 不存在的注销 API**

  删除公开 Context、Host trait、Kernel Host 实现和 Dynamic Registry 中仅为 `unregister_tool`、`unregister_command` 服务的方法。保留 Registry 值整体销毁，不添加兼容别名。

- [ ] **步骤 5：确认 GREEN 并运行 Extension/Kernel 回归**

  ```bash
  rtk cargo test -p kernel --test extensions_host
  rtk cargo test -p extension --test dynamic
  rtk cargo test -p extension --test context
  ```

### 任务 7：类型化 User Bash disposition 并修复 Guard

**文件：**
- 修改：`crates/protocol/src/extension/result.rs`
- 修改：`crates/protocol/src/lib.rs`
- 修改：`crates/tools/src/bash.rs`
- 修改：`crates/kernel/src/runtime/user_bash.rs`
- 修改：`crates/acp/src/mapping.rs`
- 修改：`extensions/src/available/command_guard.rs`
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/updateRouter.ts`（任务 9 再将已验证解码逻辑迁入 Decoder）
- 修改：`web/src/features/conversation/BashExecutionCard.tsx`
- 测试：`crates/protocol/tests/extensions.rs`
- 测试：`extensions/tests/command_guard.rs`
- 测试：`crates/acp/tests/mapping.rs`
- 测试：`crates/kernel/tests/lifecycle_order.rs`

**接口：**
- `UserBashDisposition::{Completed, Blocked { reason: String }}`，Serde 使用带 tag 的 snake_case 形式。
- `UserBashResult` 增加 `disposition`，正常 Bash 使用 `Completed`。
- Web `BashExecutionEntity.disposition` 为 `{ type: "completed" } | { type: "blocked"; reason: string }`。

- [ ] **步骤 1：写 Protocol/Kernel/ACP 失败测试**

  分别断言正常退出码 `126` 的结果 disposition 是 completed、Guard 替代结果是 blocked、ACP replay JSON 保留 typed disposition。预期值使用手写 JSON，不复用生产序列化结果生成 expected。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p extensions --test command_guard
  rtk cargo test -p acp --test mapping
  rtk cargo test -p kernel --test lifecycle_order
  ```

- [ ] **步骤 3：实现类型并沿数据流传播**

  所有 `UserBashResult::builder()` 显式设置 disposition；ACP 产品扩展消息和 Web decoder 读取该字段。`BashExecutionCard` 只根据 disposition 判断 blocked，并设置 `data-status="blocked"`；退出码 `126` 仍显示普通失败。

- [ ] **步骤 4：让 Guard 解析直接命令段**

  在 `command_guard` 内定义负责解析和判定的类型，例如 `CommandInvocation::parse(&str)` 和 `CommandInvocation::is_recursive_forced_remove(&self)`。按 shell 控制操作符分段，识别 basename 为 `rm` 的直接命令及组合/分离 flags；测试覆盖 `-rf`、`-fr`、`-r -f`、`/bin/rm -rf`、`echo 'rm -rf'` 和普通 `rm file`。

- [ ] **步骤 5：运行后端测试与 Web 检查**

  ```bash
  rtk cargo test -p extensions --test command_guard
  rtk cargo test -p acp --test mapping
  rtk cargo test -p kernel --test lifecycle_order
  rtk npm run check
  ```

### 任务 8：使 Extension 诊断基础设施可观测并移除随机 Session API

**文件：**
- 修改：`crates/kernel/src/runtime/extension/diagnostics.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/Cargo.toml`
- 测试：`crates/kernel/tests/extensions_host.rs`

**接口：**
- `KernelExtensionDiagnostics` 内部按 `DiagnosticStage` 记录 ID、序列化、Store、History、Sink 阶段错误。
- 删除 `Kernel::extension_commands()`；Session 命令继续由 `invoke_extension_command(session_id, ...)` 精确访问。

- [ ] **步骤 1：写诊断持久化失败仍可观测测试**

  集成测试安装捕获 writer 的真实 `tracing_subscriber`，使用返回 Store 错误的 SessionStore 触发 Extension handler failure。断言主 Agent 流程继续，并且日志包含 Hook Point、ExtensionId 和 `persist` 阶段，不包含 Prompt 或 Header 值。

- [ ] **步骤 2：运行测试确认 RED**

  ```bash
  rtk cargo test -p kernel --test extensions_host diagnostic_infrastructure_failures_are_logged -- --exact
  ```

  预期：旧实现静默返回，捕获日志为空。

- [ ] **步骤 3：重构 reporter 并记录每个失败阶段**

  保持 `ExtensionDiagnosticSink::report` 的非失败返回契约；每个不能继续的分支显式 `tracing::warn!`/`error!`。日志字段只包含 `extension_id`、`point`、`stage` 和错误类型文本。

- [ ] **步骤 4：删除 `Kernel::extension_commands()` 并验证调用点**

  删除无 SessionId 的接口和不再使用的 import。任何真实调用点改为传入 SessionId；若无调用点，不创建替代 API。

- [ ] **步骤 5：确认 GREEN 并运行 Kernel Extension 测试**

  ```bash
  rtk cargo test -p kernel --test extensions_host
  rtk cargo test -p acp --test extension
  ```

### 任务 9：拆分 Web ACP 解码和状态应用，并落实构造规则

**文件：**
- 创建：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/workspace/updateRouter.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`crates/extension/src/registry.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/tool_batch.rs`

**接口：**
- `WorkspaceStoreAccess` 同时提供 `getState(): WorkspaceState` 和 `dispatch(action: WorkspaceAction): void`。
- `SessionUpdateDecoder.decode(notification, receivedOrder): DecodedSessionUpdate[]` 将 unknown ACP shape 转为穷举领域更新。
- `SessionUpdateRouter` 只依赖 namespace、`WorkspaceStoreAccess` 和 runtime refresh callback。
- `HandlerRegistry`、`PointHandlers`、`ToolBatchResult` 使用 typed-builder 或受控关联构造函数，不在业务代码散布多字段字面量。

- [ ] **步骤 1：提取类型化 decoder**

  将 ACP 元数据、消息、Thinking、Tool、Bash、Extension、Retry 和 Compaction 的 narrowing 移入 `SessionUpdateDecoder`。Decoder 返回领域更新枚举；Router 只检查目标 Session 并把更新映射为 `WorkspaceAction`。

- [ ] **步骤 2：消除 Router 的全局 Store 读取**

  `WorkspaceController` 构造并注入：

  ```ts
  const store: WorkspaceStoreAccess = {
    getState: () => useWorkspaceStore.getState(),
    dispatch: (action) => this.dispatch(action)
  };
  ```

  Router 内不再 import `useWorkspaceStore`，同一次 apply 的所有读取和写入来自同一个 access 对象。

- [ ] **步骤 3：应用 typed-builder 和类型驱动构造**

  为超过三个字段的 Rust 结构体增加 builder，替换 `ToolBatchResult { ... }` 等字面量。宏生成 `PointHandlers` 时生成一致的 builder/default 构造入口；不为单个字段复制创建 free helper。

- [ ] **步骤 4：运行静态检查**

  ```bash
  rtk npm run check
  rtk npm run build
  rtk cargo check --workspace --all-targets
  ```

### 任务 10：全量验证与真实后端 WebUI E2E

**文件：**
- 仅在验证发现真实缺陷时修改对应生产模块和集成测试。

- [ ] **步骤 1：执行 Rust 格式化、测试和 Clippy**

  ```bash
  rtk cargo fmt --all -- --check
  rtk cargo check --workspace --all-targets
  rtk cargo test --workspace --all-targets
  rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

  预期：无错误、警告或忽略的新失败。

- [ ] **步骤 2：执行 Web 静态验证**

  ```bash
  rtk npm run check
  rtk npm run build
  ```

- [ ] **步骤 3：启动正式后端和 WebUI**

  使用根目录真实 `claw.toml` 和真实 Provider 启动 `app`，不使用 fixture。打开实际 WebUI WebSocket 页面。

- [ ] **步骤 4：执行真实 E2E 场景**

  验证创建 Session、真实模型对话、Thinking 渲染、read/write/edit/bash 工具、`rm -rf` Guard 阻止、普通退出码 `126` 不显示为 blocked、消息回放顺序、Session 切换和删除。

- [ ] **步骤 5：检查最终差异**

  ```bash
  rtk git diff --check
  rtk git status --short
  ```

  只报告本轮完成情况和验证证据，不创建 commit。

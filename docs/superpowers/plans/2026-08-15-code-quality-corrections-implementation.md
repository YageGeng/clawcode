# Clawcode 代码质量修复实施计划

> **执行约束：** 在当前会话串行执行；用户明确禁止 SubAgent 与 worktree，项目规则禁止未经授权提交。

**目标：** 修复审查发现的八项缺陷，并把修改触及的混杂职责拆成可独立理解的模块。

**架构：** 用类型化状态和集中归约替代分散布尔值及副作用；用业务边界拆分模块，不创建大量单用途短 helper。Store、Kernel、Extension 和 WebUI 通过现有 factory/contract 接口保持解耦。

**技术栈：** Rust 2024、Tokio、Serde、ACP 2、React 19、TypeScript 6、Zustand、Vite。

## 全局约束

- 新增函数必须有英文函数级注释；非平凡变更必须有英文注释说明。
- 超过三个字段的结构体使用 `typed-builder`，`Option` 字段声明默认值。
- 单参数且参数为项目自定义类型的逻辑优先实现为关联方法或 trait。
- 不创建 commit，不使用 SubAgent，不使用 worktree。
- 前端无需 TDD 和单元测试；后端遵循红—绿—重构。

---

### 任务 1：Store 持久化原子性与语义拆分

**文件：**
- 修改：`crates/store/src/lib.rs`
- 创建：`crates/store/src/model.rs`
- 创建：`crates/store/src/state.rs`
- 创建：`crates/store/src/jsonl.rs`
- 修改：`crates/store/tests/jsonl.rs`

**接口：** `StoreState::candidate_sequence() -> u64` 只读计算；`StoreState::apply(&StoreRecord) -> Result<(), StoreError>` 同时服务实时写入和重放。

- [ ] 增加故障注入存储测试，证明追加失败后重试序列连续且重开可加载。
- [ ] 运行定向测试并确认因序列号缺口失败。
- [ ] 改为先持久化后归约，复用统一状态归约。
- [ ] 运行 store 测试并完成模块拆分，保持测试绿色。

### 任务 2：Kernel 类型化运行结果与统一收尾

**文件：**
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 创建：`crates/kernel/src/runtime/settlement.rs`
- 修改：`crates/kernel/tests/turn.rs`
- 修改：`crates/kernel/tests/provider_runtime.rs`

**接口：** `AgentOutcome::{Succeeded, Cancelled, Failed { message }}`；执行阶段返回 `RunOutcome`，外层统一完成生命周期。

- [ ] 增加预检失败和 provider 失败仍产生完整收尾事件的测试。
- [ ] 运行定向测试并确认缺少收尾事件。
- [ ] 实现 `RunOutcome` 与集中收尾，消除中段 `?` 绕过收尾的问题。
- [ ] 运行 kernel 测试并把收尾语义移入独立模块。

### 任务 3：WebUI 多会话隔离与异步竞态

**文件：**
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/controller.ts`
- 创建：`web/src/workspace/sessionCommands.ts`
- 创建：`web/src/workspace/updateDecoder.ts`

**接口：** workspace 以 `Record<SessionId, SessionWorkspace>` 保存会话；通知解码返回带目标 session id 的状态变换；打开会话使用请求代次。

- [ ] 将状态改为按会话分区，通知只更新目标会话。
- [ ] 为 `openSession` 增加 stale-response 防护。
- [ ] 拆分会话命令与通知解码，运行 `npm run check` 和 `npm run build`。

### 任务 4：Extension 效果组合与生命周期接线

**文件：**
- 修改：`crates/extension/src/lib.rs`
- 创建：`crates/extension/src/event.rs`
- 创建：`crates/extension/src/contract.rs`
- 创建：`crates/extension/src/pipeline.rs`
- 修改：`crates/extension/tests/pipeline.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：** `ExtensionEffects` 独立累积 injections、metadata 和 control；Kernel 在已有真实边界分发事件。

- [ ] 增加多个扩展同时注入消息和元数据的失败测试。
- [ ] 运行测试并确认前置效果被覆盖。
- [ ] 实现效果聚合并拆分扩展模块。
- [ ] 增加并验证工具更新、会话变化、模型与思考级别事件接线测试。

### 任务 5：MCP 超时与 Bash 输出完整性

**文件：**
- 修改：`crates/mcp/src/factory.rs`
- 修改：`crates/mcp/tests/factory.rs`
- 修改：`crates/tools/src/builtin/bash.rs`
- 修改：`crates/tools/tests/bash.rs`

**接口：** `McpAgentTool` 持有 `Duration` 超时；Bash 执行持有 reader task 并在正常退出后等待 EOF。

- [ ] 增加 MCP 挂起调用超时测试并确认当前实现不返回。
- [ ] 传播配置超时并用 Tokio timeout 返回类型化错误。
- [ ] 增加大输出尾部完整性测试并确认当前宽限会截断。
- [ ] 等待 reader EOF，同时为继承管道保留有界兜底。

### 任务 6：ACP 结果映射与错误文本

**文件：**
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/kernel/src/provider_adapter.rs`
- 修改：相关 kernel 测试。

**接口：** ACP 映射穷举 `AgentOutcome`；ToolResult 拒绝不支持块时准确说明支持 text/image。

- [ ] 增加失败与取消映射不同结束原因的测试。
- [ ] 实现穷举映射并修正错误文本。
- [ ] 运行 ACP 与 kernel 定向测试。

### 任务 7：全量验证与真实 WebUI E2E

**文件：** 仅在验证发现真实缺陷时修改对应模块。

- [ ] 运行 `cargo fmt --all --check`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。
- [ ] 运行 `npm run check` 与 `npm run build`。
- [ ] 启动真实 fixture 与 WebUI，在浏览器验证创建会话、切换会话、消息流、工具调用和跨会话隔离。
- [ ] 检查 `git diff --check` 与工作区差异，只报告本轮修改且不提交。

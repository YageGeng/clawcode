# WebUI 事件顺序、Thinking 与会话删除实施计划

> **执行约束：** 当前项目禁止 SubAgent、worktree 和未经授权的 commit。本计划由当前会话直接执行，所有测试代码只放在各 crate 的 `tests/` 目录。

**目标：** 严格支持 ACP v2 整条消息回放，按协议顺序统一渲染消息、Thinking 和工具调用，并通过标准 `session/delete` 永久删除会话。

**架构：** 按 ACP v2 规定使用 WebSocket 本地接收序号组成稳定顺序，产品 `sequence` 仅用于诊断；Workspace 使用统一 transcript 投影。删除能力从 StoreFactory 经 Kernel 暴露到 ACP v2 标准方法，WebUI 在 capability 检查后调用。

**技术栈：** Rust、agent-client-protocol v2、JSONL Store、React、TypeScript、Zustand、WebSocket。

## 全局约束

- 不修改 provider 和 config 的职责。
- 不增加全局 `index.jsonl`。
- 不定义与标准 `session/delete` 重复的 ACP 扩展。
- 前端不增加单元测试；最终必须运行正式后端真实 E2E。
- 新增函数和方法写英文作用注释；修改旧逻辑写英文原因注释。
- 不创建 commit。

---

### 任务 1：Store 幂等删除

**文件：**

- 修改：`crates/store/src/model.rs`
- 修改：`crates/store/src/factory.rs`
- 测试：`crates/store/tests/jsonl.rs`

**接口：**

- `StoreFactory::delete(&self, session_id: &SessionId) -> Result<(), StoreError>`

- [ ] 先新增集成测试：创建会话后删除，断言文件消失、列表为空，第二次删除仍成功。
- [ ] 运行 `cargo test -p store --test jsonl delete_session`，确认因缺少接口而失败。
- [ ] 在 trait 和 JSONL factory 中实现按 v4 header 定位并删除文件。
- [ ] 重跑测试并确认通过。

### 任务 2：Kernel 删除生命周期

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 测试：`crates/kernel/tests/session_capabilities.rs`

**接口：**

- `Kernel::delete_session(&self, session_id: &SessionId) -> Result<(), KernelError>`

- [ ] 新增集成测试：删除活动会话后不再出现在列表中，重复删除成功。
- [ ] 运行目标测试，确认缺少 Kernel 方法导致失败。
- [ ] 实现“关闭活动运行时后删除持久化文件”，仅忽略 `SessionNotFound`。
- [ ] 重跑目标测试并确认通过。

### 任务 3：ACP v2 标准删除方法

**文件：**

- 修改：`crates/acp/src/server.rs`
- 测试：`crates/acp/tests/extension.rs`

**接口：**

- `capabilities.session.delete = {}`
- JSON-RPC `session/delete`，参数 `{ sessionId }`，成功结果 `{}`

- [ ] 扩展 ACP 集成测试请求类型，断言初始化 capability 并验证删除后 `session/list` 不再返回会话。
- [ ] 运行目标 ACP 测试，确认 capability 或方法缺失导致失败。
- [ ] 注册 `DeleteSessionRequest` handler，并调用 `Kernel::delete_session`。
- [ ] 重跑目标测试并确认通过。

### 任务 4：Web 统一时间线与严格整条消息 upsert

**文件：**

- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/updateRouter.ts`
- 修改：`web/src/features/conversation/Conversation.tsx`
- 修改：`web/src/features/inspector/EventLog.tsx`

**接口：**

- `EventOrder { receivedOrder: number }`
- `TranscriptEntry` 为 message/tool 判别联合类型。

- [ ] 在 Router 为每条通知生成一次稳定顺序并传给所有相关 action。
- [ ] reducer 在实体首次出现时写入统一 transcript，并按 ACP 收到顺序排序。
- [ ] 对话区按 transcript 联合渲染消息与工具。
- [ ] 事件面板按同一规则展示。
- [ ] 修正 whole-message 的 omitted/null/array 替换语义，保留 chunk 追加。

### 任务 5：Thinking 默认展示与标准删除 UI

**文件：**

- 修改：`web/src/features/conversation/ReasoningBlock.tsx`
- 修改：`web/src/acp/protocol.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/features/sessions/SessionSidebar.tsx`
- 修改：`web/src/shell/AppShell.tsx`

- [ ] Thinking 首次渲染时默认展开，保留手动折叠能力。
- [ ] Web ACP 方法表增加标准 `session/delete`。
- [ ] 初始化时检查 `capabilities.session.delete`，删除前拒绝未声明能力的 Agent。
- [ ] 将侧边栏关闭按钮改为删除按钮与永久删除二次确认。
- [ ] 删除当前会话后清空 transcript，并刷新 session/list。

### 任务 6：完整验证与真实 E2E

- [ ] 运行 `cargo fmt --all -- --check`。
- [ ] 运行完整 `cargo test --workspace`。
- [ ] 运行 `cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [ ] 在 `web/` 运行项目规定的类型检查、lint 和构建命令。
- [ ] 重启正式 `app` 后端，使用修复后的真实配置和 Provider。
- [ ] 创建真实会话并发送会触发 Thinking、Tool、最终回复的请求，确认统一顺序。
- [ ] 恢复会话，确认 whole-message replay 包含用户消息、Thinking、Tool 和 Agent 消息。
- [ ] 从侧边栏删除该会话，确认二次确认、列表移除以及后端 JSONL 文件删除。

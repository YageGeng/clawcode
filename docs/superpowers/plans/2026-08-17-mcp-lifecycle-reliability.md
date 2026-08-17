# MCP 生命周期可靠性修复实施计划

> **执行要求：** 在当前会话内逐项执行；项目规则禁止 SubAgent 和 worktree。

**目标：** 按已确认的方案 A 修复五项 MCP 生命周期与 WebUI 交互可靠性问题。

**架构：** 由 `McpServerRuntime` 统一管理生命周期互斥和有界 discovery；Modern client 管理可恢复 subscription；Kernel/ACP 提供 Session 级 pending elicitation 快照；WebUI 按 Session 保存并恢复阻塞交互。

**技术栈：** Rust、Tokio、rmcp、ACP 2.0 extension、TypeScript、React、Zustand。

## 全局约束

- 所有新增 Rust 函数必须有英文函数级注释，非平凡逻辑必须有英文注释。
- 测试代码只能位于 crate 的 `tests/` 或精确的 `#[cfg(test)] mod tests`。
- 不创建大量短 helper；单个自定义类型参数的逻辑优先归属类型实现。
- 不创建 commit。
- WebUI 使用生产 app 后端和真实 provider 完成端到端验证。

---

### 任务 1：有界 discovery 与生命周期串行化

**文件：**
- 修改：`crates/mcp/src/server/mod.rs`
- 修改：`crates/mcp/tests/dynamic_catalog.rs`
- 修改：`crates/mcp/tests/session_lifecycle.rs`

- [ ] 先增加初始 discovery 超时、动态 discovery 取消和并发生命周期操作的失败测试。
- [ ] 分别运行目标测试并确认因现有无界或竞态行为失败。
- [ ] 在 `McpServerRuntime` 中增加 lifecycle operation 互斥和类型化有界 discovery。
- [ ] 运行目标测试确认通过。

### 任务 2：Modern subscription 自动恢复

**文件：**
- 修改：`crates/mcp/src/client/core.rs`
- 修改：`crates/mcp/tests/lifecycle_modern.rs`
- 修改：`crates/mcp/tests/dynamic_catalog.rs`

- [ ] 增加 subscription 结束后重新 discovery 会建立新订阅的失败测试。
- [ ] 运行目标测试并确认现有 `Some(completed_handle)` 行为导致失败。
- [ ] 使用可清理的运行状态实现 subscription 重建，并区分 `SubscriptionEnded`。
- [ ] 运行目标测试确认通过。

### 任务 3：保留不可见 Tool 的禁用偏好

**文件：**
- 修改：`crates/kernel/src/runtime/tool_state.rs`

- [ ] 在现有 `#[cfg(test)] mod tests` 增加 Tool 离线期间修改 active 集合的失败测试。
- [ ] 运行目标测试并确认偏好被错误删除。
- [ ] 修改 `SessionToolState::set_active`，只更新当前可见名称的偏好。
- [ ] 运行目标测试确认通过。

### 任务 4：Session 级 elicitation 查询与恢复

**文件：**
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/tests/mcp_host.rs`
- 修改：`crates/protocol/src/mcp/host.rs`
- 修改：`crates/protocol/src/mcp/request.rs`
- 修改：`crates/protocol/src/identity.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/tests/extension.rs`
- 修改：`web/src/acp/extensions.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/updateRouter.ts`

- [ ] 增加 Kernel pending elicitation 快照和 ACP Session 隔离失败测试。
- [ ] 运行目标测试并确认查询能力缺失。
- [ ] 以类型化 pending 项保留 request/sender，并增加 Kernel/ACP 查询接口。
- [ ] WebUI 按 Session 缓存实时请求，并在 open/reconnect 时读取 authoritative snapshot。
- [ ] 运行 Kernel、ACP 和 WebUI 静态检查。

### 任务 5：完整验证

- [ ] 运行 `cargo fmt --all -- --check`。
- [ ] 运行 workspace check、Clippy 和全部 Rust 测试。
- [ ] 运行 WebUI check 与生产 build。
- [ ] 启动 Legacy stdio 与 Modern HTTP MCP、生产 app 后端和真实 provider。
- [ ] 在真实 WebUI 验证 catalog、Tool 调用、reconnect、Session 切换和删除流程。
- [ ] 检查后端日志及浏览器控制台无新增错误。


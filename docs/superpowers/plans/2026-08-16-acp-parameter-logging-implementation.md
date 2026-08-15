# ACP 参数日志实施计划

> **执行要求：** 在当前会话内逐项执行；项目禁止 SubAgent 和 worktree。

**目标：** 为所有有效 ACP request 和 notification 增加完整、递归脱敏、按需启用的 `DEBUG` 参数日志。

**架构：** ACP Trace 工厂统一捕获 typed params，参数策略封装在专用类型中；handler 只传入自身 request/notification，不自行拼日志。参数日志复用现有 root Trace span 和 `EnvFilter`。

**技术栈：** Rust 2024、serde、serde_json、tracing、agent-client-protocol v2。

## 全局约束

- 新增函数必须有英文函数级注释，非平凡修改必须说明原因。
- 测试只放在 `tests/` 或精确的 `#[cfg(test)] mod tests` 中。
- 不增加无意义的短 helper，优先使用类型方法和 `From`/`TryFrom` 等 trait。
- tracing 事件宏使用全路径和普通格式化文本。
- ACP `DEBUG` 参数允许包含完整 prompt、shell 命令和资源内容，但凭证字段必须递归脱敏。
- 默认 `INFO` 不得序列化或输出参数。
- 不创建 commit。

---

### Task 1：建立参数日志回归测试

**文件：**

- 修改：`crates/acp/tests/tracing.rs`

**接口：**

- 通过真实 ACP connection 观察 stderr formatter 输出。
- 断言参数事件包含 operation description、完整 prompt 和普通字段。
- 断言嵌套敏感值全部替换为 `[REDACTED]`。

- [x] 扩展现有 Prompt Trace 场景，加入 initialize `_meta` 敏感字段和 cancel notification。
- [x] 增加参数日志、完整 prompt、脱敏值和 Trace 继承断言。
- [x] 运行 `rtk cargo test -p acp --test tracing`，确认因参数日志缺失而失败。

### Task 2：实现类型化参数捕获与脱敏

**文件：**

- 修改：`crates/acp/src/trace.rs`
- 修改：`crates/acp/src/extension.rs`

**接口：**

- `AcpTraceFactory::request<T, P>(&Responder<T>, &P)`，其中 `P: Serialize + ?Sized`。
- `AcpTraceFactory::notification<P>(&str, &P)`，其中 `P: Serialize + ?Sized`。
- `AcpExtensionRequest::parameters(&self) -> &serde_json::Value`。

- [x] 实现仅在 `DEBUG` 启用时执行的 JSON 捕获。
- [x] 实现 object/array 递归脱敏和敏感字段名归一化。
- [x] 在 `AcpOperationTrace::start` 内输出参数事件。
- [x] 确保序列化错误只影响参数诊断，不影响 handler。

### Task 3：接入全部 ACP handler

**文件：**

- 修改：`crates/acp/src/server.rs`
- 修改：`AGENTS.md`

**接口：**

- 每个 baseline request 将原始 typed request 传给 Trace 工厂后再消费。
- cancel notification 将完整 typed notification 传给 Trace 工厂。
- extension request 将保留的原始 JSON parameters 传给 Trace 工厂。

- [x] 修改 initialize、session/new、session/list、session/resume、session/close、session/delete 和 session/prompt。
- [x] 修改 session/cancel notification 和全部自定义 ACP extension request。
- [x] 在 `AGENTS.md` 记录已确认的 ACP `DEBUG` prompt 例外与强制脱敏边界。
- [x] 运行 `rtk cargo test -p acp --test tracing`，确认回归测试通过。

### Task 4：完整验证

**文件：** 无新增生产文件。

- [x] 运行 `rtk cargo fmt --all -- --check`。
- [x] 运行 `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [x] 运行 `rtk cargo test --workspace`。
- [x] 运行 `rtk git diff --check`。
- [x] 以 `RUST_LOG=info,acp::trace=debug` 启动真实 WebUI 后端并发起真实 Prompt。
- [x] 检查参数完整、敏感字段脱敏、Trace 一致，随后删除测试会话并停止服务。

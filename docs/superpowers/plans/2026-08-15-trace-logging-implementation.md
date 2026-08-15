# Trace 日志实施计划

> **执行要求：** 使用 `superpowers:executing-plans` 在当前会话逐项实施。项目禁止 SubAgent、worktree 和未经允许的 commit。

**目标：** 实现从 app/ACP 入口贯穿 Kernel 与 Provider 流式阶段的 `trace_id` 日志。

**架构：** `protocol::TraceId` 提供共享强类型标识；app 初始化 stderr subscriber 并共享 ID 工厂；ACP 为每个协议操作创建根 span；Kernel 与 Provider 创建必要的子 span并显式跨异步任务和流传播。日志事件使用可读文本，结构化字段仅用于 span 关联。

**技术栈：** Rust、Tokio、tracing、tracing-subscriber、ACP v2、Axum。

## 全局约束

- 所有命令必须使用 `rtk` 前缀。
- 新增函数必须有英文函数级注释，修改的非平凡逻辑必须说明原因。
- 测试代码只能位于 crate 的 `tests/` 或准确的 `#[cfg(test)] mod tests` 中，正文不得提供测试支撑代码。
- tracing 事件宏必须使用全路径和普通文本格式化，不导入事件宏，不使用事件字段风格。
- 不记录逐 token、完整 Prompt/响应、凭据和认证头。
- 不创建 commit。

---

### Task 1：共享 Trace 类型

**文件：**
- 修改：`crates/protocol/src/id.rs`
- 修改：`crates/protocol/src/scalar.rs`
- 修改：`crates/protocol/src/lib.rs`
- 测试：`crates/protocol/tests/scalars.rs`

**接口：**
- 产出：`protocol::TraceId`
- 产出：`IdKind::Trace`，前缀为 `trace`

- [ ] 在 `scalars.rs` 集成测试先增加 `TraceId` 校验、显示和序列化断言。
- [ ] 运行定向测试，确认因 `TraceId` 不存在而失败。
- [ ] 使用现有字符串 ID 宏定义 `TraceId`，导出类型并增加 `IdKind::Trace`。
- [ ] 运行定向测试并确认通过。

### Task 2：应用日志初始化

**文件：**
- 新增：`crates/app/src/logging.rs`
- 修改：`crates/app/src/lib.rs`
- 修改：`crates/app/src/main.rs`
- 修改：`crates/app/Cargo.toml`
- 修改：`Cargo.toml`
- 测试：`crates/app/tests/logging.rs`

**接口：**
- 产出：`app::LoggingFactory::install() -> Result<(), ApplicationError>`
- 行为：日志固定写入 stderr，默认 `info`，启动时读取一次 `RUST_LOG`

- [ ] 先通过 app 集成测试定义默认过滤级别和 stderr writer 配置。
- [ ] 运行定向测试，确认日志工厂尚不存在导致失败。
- [ ] 实现 `LoggingFactory` 与初始化错误类型，并在 CLI 解析后、应用构建前安装 subscriber。
- [ ] 增加 stdio/HTTP 服务启动和正常停止日志，确保 stdout 不接收日志。
- [ ] 运行 app 定向测试并确认通过。

### Task 3：ACP 根 Trace

**文件：**
- 新增：`crates/acp/src/trace.rs`
- 修改：`crates/acp/src/lib.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/src/transport.rs`
- 修改：`crates/app/src/lib.rs`
- 修改：`crates/app/src/web.rs`
- 测试：`crates/acp/tests/tracing.rs`

**接口：**
- 产出：`acp::AcpTransportKind::{Stdio,Http}`
- `AcpServerFactory` 消费 `Arc<dyn IdGenerator>`，每个 handler 创建包含 `trace_id`、transport、method 和可选 request id 的根 span。

- [ ] 先增加 ACP 集成测试，捕获初始化与 Prompt 操作日志并断言两个请求拥有不同 Trace。
- [ ] 运行测试，确认因工厂尚未生成 Trace 而失败。
- [ ] 实现类型化 Trace 操作上下文，并让所有 ACP 请求和通知在根 span 内执行。
- [ ] 对 Prompt 的 `connection.spawn` future 显式 instrument，使根 Trace 延续到 Kernel 完成。
- [ ] 更新 app 组合根和现有 ACP 集成测试调用方式。
- [ ] 运行 ACP 定向测试并确认通过。

### Task 4：Kernel 与 Provider 子链路

**文件：**
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/provider.rs`
- 测试：`crates/kernel/tests/turn.rs`
- 测试：`crates/kernel/tests/provider_runtime.rs`

**接口：**
- Kernel Run span 记录 `run_id`，Turn span 记录 `turn_id`。
- Provider span 覆盖流获取、重试和流消费，日志正文包含 provider/model 和最终状态。

- [ ] 先增加日志捕获测试，断言 Run、Turn、Provider 日志继承同一个 `trace_id`，且不包含测试 Prompt 正文。
- [ ] 运行定向测试，确认缺少生命周期日志导致失败。
- [ ] 在 Run/Turn 开始、完成、失败和取消边界增加适量日志及子 span。
- [ ] 在 Provider 获取、重试和终止边界增加日志，并 instrument 返回的流。
- [ ] 对模型压缩调用应用相同 Provider 子链路，不重复实现 Trace 生成。
- [ ] 运行 Kernel 定向测试并确认通过。

### Task 5：统一 tracing 宏风格与全量验证

**文件：**
- 修改：存在直接 tracing 事件宏导入的 Rust 正文文件
- 修改：`AGENTS.md`

**接口：**
- 所有事件宏使用 `tracing::...!` 全路径；类型和 `Instrument` trait 可显式导入。

- [ ] 搜索并移除直接导入的 tracing 事件宏，将调用改为全路径。
- [ ] 搜索新增日志，确认没有事件字段风格和敏感正文。
- [ ] 运行 `rtk cargo fmt --all -- --check`。
- [ ] 运行 `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [ ] 运行 `rtk cargo test --workspace`。
- [ ] 启动真实 stdio 后端，发送 ACP 初始化请求并确认 stderr 有 Trace、stdout 只有 JSON-RPC。
- [ ] 启动真实 HTTP 后端，通过真实 WebUI Prompt 验证 app、Kernel、Provider 使用同一 Trace。

# 日志配置与紧凑格式实施计划

> **执行要求：** 在当前会话内逐项执行；项目禁止 SubAgent 和 worktree。

**目标：** 支持由 TOML 或 `RUST_LOG` 配置日志过滤，并输出包含本地毫秒时间、Trace ID、源码文件和行号的紧凑日志。

**架构：** `config` crate 提供不可变 `LoggingConfig`，`app` crate 在应用组合前解析来源优先级。自定义 tracing Layer 类型化保存 Trace ID，自定义 `FormatEvent` 仅负责展示，不解析 span 格式字符串。

**技术栈：** Rust 2024、serde、figment、tracing、tracing-subscriber、chrono timer。

## 全局约束

- 所有新增函数必须有英文函数级注释。
- 测试只放在 `tests/` 或精确的 `#[cfg(test)] mod tests` 中；正文不能增加测试支撑代码。
- 不创建大量短小 helper；单自定义类型参数的行为优先放入对应类型的 `impl` 或标准 trait。
- tracing 事件宏必须使用 `tracing::info!()` 等全路径形式，正文不得导入事件宏。
- 不记录完整 prompt、response、凭证、请求头或逐 token 事件。
- 不创建 commit。

---

### Task 1：增加类型化日志配置

**文件：**

- 新建：`crates/config/src/logging.rs`
- 修改：`crates/config/src/config.rs`
- 修改：`crates/config/src/lib.rs`
- 测试：`crates/config/tests/loading.rs`

**接口：**

- 产出：`LoggingConfig { filter: String }`
- 默认：`LoggingConfig::default().filter == "info"`

- [ ] 先增加 TOML 加载和默认值测试。
- [ ] 运行 `rtk cargo test -p config --test loading logging`，确认因类型或字段缺失失败。
- [ ] 实现 `LoggingConfig` 并接入 `AppConfig`。
- [ ] 重跑测试并确认通过。

### Task 2：实现紧凑 formatter 和 Trace 上下文

**文件：**

- 修改：`Cargo.toml`
- 修改：`crates/app/src/logging.rs`
- 测试：`crates/app/tests/logging.rs`

**接口：**

- `LoggingFactory::from_config(&LoggingConfig) -> Result<Self, LoggingError>`
- `LoggingFactory::build(writer) -> impl Subscriber`
- 生效来源：非空 `RUST_LOG` 优先，否则 `LoggingConfig::filter`

- [ ] 先增加日志格式、文件行号、Trace 去重和过滤行为测试。
- [ ] 运行 `rtk cargo test -p app --test logging`，确认新断言失败。
- [ ] 增加 chrono formatter feature、Trace context Layer 和紧凑事件 formatter。
- [ ] 删除 ACP 生命周期正文中重复的 Trace ID。
- [ ] 重跑日志测试并确认通过。

### Task 3：调整启动顺序并验证环境覆盖

**文件：**

- 修改：`crates/app/src/main.rs`
- 修改：`claw.toml`
- 测试：`crates/app/tests/logging.rs`

**接口：**

- 主进程先得到 `ConfigHandle`，再安装日志，然后调用 `ApplicationFactory::new(handle)`。
- `RUST_LOG` 只在进程启动时读取一次。

- [ ] 先增加启动进程测试，分别验证配置 filter 和 `RUST_LOG` 覆盖。
- [ ] 运行目标测试，确认当前启动顺序无法满足测试。
- [ ] 调整入口组合顺序并补充 `claw.toml` 的 `[logging]` 示例。
- [ ] 重跑目标测试并确认通过。

### Task 4：完整验证

**文件：** 无新增文件。

- [ ] 运行 `rtk cargo fmt --all -- --check`。
- [ ] 运行 `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [ ] 运行 `rtk cargo test --workspace`。
- [ ] 运行 `rtk git diff --check`。
- [ ] 使用真实 `claw.toml` 启动 HTTP 后端，核对日志格式、文件行号及 Prompt Trace 链路。
- [ ] 启动 stdio 后端，确认 stdout 保持 ACP 协议纯净、日志只进入 stderr。

# 日志颜色配置实施计划

> **执行要求：** 在当前会话使用 superpowers:executing-plans 逐项执行；项目禁止 SubAgent、worktree，以及未经用户明确授权的 commit。

**目标：** 通过 `logging.color` TOML 布尔值显式控制 tracing 日志的 ANSI 颜色，缺省关闭颜色。

**架构：** `config::LoggingConfig` 负责持有和反序列化颜色开关；`app::LoggingFactory` 将不可变颜色策略传给 tracing formatter。`RUST_LOG` 只覆盖 filter，颜色始终来自配置。

**技术栈：** Rust 2024、serde、toml、tracing、tracing-subscriber。

## 全局约束

- 新增函数必须有英文函数级注释，非平凡修改必须说明原因。
- 测试只放在 crate 的 `tests/` 集成测试目录。
- 不增加无意义的 helper，颜色作为 `LoggingFactory` 的类型状态字段传递。
- 不增加依赖，不改变日志格式、Trace ID、文件行号或 ACP 脱敏逻辑。
- 不创建 commit。

---

### Task 1：扩展日志配置模型

**文件：**

- 修改：`crates/config/tests/loading.rs`
- 修改：`crates/config/src/logging.rs`

**接口：**

- 产出：`LoggingConfig { filter: String, color: bool }`
- 缺省：`LoggingConfig::default().color == false`
- TOML：`[logging] color = true`

- [x] 在 `logging_defaults_to_info_filter` 中增加缺省颜色关闭断言，并在 TOML 加载测试中增加 `color = true` 及对应断言。
- [x] 运行 `rtk cargo test -p config --test loading logging`，确认测试因 `LoggingConfig.color` 不存在而编译失败。
- [x] 为 `LoggingConfig` 增加带 `#[serde(default)]` 的公开 `color: bool` 字段，并在 `Default` 中赋值 `false`。
- [x] 重跑 `rtk cargo test -p config --test loading logging`，确认配置测试通过。

### Task 2：将颜色配置接入日志 formatter

**文件：**

- 修改：`crates/app/tests/logging.rs`
- 修改：`crates/app/src/logging.rs`
- 修改：`crates/app/src/logging/format.rs`
- 修改：`claw.toml`

**接口：**

- 修改：`LoggingFactory::new(filter: impl AsRef<str>, color: bool) -> Result<Self, LoggingError>`
- 保持：`LoggingFactory::from_config(&LoggingConfig) -> Result<Self, LoggingError>`
- 行为：`LoggingFactory::build` 调用 `.with_ansi(self.color)`，`CompactEventFormat` 根据 `Writer::has_ansi_escapes()` 为日志级别着色。

- [x] 调整现有 app 日志测试使用 `LoggingFactory::new("info", false)`，断言默认测试输出不含 `\u{1b}[`。
- [x] 增加 `logging_factory_emits_ansi_when_color_is_enabled`，使用内存 writer 和 `LoggingFactory::new("info", true)`，断言格式化结果包含 `\u{1b}[`。
- [x] 运行 `rtk cargo test -p app --test logging`，确认测试因 `LoggingFactory::new` 尚未接收颜色参数而编译失败。
- [x] 为 `LoggingFactory` 增加 `color: bool` 字段，修改 `new` 和 `from_config`，将该字段传给 `.with_ansi(...)`，并由自定义 formatter 根据 writer 能力为日志级别着色。
- [x] 在根 `claw.toml` 的 `[logging]` 中显式设置 `color = true`，使当前开发配置启用颜色。
- [x] 重跑 `rtk cargo test -p app --test logging`，确认 ANSI 开关测试和既有日志测试通过。

### Task 3：完整验证

**文件：** 无新增生产文件。

- [x] 运行 `rtk cargo fmt --all -- --check`。
- [x] 运行 `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [x] 运行 `rtk cargo test --workspace`。
- [x] 运行 `rtk git diff --check`。
- [x] 核对 diff，确认 `RUST_LOG` 仍只覆盖 filter，颜色仅受 `logging.color` 控制。

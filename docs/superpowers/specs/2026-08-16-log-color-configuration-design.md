# 日志颜色配置设计

## 目标

通过 TOML 明确控制日志是否输出 ANSI 颜色，不再根据 debug 或 release 编译模式推断行为。

## 配置模型

`logging` 增加布尔字段 `color`：

```toml
[logging]
filter = "info"
color = false
```

`color` 缺省时为 `false`，保持非终端、重定向和日志采集场景的纯文本兼容性。配置为 `true` 时，tracing formatter 为时间、级别等支持颜色的内容输出 ANSI 转义序列。

`RUST_LOG` 仍然只覆盖过滤规则，不改变颜色配置。颜色仅由 `logging.color` 决定。

## 模块边界与数据流

- `config::LoggingConfig` 持有 `filter` 和 `color`，并负责 Serde 默认值。
- `app::LoggingFactory` 在构造时保存已解析的颜色开关。
- `LoggingFactory::build` 将开关传给 tracing formatter 的 `with_ansi`；`CompactEventFormat` 读取 writer 的 ANSI 能力并为日志级别着色，现有紧凑字段顺序和 stderr 输出位置不变。
- 不增加新的环境变量，也不增加终端自动探测。

## 兼容性与错误处理

- 旧配置未声明 `color` 时继续正常加载，默认关闭颜色。
- `color` 不是布尔值时沿用现有 TOML 配置解析错误。
- 颜色开关不影响日志过滤、Trace ID、文件行号或 ACP 参数脱敏。

## 测试与验收

- 配置集成测试验证缺省值为 `false`，并验证 TOML 可以显式启用。
- App 集成测试验证关闭时不存在 ANSI 转义序列，开启时存在 ANSI 转义序列。
- 运行格式检查、Clippy、workspace 全量测试和 `git diff --check`。

# 日志配置与紧凑格式设计

## 目标

为应用日志增加 TOML 配置入口，同时保留 `RUST_LOG` 的临时覆盖能力；将默认 tracing 输出改为紧凑、可读、可追踪到源码位置的文本格式。

## 配置模型

顶层配置增加 `logging`：

```toml
[logging]
filter = "info,kernel=debug,provider=debug,hyper=warn"
```

`logging.filter` 完整复用 `tracing_subscriber::EnvFilter` 语法，不额外设计日志级别枚举或模块映射。过滤器来源优先级固定为：

1. 非空的 `RUST_LOG`。
2. `logging.filter`。
3. 默认值 `info`。

无效过滤器属于启动配置错误，应用必须拒绝启动并输出明确错误，不能静默降级。

## 输出格式

每条日志固定为一行：

```text
2026-08-15 23:31:18.192 INFO  [trace-8QsPuixn6nw0jdEsbALJP] crates/kernel/src/provider.rs:176 | completed Provider request deepseek/deepseek-v4-flash in 866 ms
```

字段依次为：

1. 当前系统本地时间，精确到毫秒。
2. 左对齐日志级别。
3. 当前 ACP 操作的 `trace_id`；无 Trace 时显示 `-`。
4. 事件源码文件和行号。
5. 日志正文以及事件自带的非 Trace 字段。

formatter 不展示 span 名称或完整 span 链。Trace 仍通过 root span 传播，但 formatter 只读取并输出一次 `trace_id`。日志正文不再重复 Trace ID。

工作区源码保留相对路径。依赖库若提供绝对源码路径，只展示文件名，避免输出本机 Cargo registry 路径。元数据缺少文件名时使用 tracing target，缺少行号时仅输出可用位置。

## 模块边界

- `config::LoggingConfig`：只负责 TOML 数据模型和默认 filter。
- `app::LoggingFactory`：解析环境变量与配置优先级，构造不可变 subscriber。
- `app::CompactEventFormat`：负责单行文本展示。
- `app::TraceContextLayer`：在 span 创建时提取 `trace_id` 并保存为类型化 span 扩展，避免解析 formatter 生成的字符串。
- `main`：先加载配置，再安装日志 subscriber，然后用同一份 `ConfigHandle` 构造应用。

## 错误处理

- `RUST_LOG` 不是 Unicode：返回 `LoggingError::InvalidEnvironment`。
- 生效的 filter 无法解析：返回 `LoggingError::InvalidFilter`。
- subscriber 已安装：保留现有 `LoggingError::AlreadyInstalled`。
- 日志初始化前的配置加载错误由进程入口直接返回，不能通过尚未安装的 subscriber 记录。

## 测试与验收

- 配置集成测试验证 `logging.filter` 的默认值和 TOML 加载。
- 日志集成测试验证模块过滤、紧凑格式、文件行号、无 Trace 占位符和 Trace 仅出现一次。
- 进程级测试验证 `RUST_LOG` 覆盖配置文件。
- 运行 workspace 格式、Clippy 和测试。
- 启动真实后端，检查 stderr 实际输出；stdio 模式 stdout 必须继续只包含 ACP framing。

# Trace 日志设计

## 目标

为每个外部 ACP 操作生成稳定的 `trace_id`，并让该标识贯穿应用入口、ACP、Kernel 和 Provider。HTTP/SSE/WebSocket 与 stdio 使用同一传播模型。日志应足以定位生命周期、重试和错误问题，但不得记录逐 token 数据、完整提示词、完整响应、凭据或认证头。

## Trace 生命周期

- 每个 ACP 请求或通知创建一个独立 Trace；ACP 批量请求中的每条消息分别创建 Trace。
- Prompt Trace 不在立即返回 `PromptResponse` 时结束，而是在后台 Kernel Run 完成、失败或取消后结束。
- 一个 Prompt Trace 可以包含多个 Turn。`session_id`、`run_id`、`turn_id`、Tool 和 Provider 请求是同一 Trace 下的关联上下文，不替代 `trace_id`。
- Provider 获取流、重试和消费流都必须保留父 span，避免只覆盖 HTTP 建连而丢失流式阶段。
- 非 Prompt ACP 操作在响应成功或失败后结束。HTTP 静态资源、健康检查和服务启停只记录入口生命周期日志，不伪造 Kernel 调用链。

## 类型与模块边界

- `protocol` 定义 `TraceId`，并在 `IdKind` 增加 `Trace`。Trace ID 使用现有 `IdGenerator` 生成，保持标识生成策略统一。
- `app` 创建并共享同一个 `IdGenerator`，初始化写入 stderr 的全局 tracing subscriber。stdio 的 stdout 仅用于 ACP JSON-RPC，日志不得写入 stdout。
- `acp` 定义传输类型和 Trace 操作上下文。它在协议 handler 入口根据 JSON-RPC method、request id 和传输类型创建根 span，并显式 instrument Prompt 后台任务。
- `kernel` 在 Run、Turn 和 Provider 边界创建子 span，并只在开始、结束、失败、取消、重试等关键状态写日志。
- `provider` 的通用适配层负责覆盖流获取与流消费阶段；具体 Provider 继续保留既有 GenAI span，但不要求每条日志重复 `trace_id`。

## 日志格式与级别

- tracing 事件宏必须使用 `tracing::info!()`、`tracing::warn!()` 等全路径调用，不直接导入事件宏。
- 事件正文使用普通格式化文本，例如 `tracing::info!("completed Turn {}", turn_id)`，不使用 `tracing::info!(turn = %turn_id, "completed Turn")` 字段风格。
- 仅根 span 以及必要的子 span 使用最少结构化字段，以便 formatter 自动携带 `trace_id` 和层级上下文。
- `INFO`：服务启停、ACP 操作、Run、Turn、Provider 请求的开始和最终状态。
- `WARN`：可恢复重试、取消、协议异常和降级行为。
- `ERROR`：导致操作失败且无法继续的错误。
- `DEBUG`：排障需要但正常运行不应展示的状态；不得记录完整请求或响应正文。
- 默认级别为 `info`，允许通过 `RUST_LOG` 在启动时覆盖，不实现热更新。

## 错误与安全

- Subscriber 初始化失败必须终止启动，防止服务在用户以为有日志时静默运行。
- Trace ID 生成或校验失败必须在进入业务逻辑前返回错误。
- 日志错误文本不得包含 API key、Authorization、Cookie、完整 Prompt、完整模型响应或逐 token 内容。
- Provider 重试日志记录 provider/model、尝试次数、延迟和安全错误摘要，不记录请求体。

## 验证

- `protocol` 集成测试验证 `TraceId` 的校验、显示和生成前缀。
- `acp` 集成测试通过真实 ACP in-process 连接验证每条请求生成独立 Trace，并验证 Prompt 后台任务继承同一 Trace。
- `kernel` 集成测试捕获 tracing 输出，验证 Run、Turn 和 Provider 日志处于同一个 `trace_id` span 中，且输出不包含 Prompt 正文。
- 完成后运行格式化、workspace clippy、workspace 测试，并分别启动真实 stdio 与 HTTP 后端做端到端日志验证。

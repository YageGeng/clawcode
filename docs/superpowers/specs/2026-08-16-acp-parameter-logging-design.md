# ACP 参数日志设计

## 目标

在不增加 `INFO` 噪音的前提下，为所有进入应用 handler 的 ACP v2 request 和 notification 增加完整参数日志，便于通过同一个 Trace 还原协议调用现场。

## 日志级别与格式

ACP 生命周期继续使用 `INFO`：

```text
2026-08-16 10:00:00.000 INFO  [trace-...] crates/acp/src/trace.rs:120 | started ACP request session/prompt:10 over http
```

参数使用 `DEBUG` 单独输出：

```text
2026-08-16 10:00:00.001 DEBUG [trace-...] crates/acp/src/trace.rs:125 | parameters for ACP request session/prompt:10 over http: {"sessionId":"...","prompt":[...]}
```

通过现有过滤器开启：

```toml
[logging]
filter = "info,acp::trace=debug"
```

参数不截断。完整 prompt、shell 命令、资源内容和扩展消息参数允许进入 ACP `DEBUG` 日志。`INFO` 不输出参数。

## 安全边界

序列化后的 JSON 必须递归遍历 object 和 array。字段名忽略大小写、下划线和连字符后，以下字段及以这些名称结尾的字段替换为字符串 `[REDACTED]`：

- `api_key`
- `token`、`access_token`、`refresh_token`、`session_token`
- `authorization`
- `cookie`
- `password`
- `secret`

复数统计字段（例如 `input_tokens`、`output_tokens`）不属于凭证，不脱敏。日志不得输出被替换前的值。

该设计是对 `AGENTS.md` 原有“不得记录完整 prompt/response”规则的明确例外：仅 ACP request/notification 的 `DEBUG` 参数日志允许记录完整 prompt；凭证、认证头和其他敏感字段的保护规则仍然有效。

## 模块设计

- `AcpTraceFactory::request` 同时接收 responder 与序列化参数。
- `AcpTraceFactory::notification` 同时接收 method 与序列化参数。
- `AcpLogParameters` 负责按需序列化、递归脱敏和保存单行 JSON。
- `SensitiveParameterName` 封装字段名归一化与敏感字段判断。
- `AcpOperationTrace::start` 在 root Trace span 内先记录 `INFO` 生命周期，再记录 `DEBUG` 参数。
- `AcpExtensionRequest` 只暴露其原始 `parameters` 引用，不重复序列化 method。

当 `acp::trace` 的 `DEBUG` 未启用时，不执行参数序列化与脱敏，避免为默认 `INFO` 日志支付大对象序列化成本。参数文本使用 `Arc<str>` 保存，Prompt 后台任务克隆 Trace 时不会复制完整参数。

## 错误处理

参数日志不能改变 ACP 请求结果。若 serde 无法序列化参数，handler 继续执行，并在 `DEBUG` 输出参数不可用及序列化错误。生成 Trace ID 的既有错误语义保持不变。

## 测试与验收

- 使用真实 ACP 连接发送 initialize、session/new、session/prompt 和 session/cancel。
- 验证每个已发送操作都有参数日志且继承对应 Trace。
- 验证完整 prompt 与普通嵌套字段可见。
- 验证 API key、token、authorization、cookie、password 和 secret 的嵌套值均不可见，并出现 `[REDACTED]`。
- 验证 `INFO` 生命周期内容不重复参数。
- 运行格式检查、Clippy 和 workspace 测试。
- 启动真实 HTTP/WebUI 后端，以 `acp=debug` 发起 Prompt 并检查实际日志。

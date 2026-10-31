# rig-core 本地遥测验收

用户选择将 rig-core 遥测接入现有本地日志。本次在 `LoggingFactory` 中接入原生 span 的元数据记录与关闭摘要，未安装 OpenTelemetry exporter、未配置外部采集器、未新增配置开关或运行时依赖；未创建 commit。

## 行为

- 在原生 `rig::completions`、`rig::modalities` span 或 completion-parent span 关闭时输出一条 `Rig telemetry closed`；重复进入/退出 span 和释放部分句柄不产生额外摘要。
- 沿用现有本地时间、级别、Trace ID 和来源格式，写 stderr；不污染 ACP stdio stdout。
- 输出供应商、请求/响应模型、可用的响应/传输请求 ID、stream 标记，以及上游已报告的输入/输出、缓存读写、tool-use prompt、reasoning 计数。
- 缺失计数省略，明确的 0 保留。elapsed_ms 是客户端原生 span 创建到关闭的耗时；closed 不等于成功，结果应结合相同 Trace 下的完成/错误/取消日志判断。
- Rig 摘要明确排除 system instructions、输入/输出 messages、认证 headers、Native/encrypted payload；即使某调用方显式开启上游 content telemetry，摘要也只读取允许的元数据。
- 沿用 `[logging] filter` 与优先级更高的 RUST_LOG。默认 info 可见；`info,rig::completions=off,rig::modalities=off` 可关闭原生 Rig 摘要。若只启用 Rig 而过滤掉应用的 Trace span，相关日志可能显示 `[-]`。
- 既有 ACP DEBUG 参数日志保留其独立脱敏规则，上述正文排除不代表禁用该现有功能。

## 验证

| 验证 | 结果 |
|---|---|
| 新增日志回归 | 3 项先失败后通过：原生调用/parent adoption 的最终元数据和敏感内容排除、关闭时机和未知计数、现有过滤规则 |
| 完整 app logging 测试 | 9 passed，原有时间、颜色、Trace、stdio 和环境覆盖语义保持 |
| workspace all-targets | 782 passed、0 failed、2 ignored，107 suites；ignored 为既有外部 MCP 互操作测试 |
| workspace Clippy -D warnings | 通过，无警告 |
| fmt 与 diff 检查 | 通过 |

日志证据：`/tmp/rig-local-telemetry/red.log`、`green.log`、`workspace.log`、`clippy.log`。新增 rig-core 仅为 app 的测试依赖，用于验证真实上游 span 契约；生产没有新增依赖。

## 真实生产调用

重新构建当前 production app，使用原 DeepSeek 配置的 0600 副本、独立工作/数据目录和 Chrome profile。保留 provider/model/模型参数；测试时 RUST_LOG=info，以明确验证普通日志不包含正文。

证据目录：`/tmp/clawcode-rig-telemetry-6ou7tbs0`。实际模型为 `deepseek/deepseek-flash`，页面收到预期回答，浏览器脚本退出 0，未捕获浏览器异常。

一次请求产生一条摘要，含有效 Trace ID、stream=true、provider=deepseek 和请求/响应模型。实测 elapsed_ms=914，input_tokens=4132、output_tokens=9、cache_read_tokens=2432、reasoning_tokens=0；cache_write_tokens 未上报，因此未打印。

`evidence/telemetry-validated.json` 核对这些计数与真实 ACP usage 逐项一致，并确认请求/回答的测试标记未出现在整个 INFO 日志中。其他证据为 `evidence/app.log`、`updates.json`、`results.json` 和 `telemetry-final.png`。

测试 app 和 Chrome 已退出，保留临时证据。此次没有测试远端 OTLP、其他供应商或 OAuth，也没有把成功调用的 span 关闭当作所有失败/取消路径的 live 验证。

# rig-core 迁移修复后的真实端到端验收

本轮重新构建当前工作区生产 app 和 WebUI，使用 crates.io rig-core / rig-reqwest `0.46.0-rc.1` 以及已完成的五项复审修复。结果：**基础浏览器 12 项 + 真实 MCP Sampling 1 项通过**；两个浏览器脚本均退出 0，未捕获浏览器异常。

## 环境

- 模型键 `deepseek/deepseek-flash`，实际 upstream_id 为 `deepseek-flash`；保留现有 thinking enabled 和 reasoning_effort high，未更换 provider、服务地址或凭据渠道。
- `cargo build --offline -p app --bin clawcode --locked`、`npm --prefix web run build` 均通过。
- 证据根目录：`/tmp/clawcode-rig-live-z3yvgrqz`。配置副本为 0600，工作目录、数据目录和 Chrome profile 独立。只在副本将 keep_recent_tokens 调为 128，以便短历史能够执行手动压缩；项目 claw.toml 未修改。
- `evidence/environment.json` 记录二进制 SHA-256、Cargo.lock SHA-256、Rig package 来源和非秘密模型参数。

## 基础生产浏览器场景

运行实际 `target/debug/clawcode serve` 与 `web/dist`，由仓库 `crates/app/tests/web/provider_migration.e2e.mjs` 驱动。没有模拟模型或替代 app 后端。

| 场景 | 实测 |
|---|---|
| Bootstrap | 当前真实 DeepSeek 模型 |
| 两次工具调用 | 两个不同调用 ID、不同回合；实际工具名为 `bash`，command 分别为 `printf RIG_MIGRATION_ONE`、`printf RIG_MIGRATION_TWO`；结果精确匹配，exit_code=0、is_error=false |
| 中间状态 | 17 个 DOM 样本，4 个不同可见状态；3 个非空 reasoning chunk 均能在实际页面样本中找到 |
| Usage 重连 | 重载前后完全一致；首次 Run 总计 12,778、输入 12,668、输出 110、缓存读 10,496、推理 5，缓存写保持未知 |
| 取消 | 106ms 回到非运行态；后台 13 次 Provider 启动、12 次完成，另一次被取消 |
| 取消后继续 | 新的真实请求成功 |
| follow-up / steer | 忙时入队、本轮补充、消费及终态成功 |
| DeepSeek 图片回退 | 实际图片经不支持图片提示降级，返回 RIG_IMAGE_FALLBACK，没有声称视觉识别成功 |
| 真实压缩 | 摘要 input=859、output=702、total=1561、cache read=0，cache write 未报告 |
| 压缩后继续及 tree | 继续请求成功；浏览器树投影不含 model_message 或 retainedTail 原生内容 |
| fork / close-resume | fork 和重开的原会话均能继续请求真实模型 |

脚本结果中的 Bootstrap、工具流式、重连、取消、取消后请求、队列、图片、压缩、压缩后请求、tree、fork、重开共 12 项；上表合并了相关观测，没有重复计数。

## 真实 MCP Sampling

第二个隔离配置保留相同 provider，只增加本地 stdio MCP peer。peer 是实际协议参与者；生产 app、Kernel 和模型没有被替换。

`生产 WebUI → DeepSeek 工具调用 → stdio MCP sample → sampling/createMessage → 生产 Session::sample_mcp → DeepSeek → MCP 工具结果 → 页面最终回答`

- 实际只发出一次 Sampling 请求，maxTokens=2048。
- 回调返回 model=`deepseek/deepseek-flash`、stopReason=`endTurn`、role=`assistant`，文本精确为 `RIG_MCP_SAMPLED_2026`。
- 工具结果包含真实回调内容，页面最终回答为 `RIG_MCP_LIVE_DONE`。
- 后台 3 次 Provider 启动、3 次完成：外层调用、内层采样、外层续答；该场景无 WARN/ERROR 日志。
- 临时 peer 与浏览器脚本位于 `/tmp/clawcode-rig-live-z3yvgrqz/mcp/server.mjs`、`/tmp/clawcode-rig-live-z3yvgrqz/mcp/browser.mjs`；生产源码未增加测试专用扩展或字段。

## 证据与告警

- `/tmp/clawcode-rig-live-z3yvgrqz/evidence/results.json`：基础 12 项结果。
- `/tmp/clawcode-rig-live-z3yvgrqz/evidence/validated-results.json`：真实工具输入/输出、不同调用 ID、页面 reasoning 和 MCP 回调结果的额外断言。
- `/tmp/clawcode-rig-live-z3yvgrqz/evidence/updates.json`、`stream-samples.json`、`tree.json`：真实 ACP 更新、页面中间状态和树投影。
- `/tmp/clawcode-rig-live-z3yvgrqz/evidence/tool-loop.png`、`final.png`：生产页面截图。
- `/tmp/clawcode-rig-live-z3yvgrqz/mcp/evidence/results.json`、`updates.json`、`mcp-wire-*.jsonl`、`mcp-final.png`：MCP 真实链路及页面。

基础脚本直接关闭 fork 和原会话后，两次并行的 `_clawcode/mcp/status` 查询收到 SessionNotFound，产生 6 行 WARN（每次请求 3 行）。这两次后台查询未导致场景断言失败，也没有浏览器未捕获异常；保留原日志，没有报告为“后台零错误”。没有发现模型请求或工具执行失败。

## 范围与清理

本次补齐了当前 crates.io 发布版及复审修复后的上述 live 场景。它不证明其他供应商/ChatGPT OAuth、真实供应商拒绝或畸形 JSON、最终内容修正、MessageEnd 扩展改写、自定义 HTTP middleware 等组合 live 边界；这些仍依赖已有定向回归或保留未验状态。

本轮启动的两个 app 进程、stdio MCP peer 和独立 Chrome 已退出；保留隔离证据与会话数据。未创建 commit。

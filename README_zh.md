# clawcode

一个基于 pi Turn 生命周期与会话模型重新设计的模块化 Agent。运行时通过
Agent Client Protocol（ACP）v2 提供 stdio、HTTP/SSE 和 WebSocket，并包含本机
亮色 Agent WebUI。

## 架构

| Crate | 职责 |
|---|---|
| `app` | 生产组合根、CLI、bootstrap 与 WebUI 静态服务 |
| `protocol` | 公共领域类型、Turn/消息事件与产品标识 |
| `config` | 不可变的 TOML 配置 |
| `provider` | LLM API 适配与 provider factory |
| `prompt` | 不可变的 pi 兼容 System Prompt、指令与 Template 资源 |
| `kernel` | Session 所有权、串行运行、Turn 循环和工具编排 |
| `tools` | 公共 `AgentTool` 接口、registry 与 pi 兼容编码工具 |
| `mcp` | Session 级 MCP stdio 与 Streamable HTTP 工具 |
| `skill` | 确定性的 pi 兼容 `SKILL.md` 发现与显式调用 |
| `extension` | 非 UI 的 pi 生命周期接入点和插件注册 |
| `store` | 追加式 pi v4 JSONL、lane、record、fact、branch 与 fork |
| `acp` | ACP v2 映射及 stdio、HTTP/SSE、WebSocket transport |
| `web` | 基于 React/TypeScript 与 ACP WebSocket 的 Agent 工作台 |

保留的 `config` 和 `provider` 通过 factory 接入新 Kernel。每个运行时消息和事件
都携带 `turn_id` 与十进制字符串毫秒时间戳；持久化消息还保存创建时间、首次输出
时间与末次输出时间。

## 环境要求

- Rust stable（参见 `rust-toolchain.toml`）
- Node.js 22.12 或更高版本（仅构建 WebUI 时需要）
- 配置模型对应的 API Key

## 配置

配置只在启动时读取一次。搜索顺序为：

1. 显式配置路径环境变量。
2. 平台配置目录下的产品目录。
3. 仓库内的 TOML 文件。

示例：

```toml
active_model = "deepseek/deepseek-v4-pro"
approval = "request_approval"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
provider_type = "openai-completions"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }

[[providers.models]]
id = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
input = ["text"]
context_tokens = 1000000
max_output_tokens = 384000

[app.recovery]
# 限制一次出站 ACP 帧可消费的源 Session 更新数。
max_batch_size = 512

[kernel]
max_turns = { type = "unlimited" }
# max_turns = { type = "limited", turns = 64 }

[subagent]
# 后台子代理的回复如何进入忙碌中的主代理："follow_up" 或 "steer"。
reply_as = "follow_up"

[logging]
filter = "info,acp::trace=debug"
color = true

[retry]
enabled = true
max_retries = 3
base_delay_ms = 2000
max_retry_delay_ms = 60000

[retry.provider]
timeout_ms = 120000
max_retries = 2
max_retry_delay_ms = 60000

[compaction]
enabled = true
reserve_tokens = 65536
keep_recent_tokens = 20000

[prompt]
load_project_instructions = true
load_templates = true
template_paths = []

[skills]
include_instructions = true
paths = ["./team-skills"]

[[skills.rules]]
name = "manual-review"
enabled = false

[extensions]
enabled = ["command-guard"]

[tools]
enable_fs = true
enable_shell = true
enable_skill = true

[[mcp_servers]]
enabled = true
name = "example"
protocol = "auto"
command = "example-mcp-server"
args = []
startup_timeout_sec = 30
request_timeout_sec = 120
request_total_timeout_sec = 300

[[mcp_servers]]
enabled = true
name = "remote"
protocol = "auto"
url = "https://example.com/mcp"
startup_timeout_sec = 30
request_timeout_sec = 120
request_total_timeout_sec = 300
```

省略 `[kernel]` 时，`max_turns` 默认为 `unlimited`。如需防止工具循环无限
执行，可使用 `limited` 并将 `turns` 配置为正整数。

模型的 `id` 是 `active_model`、Session 元数据和模型选择使用的稳定本地标识。
如果上游 API 要求不同的 `model` 参数，可配置可选的 `upstream_id`；省略时请求继续
使用 `id`。

每个模型上限只有一个含义。`context_tokens` 是模型的输入上限，`max_output_tokens`
是输出能力，两者默认取自 rig 目录，且只是元数据。`request_max_tokens` 是每个 Turn
发送的 `max_tokens`，不得大于 `max_output_tokens` 或 `compaction.reserve_tokens`，
默认取两者中较小值。压缩会把输入控制在 `context_tokens` 减去 `reserve_tokens` 以下，
因此在 reserve 之内的预算能保证输入加输出仍在窗口内。需要更长响应时，同时提高
`compaction.reserve_tokens`（压缩会更早触发）和 `request_max_tokens`。关闭压缩后
输入不再受限，该保证不适用。

升级影响：以前每个 Turn 把 `max_output_tokens` 作为 `max_tokens` 发送，现在发送
`request_max_tokens`，通常更小（`max_output_tokens = 384000` 的模型在内置的 16384
reserve 下现在请求 16384）。要恢复更长的响应，请提高
`compaction.reserve_tokens`；如有需要，再把 `request_max_tokens` 设到不超过该 reserve
的值。

Provider 通过 rig 的 provider registry 解析。provider 的 `id` 是 `provider/model`
选择使用的本地键，因此任何非空、不含 `/`、首尾无空白的名字都可用；请保持稳定，
因为已保存的 Session 会引用它。可选的 `preset` 指定决定协议细节与模型元数据的 rig
预设，默认等于 `id`。`provider_type`（`responses`、`openai-completions`、
`anthropic`、`gemini`）选择协议。rig 已知的预设可以省略 `provider_type` 与
`base_url`；在 rig 目录中的模型可以省略上限与 `input`。配置值始终优先于目录。
最小的 provider 写法是：

```toml
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
api_key = { env = "DEEPSEEK_API_KEY" }

[[providers.models]]
id = "deepseek-flash"
```

| 场景 | Provider 字段 |
|---|---|
| 官方端点 | `id = "deepseek"` |
| 已知预设的另一种协议 | `id = "moonshot"`、`provider_type = "anthropic"` |
| 已知预设的代理 | `id = "ds-proxy"`、`preset = "deepseek"`、`base_url = "…"` |
| 通用兼容端点（vLLM、LiteLLM、网关） | `id = "corp"`、`provider_type = "openai-completions"`、`base_url = "…"`，模型必须写上限 |
| 网关以另一种协议转发已知预设的模型 | `id = "litellm"`、`preset = "anthropic"`、`provider_type = "openai-completions"`、`base_url = "…"` |

当 rig 对该预设与协议的组合没有可配置预设时，`base_url` 必填，并使用该协议的通用
方言；如果 rig 认识这个预设、只是不支持所选协议，启动时会输出写明 provider 与协议
的警告，因为该预设的方言细节不再适用。省略 `provider_type` 时使用预设的 rig 默认
协议；rig 不认识的预设仍按 `responses`。

升级说明：如果 provider 的 `id` 恰好是 rig 预设、但不在以前的硬编码列表中
（例如 `openrouter`、`groq`、`mistral`、`xai`、`ollama`），它以前走通用 OpenAI
方言，且省略 `provider_type` 时使用 Responses 路由；现在改为使用该预设的 rig 方言
与默认路由。要保留旧行为，可把 `preset` 设为 rig 不认识的名字（例如
`preset = "custom"`）并保留 `base_url`。

Gemini 的 rig 预设是 `gcp.gemini`：官方端点使用 `preset = "gcp.gemini"`；兼容端点
使用 `provider_type = "gemini"` 加 `base_url`。

Session 的 thinking 级别会作为可移植的推理强度发送，由 rig 翻译为各家参数。没有
持久化选择的 Session 从 `medium` 开始；压缩摘要与 MCP sampling 始终不发送推理
参数。`off` 不发送推理参数，交由模型默认值和 `extra_param` 决定。当 rig 目录列出
模型支持的档位时，级别会吸附到不高于它的最高档，没有更低档时取最低档：在 DeepSeek
（low、high、max）上 Minimal 与 Medium 发送 low，XHigh 发送 high，因此更高的级别
永远不会发送更少的推理。目录未列出档位的模型，若提供方不接受所选级别，该参数会被
跳过而不会让请求失败。

恢复 Session 时如果持久化的模型已不在配置中，Session 会以之前选中的或当前活动
模型打开，并记录一条警告。`extra_param` 按键覆盖翻译后的参数，覆盖按顶层键进行；
例如 `extra_param` 中的 `thinking` 会整体替换生成的 `thinking`。因此模型
`extra_param` 里的 `thinking` 或 `reasoning_effort` 会为所有 Session thinking
级别钉住推理；删除这些键才能让 Session 级别生效。

内置 `subagent` 工具与 `background_subagent` 的 `spawn` 动作支持可选的 `model`
参数，用 `provider/model` 指定一个已配置模型。引用按第一个 `/` 切分，因此
`openrouter/aion-labs/aion-2.0` 可用。省略时子代理使用父会话当前模型；它始终继承
父会话的 thinking 级别与工具。未配置的模型会被拒绝，并返回已配置模型列表。

`background_subagent` 的回复默认以 follow-up 送达主代理：主代理忙碌时，回复会等到
其当前 Run 即将结束时再插入。设置 `[subagent] reply_as = "steer"` 则改为在主代理
当前 Turn 结束后插入。前台 `subagent` 的回答就是工具结果本身，不受此设置影响。

## MCP 发现与输出

MCP 与 Pi 支持相同的四个版本：`2025-11-25`、`2025-06-18`、`2025-03-26`、
`2024-11-05`。省略 `protocol` 或配置 `"auto"` 时请求 `2025-11-25`；显式版本只作为
协商偏好。两种策略都接受服务器在一次 initialize 握手中选择的任一受支持版本。
界面显示实际协商版本，HTTP Session 过期不会悄悄重新握手。

旧 `2026-07-28` 配置应改为 `auto`；删除 `mrtr_max_rounds`，将
`mrtr_total_timeout_sec` 改为 `request_total_timeout_sec`，默认 300 秒。
旧字段会明确报迁移错误。正常请求进度可重置活动超时，但不会延长总时限。

工具默认 `exposure = "direct"`；`"deferred"` 通过本地 `tool_search` 发现后加载，
`"hidden"` 不向模型暴露。逐工具覆盖只匹配原始远端名称，可覆盖服务器默认值：

```toml
[[mcp_servers]]
name = "github"
url = "https://example.com/mcp"
exposure = "deferred"

[mcp_servers.tool_exposure]
get_me = "direct"
delete_repository = "hidden"
```

搜索只加载**下一次模型请求**的工具定义，不执行远端工具；同批调用仍使用原来的
声明集合。单次搜索最多新增 32 KiB 的完整定义，这不是长会话累计上限，也没有自动
淘汰机制。显式加载/禁用偏好随 reconnect、resume、分支导航、fork 和 compaction
恢复。服务器注解仅作为提示展示，不能授予批准。

模型可使用 `list_mcp_resources`、`list_mcp_resource_templates` 和
`read_mcp_resource`。列表从已经完整发现的目录快照分页，默认 50 条、最多 200 条，
每页同时限制为 20 KiB；这不是远端按需分页。游标绑定 Session、目录版本、种类和
服务器过滤条件。读取将原 URI 交给指定服务器，支持未列出的模板 URI。
Hidden 服务器即使开放个别工具，也不会开放资源。

MCP、搜索和资源读取的结果在结果 hook 之后统一结算：文本合计最多 20 KiB，包含
JSON 文本投影和文件位置；媒体整块保留，最多 4 块、编码数据合计 8 MiB。
超限时尝试保存完整规范化 JSON，序列化大小最多 64 MiB。完整文件采用私有临时文件的
保留方式，可能被系统清理，不提供下载服务。保存失败单独显示，不重跑远端调用。
这些限制不代表传输层内存上限或模型 token/媒体计费上限。进度文本最多 1 KiB，
不创建完整输出文件。

## Prompt 资源

每个新 Session 会冻结一份 pi 兼容 Prompt 快照，资源文件不支持热更新。项目可用
`.pi/SYSTEM.md` 替换内置 System Prompt，用 `.pi/APPEND_SYSTEM.md` 追加指令；项目
文件不存在时，会读取用户产品配置目录下的同名文件。

指令先读取用户配置目录，再按文件系统根目录到 Session cwd 的顺序读取项目祖先目录。
每个目录按 `AGENTS.override.md`、`AGENTS.md`、`AGENTS.MD`、`CLAUDE.md`、
`CLAUDE.MD` 的优先级选择第一个可读文件。

Prompt Template 从用户配置目录的 `prompts/` 与项目 `.pi/prompts/` 发现。Markdown
Template 可在 YAML frontmatter 中声明 `description` 和 `argument-hint`。正文支持
`$1` 至 `$9`、`$@`、`${@:N}` 和 `${@:N:L}` 参数语法；缺失的位置参数会替换为空字符串。
例如 `/review README.md` 由服务端展开 `review.md`。Skill 使用 `/skill:name`，也只在
服务端展开。WebUI 命令面板只展示 ACP v2 Available Commands 快照，不读取或展开
Prompt 资源。

统一的 Slash Command 目录按四个有序来源组成：Kernel 内置命令、Extension 命令、
Skill 与 Prompt Template。内置命令是 `/compact [custom instructions]`、
`/name [name]` 和 `/session`。Extension 的规范名使用 `/extension-id:command`，
无歧义的短别名会单独公告。命令通过标准 ACP `session/prompt` 提交。直接的命令输入
与输出、以及冻结的 Skill 或 Template 展开，都会保留其 Turn 与毫秒时间用于回放。

## Skill 资源

每个 Session 同样会冻结一份不可变 Skill Catalog。固定的优先级为：配置中的
`skills.paths`、项目 `.pi/skills`、从 Session cwd 向 Git 根逐级查找的
`.agents/skills`、用户产品配置目录的 `skills/`、`~/.agents/skills`，最后是
Extension 路径。项目来源必须通过项目可信判断。配置和规则中的相对路径基于
Session cwd 解析，开头的 `~` 基于用户 home 解析。

Pi 模式来源允许根目录 Markdown 和嵌套 `SKILL.md`；`.agents/skills` 只接受嵌套
`SKILL.md`。发现过程遵守支持的 ignore 文件，按 canonical path 去重，同名冲突保留先
发现的资源，并返回包含来源及 winner/loser 路径的结构化诊断。非法名称和超长描述只
警告，不会隐藏其他方面可加载的 Skill；描述缺失或 frontmatter 非法时跳过该 Skill。

`disable-model-invocation: true` 只会阻止 Skill 进入模型可见的 System Prompt 清单，
不影响用户显式调用。WebUI Skill 页面展示来源、引用基准目录、仅手动调用状态和发现
诊断。Skill 正文只在实际调用时重新读取，列表接口不会返回正文。

## 运行

启动 ACP stdio Agent：

```sh
cargo run -p app --bin clawcode -- stdio
```

使用 Node.js 22.12 或更高版本构建 WebUI，然后启动同源 HTTP 服务：

```sh
cd web
npm install --ignore-scripts
npm run build
cd ..
cargo run -p app --bin clawcode -- serve --web-root web/dist
```

浏览器访问 `http://127.0.0.1:3000`。`GET /acp` 在请求时升级为 WebSocket，否则用于
SSE；`POST /acp` 承载 ACP JSON-RPC 请求；健康检查位于 `GET /health`。该端口同时接受
HTTP/1.1 与明文 HTTP/2（h2c，prior knowledge）；WebSocket 升级仅支持 HTTP/1.1。

WebUI 无认证，因此刻意只监听 loopback；通配、局域网和公网地址会被拒绝。模型、
Skills 与 MCP 配置在界面中只读，均在启动时从 TOML 读取。界面支持文本、Markdown、
资源链接和栅格图片附件；图片由浏览器编码为 ACP Prompt 内容块，不实现 ACP 客户端
文件系统或 terminal callback。缺少 `web/dist` 时，UI 路由返回 HTTP 503，但 ACP 与
health 仍可用。

前端开发时先在 3000 端口启动 Rust 服务，再从 `web/` 运行 `npm run dev`；Vite
会代理 bootstrap 和 ACP WebSocket 流量。浏览器验收只连接正式 `app` 后端及配置的
provider，因此必须提供有效模型配置和凭据。

## 日志

rig 的 model span 关闭时会向 stderr 写入一条本地 `Rig telemetry closed` 摘要。
摘要继承请求的 Trace ID，并包含耗时毫秒数、provider/请求/响应的模型标识、可用的
请求 ID、流式模式，以及上游报告的输入/输出/缓存/推理 token 计数。缺失的计数直接
省略，报告为 0 的保持为 0。span 关闭本身不代表成功，结果请结合关联的 provider
完成/错误/取消日志判断。

现有的 `[logging] filter` 与覆盖用的 `RUST_LOG` 控制这些摘要。例如
`info,rig::completions=off,rig::modalities=off` 关闭 rig 原生摘要；
`warn,rig::completions=info` 在保留其他警告的同时开启完成摘要。需要 Trace 关联时
请保持应用自身的 Trace span 开启。本实现不安装 OpenTelemetry collector、外部
exporter 或额外运行时依赖。即使 SDK 调用方开启内容遥测，Prompt/响应正文、System
指令、认证头和原生/加密载荷都不会被记录。其他应用 span 不会因此新增生命周期日志行。
该排除只适用于 rig 摘要；既有 ACP DEBUG 参数日志仍遵循其独立的递归凭据脱敏策略。

## 存储

Session 使用 pi v4 兼容 JSONL，按 cwd 编码目录保存。项目不创建全局
`index.jsonl`，列表通过扫描 v4 header 获得。存储支持 lane、树导航、branch
读取、fork、全局 name/label fact、operation record 和 torn-tail 恢复。

瞬时失败共享一份 Kernel retry 预算。`[retry.provider] max_retries` 设置时覆盖
`[retry] max_retries`；`[retry] enabled = false` 关闭 retry。服务器返回的
`Retry-After` 会被完整遵守，超出 provider 延迟上限时则被拒绝。`timeout_ms` 是上游
流式空闲超时（含首帧）：设为 0 关闭，省略时使用上游默认值。Compaction 检查点会
保存 `summary`、`tokensBefore`、`readFiles` 与 `modifiedFiles`，并带完整关联时间。
阈值压缩不会重放成功的 Assistant。显式 overflow 与可恢复的 length stop 会移除失败
Assistant 的活动上下文，压缩后最多恢复一次。旧版按 Turn 数保留的 compaction 字段
不兼容。

Provider 适配层使用 crates.io 的 `rig-core` 与 `rig-reqwest`，锁定在 `0.46`
（`>=0.46.0, <0.47.0`），因此 `cargo update` 只接受 0.46.x 补丁版本。`Cargo.lock`
记录了本应用验证过的版本。1.0 之前的次版本可能改变 API 与 Session 文件中保存的
原生 Assistant 消息，因此升级到 0.47 或更高版本是对两个 crate 一起的显式范围变更，
完成后再运行 workspace 测试与 Clippy。模型输入能力优先取配置，其次取 rig 目录。
每个请求都会构建上游模型；HTTP 连接池共享，每个 provider 的多个模型共享同一个
authenticator。

用量沿用上游的可选计数：输入 token 已包含缓存读取与写入，它们是子集而非额外输入。
缺失的计数显示为未知，报告为 0 的保持为 0。新的 JSONL 用量记录使用
`rig-core-v1` 信封。旧记录在读取时按保守策略解释，不会重写文件；旧版本应用不一定
能读取新记录。原生推理内容与 provider 标识会被保留用于回放，但不会进入浏览器载荷。
编辑 assistant 内容会使其原生回放数据失效。

可信的编译期 Extension 通过 `ExtensionRegistrar::register_http_middleware` 注册
`HttpMiddleware` factory。它们可以检查并修改完整 header、原始 body 与响应，包括
认证头。旧的 provider request/header/response hook 点与 header 白名单已移除。
factory 会收到请求的 `ExtensionContext`，并在每次尝试时执行；认证流量使用基础
transport。

ChatGPT 订阅认证使用上游 `FileTokenStore`，需要一个独立的绝对目录，例如：

```toml
[[providers]]
id = "chatgpt"
display_name = "ChatGPT"
provider_type = "responses"
base_url = "https://chatgpt.com/backend-api/codex"
auth = { type = "chatgpt", token_store_dir = "/absolute/path/to/clawcode-chatgpt" }

[[providers.models]]
id = "your-model-id"
context_tokens = 128000
max_output_tokens = 16384
```

不要把该认证配置与 `api_key` 同时使用。旧的 `codex`/`auth_file` 配置会被拒绝；
已有 Codex 凭据不会被读取或迁移。首次请求需要完成一次设备登录。登录提示写入
stderr 以保持 ACP stdio 可用，刷新与持久化由上游负责。

## ACP 扩展

ACP 原生能力继续使用标准 `session/new`、`session/list`、`session/resume`、
`session/prompt`、`session/cancel` 与 `session/close`。pi 中没有 ACP 原生对应项的
Session Tree 导航、Navigate、Branch、Fork、Compact、队列化 follow-up、Skill
调用/列表和 MCP 状态使用 ACP v2 扩展方法与 `SessionUpdate::Other`。Extension
Command、Skill 和 Prompt Template 通过 ACP v2 原生 `available_commands_update` 公告。
产品元数据记录每个来源、限定别名与命令消息状态。Agent 不声明或调用客户端文件系统
和 terminal callback。恢复扩展会在 WebSocket 客户端重连时恢复进行中的 Session，
用回放游标核对客户端最后应用的 Kernel 序列。

## 验证

```sh
cd web && npm run check && npm run build && cd ..
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## 许可证

项目采用 [MIT](./LICENSE-MIT) 或 [Apache 2.0](./LICENSE-APACHE) 双许可证，任选其一。
第三方依赖信息参见 [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md)。

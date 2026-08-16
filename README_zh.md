# clawcode

一个基于 pi Turn 生命周期与会话模型重新设计的模块化 Agent。运行时通过
Agent Client Protocol（ACP）v2 提供 stdio、HTTP/SSE 和 WebSocket，并包含本机
亮色 Agent WebUI。

## 架构

| Crate | 职责 |
|---|---|
| `app` | 生产组合根、CLI、bootstrap 与 WebUI 静态服务 |
| `protocol` | 公共类型、Turn/消息事件与集中式产品标识 |
| `config` | 启动时读取的不可变 TOML 配置 |
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
都携带 `turn_id` 与十进制字符串毫秒时间戳；完整消息还保存创建时间、首 token
时间与末 token 时间。

## 环境要求

- Rust stable（参见 `rust-toolchain.toml`）
- Node.js 22.12 或更高版本（仅构建 WebUI 时需要）
- 生产模型对应的 API Key

## 配置

配置只在启动时读取，不支持热更新。搜索顺序为：显式配置路径环境变量、平台配置
目录下的产品目录、仓库内 `claw.toml`。

```toml
active_model = "deepseek/deepseek-v4-pro"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
provider_type = "openai-completions"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }

[[providers.models]]
id = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
context_tokens = 1000000
max_output_tokens = 384000

[retry]
enabled = true
max_retries = 3
base_delay_ms = 2000

[retry.provider]
timeout_ms = 120000
max_retries = 2
max_retry_delay_ms = 60000

[compaction]
enabled = true
reserve_tokens = 16384
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

[[mcp_servers]]
enabled = false
name = "example"
command = "example-mcp-server"
args = []
```

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

## Skill 资源

每个 Session 同样会冻结一份不可变 Skill Catalog。固定的优先级为：配置中的
`skills.paths`、项目 `.pi/skills`、从 Session cwd 向 Git 根逐级查找的
`.agents/skills`、用户产品配置目录的 `skills/`、`~/.agents/skills`，最后是
Extension 路径。项目来源必须通过项目可信判断。配置和规则中的相对路径基于
Session cwd 解析，开头的 `~` 基于用户 home 解析。

Pi 模式来源允许根目录 Markdown 和嵌套 `SKILL.md`；`.agents/skills` 只接受嵌套
`SKILL.md`。发现过程遵守 ignore 文件，按 canonical path 去重，同名冲突保留先发现
资源，并返回包含来源及 winner/loser 路径的结构化诊断。非法名称和超长描述只警告，
不会隐藏其他方面可加载的 Skill；描述缺失或 frontmatter 非法时跳过该 Skill。

`disable-model-invocation: true` 只会阻止 Skill 进入模型可见的 System Prompt 清单，
不影响用户显式调用。WebUI Skill 页面展示来源、引用基准目录、仅手动调用状态和发现
诊断。Skill 正文只在实际调用时重新读取，列表接口不会返回正文。

## 运行

启动 ACP stdio Agent：

```sh
cargo run -p app --bin clawcode -- stdio
```

构建 WebUI 并启动同源服务：

```sh
cd web
npm install --ignore-scripts
npm run build
cd ..
cargo run -p app --bin clawcode -- serve --web-root web/dist
```

浏览器访问 `http://127.0.0.1:3000`。`GET /acp` 可升级为 WebSocket，也用于
SSE；`POST /acp` 承载 ACP JSON-RPC；健康检查位于 `GET /health`。

WebUI 没有登录能力，因此服务强制只监听 loopback，通配、局域网和公网地址会被
拒绝。模型、Skills 与 MCP 配置在界面中只读，修改 TOML 后需要重启。界面支持
文本、Markdown 和资源链接，不上传文件，也不实现 ACP 客户端文件系统或 terminal
callback。缺少 `web/dist` 时，UI 路由返回 HTTP 503，但 ACP 与 health 仍可用。

前端开发时先在 3000 端口启动 Rust 服务，再从 `web/` 运行 `npm run dev`；Vite
会代理 bootstrap 和 ACP WebSocket。
浏览器验收只连接正式 `app` 后端及配置的 provider，因此必须提供有效模型配置和凭据。

## 存储

Session 使用 pi v4 兼容 JSONL，按 cwd 编码目录保存。项目不创建全局
`index.jsonl`，列表通过扫描 v4 header 获得。存储支持 lane、树导航、branch
读取、fork、全局 name/label fact、operation record 和 torn-tail 恢复。Compact
保存 `summary`、`retainedTail`、`tokensBefore` 及完整关联时间。

Agent 级瞬时失败使用 2/4/8 秒指数退避；provider 请求级 retry 独立配置。上下文超过
`context_tokens - reserve_tokens` 时在成功响应后压缩但不重放；显式 overflow 或可恢复的
length stop 会移除失败 Assistant 的活动上下文，压缩后最多恢复一次。旧版按 Turn 数保留
的 compaction 字段不兼容。

## ACP 扩展

ACP 原生能力继续使用标准 `session/new`、`session/list`、`session/resume`、
`session/prompt`、`session/cancel` 与 `session/close`。pi 中没有 ACP 原生对应项的
Tree、Navigate、Branch、Fork、Compact、队列、Skill 和 MCP 状态使用集中定义的
ACP v2 扩展方法与 `SessionUpdate::Other`。Extension Command、Skill 和 Prompt
Template 使用 ACP v2 原生 `available_commands_update`。Agent 不声明或调用客户端
文件系统和 terminal callback。

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

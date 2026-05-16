# clawcode

基于 Agent Client Protocol (ACP) 的 AI 编程代理。负责编排 LLM 调用、管理会话、发现项目级技能并执行内置工具及 MCP 工具。

clawcode 包含两个二进制文件：

- **`acp`** — ACP stdio 代理，可接入任意 ACP 兼容客户端。
- **`claw`** — 交互式终端客户端，支持会话恢复和流式响应。

## 架构

```
┌──────────┐  ACP (stdio)  ┌───────┐
│  ACP     │◄────────────►│  acp  │
│  Client  │               └───┬───┘
└──────────┘                   │
                    ┌──────────▼──────────┐
                    │       kernel        │
                    │  会话 / 轮次调度     │
                    └──┬───────┬───────┬──┘
                       │       │       │
              ┌────────▼┐ ┌────▼──┐ ┌──▼──────┐
              │ provider │ │ tools │ │ skills  │
              │ (LLM)    │ │       │ │         │
              └──────────┘ └───┬───┘ └─────────┘
                          ┌────▼─────┐
                          │   mcp    │
                          │ servers  │
                          └──────────┘
```

## Crate 说明

| Crate | 用途 |
|---|---|
| `protocol` | 内部事件/操作类型，用于 agent-core 与前端通信 |
| `acp` | ACP 桥接层 — 将内部协议转换为 Agent Client Protocol，通过 stdio 传输 |
| `kernel` | 代理核心 — 会话生命周期、轮次循环、LLM 编排、工具调度 |
| `config` | 类型化配置，从 `claw.toml` 加载，基于 Figment，通过 `ArcSwap` 共享 |
| `provider` | LLM 提供商抽象 — 工厂、补全、流式客户端 |
| `tools` | 内置工具：shell 执行、文件 I/O（读/写/编辑/补丁）、技能调用、子代理派发、MCP 工具透传 |
| `skills` | 技能发现（`.agents/skills/` 与 `$HOME/.agents/skills/`）、目录渲染、`$skill-name` 提及匹配 |
| `mcp` | MCP 客户端 — 服务端连接管理、工具发现、通过 stdio 或 streamable HTTP 调用 |
| `store` | 会话持久化 — 基于文件的存储，含清单、录制与回放 |
| `tui` | 交互式终端 UI — 在进程内启动本地 ACP 代理，并渲染流式会话更新 |

## 快速开始

### 环境要求

- Rust stable（参见 `rust-toolchain.toml`）
- LLM API Key（OpenAI、DeepSeek 或任意 OpenAI 兼容提供商）

### 配置

在项目根目录创建 `claw.toml`（或复制项目自带示例）：

```toml
active_model = "deepseek/deepseek-v4-pro"
approval = "yolo"  # 或 "on_request"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
provider_type = "openai-completions"
base_url = "https://api.deepseek.com"

[providers.api_key]
env = "DEEPSEEK_API_KEY"

[[providers.models]]
id = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
context_tokens = 1000000
max_output_tokens = 384000

# 可选：接入 MCP 服务
[[mcp_servers]]
enabled = false
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
```

运行 TUI：

```sh
# 交互式终端 UI
cargo run -p tui

# 构建并启动 ACP 代理
cargo run --bin acp
```

TUI 还支持列出已持久化的会话并恢复其中一个会话：

```sh
cargo run -p tui -- --list-sessions
cargo run -p tui -- --resume <SESSION_ID>
```

## 技能（Skills）

技能存放在 `.agents/skills/<name>/SKILL.md`（项目级）或
`$HOME/.agents/skills/<name>/SKILL.md`（用户级）。每个技能包含 YAML frontmatter
（定义 `name` 和 `description`）及 Markdown 正文。当用户输入 `$skill-name` 时，
对应技能的正文会被注入到 system prompt 中。

同名技能以项目级优先。

## 本地工作区状态

本地代理状态目录会被 Git 忽略：

- `.agents/`
- `.claude/`
- `.codex/`

这些目录可能包含本地技能、工具缓存、会话记录或代理运行时文件。需要长期保留的项目文档应放在 `docs/`。

## 许可证

本项目以 [MIT](./LICENSE-MIT) 或 [Apache 2.0](./LICENSE-APACHE) 双协议授权，任选其一。

参见 [LICENSES](./LICENSES/) 与 [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md)。

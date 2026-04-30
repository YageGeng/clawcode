# ClawCode

ClawCode 是一个基于 Rust 的 AI 编程助手，实现了 [Agent Client Protocol (ACP)](https://agentclientprotocol.com/)，支持多 LLM 后端的交互式开发工作流。

## 功能

- **ACP 协议** — 完整实现 `session/load`、`session/prompt`、`initialize` 等标准方法。
- **会话持久化** — JSONL 格式存储，支持 `-r` 恢复历史会话，含工具调用上下文。
- **本地 CLI** — 交互式终端客户端，流式输出，工具审批，历史回放。
- **内置工具** — 文件读写、shell 执行、apply-patch，可通过 `ToolRouter` 扩展。
- **多模型** — 支持 DeepSeek、OpenAI、ChatGPT。

## 快速开始

```bash
cargo run -p cli                          # 新建会话
cargo run -p cli -- -r <session-id>       # 恢复会话
cargo run -p cli -- -l                    # 列出历史会话
cargo run -p cli -- --serve               # ACP stdio agent 模式
cargo run -p cli -- --log /tmp/claw.log   # 指定日志路径

cargo test                                # 全部测试
```

## 目录结构

```text
crates/
  cli/      # CLI 入口，参数解析 (clap)
  kernel/   # 运行时、轮次编排、工具调度
  tools/    # 工具路由与内置工具
  llm/      # 模型提供方适配与协议转换
  acp/      # ACP agent、client、会话管理、消息类型
  store/    # JSONL 会话持久化 (读写)
  skills/   # 技能注入管线
```

## 会话持久化

会话以 JSONL 文件存储在：
- **Linux / macOS** — `~/.local/share/clawcode/sessions/YYYY/MM/DD/`
- **Windows** — `%APPDATA%/clawcode/sessions/YYYY/MM/DD/`

每个轮次记录用户输入、模型回复、工具调用与结果、token 统计。恢复会话时完整回放对话历史，包括工具交互。

## 开发说明

- 使用 Rust 2024 工具链（`rust-toolchain.toml`）
- LLM 凭据在 `base.toml` 中配置
- `pre-commit` 强制 `rustfmt` 和 `clippy --tests --examples -Dwarnings`
- 模块边界：协议在 `acp`，编排在 `kernel`，工具在 `tools`，模型在 `llm`，持久化在 `store`

# ClawCode

ClawCode 是一个基于 Rust 实现的 AI REPL，提供基于工具调用的本地交互能力。

## 功能概览

- 提供本地 CLI 入口 `cli`，支持与模型交互。
- `kernel` 负责对话轮次编排、工具调用执行、状态与事件流。
- `tools` 提供工具路由与内置工具能力（例如 shell/apply_patch）。
- `llm` 封装模型提供方协议转换与请求/响应处理（含 DeepSeek/OpenAI）。
- `skills` 注入技能处理链路。
- `acp` 管理相关协议类型。

## 快速开始

```bash
# 编译 CLI
cargo build -p cli

# 运行 CLI
cargo run -p cli

# 运行测试
cargo test
# 或单个 crate
cargo test -p kernel
```

## 目录结构

```text
Cargo.toml                  # 工作区配置
crates/
  cli/      # 命令行入口
  kernel/   # 运行时与编排
  tools/    # 工具注册/执行
  llm/      # 模型提供方适配
  skills/   # 技能注入
  acp/      # 协议类型
```

## 开发说明

- 使用 Rust 2024 工具链（见 `rust-toolchain.toml`）。
- 需要按运行时配置提供对应 LLM 凭据。
- 工具报错应返回可读的模型文本，同时保留内部细节于结构化字段。
- 保持模块边界：
  - `llm` 做协议转换
  - `kernel` 做编排与状态管理
  - `tools` 做具体工具实现

## 质量要求

- 代码格式/静态检查通过 `pre-commit`（`fmt`, `clippy --tests --examples -Dwarnings`）。
- 修改新行为优先补充对应测试。

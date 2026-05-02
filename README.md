# ClawCode

ClawCode is a Rust AI coding assistant that implements the [Agent Client Protocol (ACP)](https://agentclientprotocol.com/) for interactive agent workflows with LLM providers.

## Features

- **ACP-compliant** — `session/load`, `session/prompt`, `initialize` per the ACP specification.
- **Session persistence** — JSONL-based session storage with resume support (`-r`).
- **Local CLI** — interactive terminal client with streaming output, tool approval, and history replay.
- **Built-in tools** — file read/write, shell execution, apply-patch, extensible via `ToolRouter`.
- **Multi-provider** — supports DeepSeek, OpenAI, and ChatGPT backends.

## Quick start

```bash
cargo run -p cli                          # new session
cargo run -p cli -- -r <session-id>       # resume a session
cargo run -p cli -- -l                    # list persisted sessions
cargo run -p cli -- --serve               # ACP stdio agent
cargo run -p cli -- --log /tmp/claw.log   # custom log path

cargo test                                # all tests
```

## Project layout

```text
crates/
  cli/      # CLI entrypoint, argument parsing (clap)
  kernel/   # agent runtime, turn orchestration, tool dispatch
  tools/    # tool router and built-in tools
  llm/      # LLM provider adapters and protocol conversion
  acp/      # ACP agent, client, session management, message types
  store/    # JSONL session persistence (read/write)
  skills/   # skill injection pipeline
```

## Session persistence

Sessions are stored as JSONL files under:
- **Linux / macOS** — `~/.local/share/clawcode/sessions/YYYY/MM/DD/`
- **Windows** — `%APPDATA%/clawcode/sessions/YYYY/MM/DD/`

Each turn records user input, model responses, tool calls, tool results, and usage stats. Resumed sessions replay full conversation history including tool interactions.

## Requirements

- Rust 2024 (toolchain in `rust-toolchain.toml`)
- Configured LLM provider credentials in `base.toml`
- CLI runtime limits such as `max_subagent_depth` live under `[runtime]` in `base.toml`

## Code quality

- `pre-commit` enforces `rustfmt` and `clippy --tests --examples -Dwarnings`
- Crate boundaries: protocol in `acp`, orchestration in `kernel`, tools in `tools`, providers in `llm`, persistence in `store`

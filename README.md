# ClawCode

ClawCode is a Rust implementation of an AI REPL that executes tool calls through a kernel/runtime loop and routes requests to LLM providers.

## What it does

- Runs a local CLI entrypoint (`cli`) for interactive agent workflows.
- Orchestrates turns in `kernel` (tool execution, continuation, turn/event flow).
- Executes built-in tools in `tools` and exposes shell/apply-patch style capabilities.
- Sends prompts/completions via `llm` providers (including DeepSeek/OpenAI adapters).
- Supports skill injections via the `skills` crate.

## Quick start

```bash
# build CLI
cargo build -p cli

# run CLI
cargo run -p cli

# run tests
cargo test
# or a single crate
cargo test -p kernel
```

## Project layout

```text
Cargo.toml                 # workspace manifest
crates/
  cli/      # command entrypoint
  kernel/   # agent runtime and orchestration
  tools/    # tool router + built-ins
  llm/      # completion providers and protocol conversion
  skills/   # skill pipeline hooks
  acp/      # ACP protocol types
```

## Requirements

- Rust 2024 (toolchain in `rust-toolchain.toml`).
- A configured LLM provider environment (depending on runtime config).

## Notes

- Tool errors should remain model-consumable while preserving internal context in structured fields.
- Keep changes aligned to crate boundaries: model protocol conversions in `llm`, orchestration in `kernel`, tool behavior in `tools`.
- Formatting and linting are enforced by `pre-commit` (`fmt`, `clippy --tests --examples -Dwarnings`).

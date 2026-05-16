# clawcode

An AI coding agent that runs over the Agent Client Protocol (ACP). It orchestrates
LLM calls, manages chat sessions, discovers project-level skills, and executes a
registry of built-in and MCP-backed tools.

clawcode ships two binaries:

- **`acp`** — ACP stdio agent for any ACP-compatible client.
- **`claw`** — Interactive terminal client with session resume and streaming
  responses.

## Architecture

```
┌──────────┐  ACP (stdio)  ┌───────┐
│  ACP     │◄────────────►│  acp  │
│  Client  │               └───┬───┘
└──────────┘                   │
                    ┌──────────▼──────────┐
                    │       kernel        │
                    │  session / turn     │
                    │  orchestration      │
                    └──┬───────┬───────┬──┘
                       │       │       │
              ┌────────▼┐ ┌────▼──┐ ┌──▼──────┐
              │ provider │ │ tools │ │ skills  │
              │ (LLMs)   │ │       │ │         │
              └──────────┘ └───┬───┘ └─────────┘
                          ┌────▼─────┐
                          │   mcp    │
                          │ servers  │
                          └──────────┘
```

## Crate map

| Crate | Purpose |
|---|---|
| `protocol` | Internal event/op types for agent-core ↔ frontend communication |
| `acp` | ACP bridge — translates internal protocol to Agent Client Protocol over stdio |
| `kernel` | Agent core — session lifecycle, turn loop, LLM orchestration, tool dispatch |
| `config` | Typed configuration loaded from `claw.toml` via Figment, shared behind `ArcSwap` |
| `provider` | LLM provider abstraction — factory, completion, streaming clients |
| `tools` | Built-in tools: shell execution, file I/O (read / write / edit / patch), skill invocation, sub-agent spawning, MCP tool passthrough |
| `skills` | Skill discovery from `.agents/skills/` and `$HOME/.agents/skills/`, catalog rendering, `$skill-name` mention matching |
| `mcp` | MCP client — server connection management, tool discovery, calls over stdio or streamable HTTP |
| `store` | Session persistence — file-based store with manifest, recording, and replay |
| `tui` | Interactive terminal UI — starts the local ACP agent in-process and renders streamed session updates |

## Quick start

### Prerequisites

- Rust stable (see `rust-toolchain.toml`)
- An LLM API key (OpenAI, DeepSeek, or any OpenAI-compatible provider)

### Configuration

Create a `claw.toml` in the project root (or copy the bundled example):

```toml
active_model = "deepseek/deepseek-v4-pro"
approval = "yolo"  # or "on_request"

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

# Optional: connect MCP servers
[[mcp_servers]]
enabled = false
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
```

Run the TUI with:

```sh
# Interactive terminal UI
cargo run -p tui

# Or build and launch the ACP agent 
cargo run --bin acp
```

The TUI also supports listing persisted sessions and resuming one:

```sh
cargo run -p tui -- --list-sessions
cargo run -p tui -- --resume <SESSION_ID>
```

## Skills

Skills live in `.agents/skills/<name>/SKILL.md` (project) or
`$HOME/.agents/skills/<name>/SKILL.md` (user). Each skill has YAML frontmatter
with a `name` and `description`, and a markdown body that is injected into the
system prompt when the user mentions `$skill-name`.

Project skills take priority over user skills with the same name.

## Local workspace state

Local agent state directories are intentionally ignored by Git:

- `.agents/`
- `.claude/`
- `.codex/`

These directories may contain local skills, tool caches, transcripts, or
agent-specific runtime files. Keep durable project documentation in `docs/`
instead.

## License

Licensed under either of [MIT](./LICENSE-MIT) or [Apache 2.0](./LICENSE-APACHE), at your option.

See [LICENSES](./LICENSES/) and [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md).

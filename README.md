# clawcode

A modular agent backend inspired by pi's Turn lifecycle and session model. The
runtime speaks Agent Client Protocol (ACP) v2 over stdio, HTTP/SSE, and
WebSocket and includes a local, light-themed Agent WebUI.

## Architecture

| Crate | Responsibility |
|---|---|
| `app` | Production composition root and CLI |
| `protocol` | Shared domain types, Turn/message events, and product identity |
| `config` | Immutable TOML configuration |
| `provider` | LLM API adapters and provider factory |
| `prompt` | Immutable pi-compatible System Prompt, instruction, and Template resources |
| `kernel` | Session ownership, serialized per-session runs, Turn loop, and tool orchestration |
| `tools` | Public `AgentTool` interface, registry, and pi-compatible coding tools |
| `mcp` | Session-scoped MCP stdio and Streamable HTTP tools |
| `skill` | Deterministic pi-compatible `SKILL.md` discovery and explicit invocation |
| `extension` | Non-UI pi lifecycle hooks and plugin registrations |
| `store` | Append-only pi v4 JSONL sessions, lanes, records, facts, branches, and forks |
| `acp` | ACP v2 mapping plus stdio, HTTP/SSE, and WebSocket transports |
| `web` | React/TypeScript Agent workbench using the ACP WebSocket transport |

The retained `config` and `provider` crates feed the new kernel through
factories. Every runtime message and event carries a `turn_id` and a decimal
string millisecond timestamp. Persisted messages also include creation,
first-output, and last-output timestamps.

## Configuration

Configuration is loaded once at startup. Search order is:

1. The explicit config-path environment variable.
2. The product directory under the platform config directory.
3. The repository-local TOML file.

Example:

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
name = "example"
command = "example-mcp-server"
args = []
```

## Prompt resources

Each new Session freezes a pi-compatible Prompt snapshot; resource files are
not hot-reloaded. The project may provide `.pi/SYSTEM.md` to replace the
built-in System Prompt and `.pi/APPEND_SYSTEM.md` to append guidance. If a
project file is absent, the same names under the product's user configuration
directory are used.

Instructions are loaded first from the user configuration directory and then
from project ancestors, from filesystem root to the Session cwd. Each directory
uses the first readable candidate in this order: `AGENTS.override.md`,
`AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, `CLAUDE.MD`.

Prompt Templates are discovered from the user `prompts/` directory and the
project `.pi/prompts/` directory. A Markdown Template can declare
`description` and `argument-hint` in YAML frontmatter. `$1` through `$9`, `$@`,
`${@:N}`, and `${@:N:L}` expand submitted arguments; missing positional
arguments expand to empty strings. For example, `/review README.md` expands the
`review.md` Template on the server. Skills are exposed as `/skill:name` and are
also expanded only by the server. The WebUI command palette displays the ACP
v2 Available Commands snapshot but never reads or expands Prompt resources.

The unified Slash Command catalog has four ordered sources: Kernel Builtins,
Extension commands, Skills, and Prompt Templates. Builtins are `/compact
[custom instructions]`, `/name [name]`, and `/session`. Extension canonical
names use `/extension-id:command`; an unambiguous short alias is advertised
separately. Commands are submitted through standard ACP `session/prompt`.
Direct command input and output, and frozen Skill or Template expansions,
retain their Turn and millisecond timing for replay.

## Skill resources

Each Session also freezes an immutable Skill catalog. Winner-first discovery
order is: configured `skills.paths`, project `.pi/skills`, `.agents/skills`
from the Session cwd upward to the Git root, the user configuration `skills/`
directory, `~/.agents/skills`, and Extension paths. Project sources require
project trust. Relative configured paths and rule paths resolve from the
Session cwd; a leading `~` resolves from the user home.

Pi-style sources accept root Markdown files and nested `SKILL.md` files.
`.agents/skills` accepts only nested `SKILL.md`. Discovery follows supported
ignore files, de-duplicates canonical paths, keeps the first name collision,
and returns structured diagnostics containing source and winner/loser paths.
Invalid names and overlong descriptions warn without hiding an otherwise
loadable Skill; missing descriptions and invalid frontmatter skip that Skill.

`disable-model-invocation: true` keeps a Skill out of the model-visible System
Prompt catalog while preserving explicit invocation. The WebUI Skill page
shows provenance, reference directories, manual-only state, and discovery
diagnostics. Skill bodies are read again when invoked and are never returned by
the list method.

## Running

Run an ACP stdio agent:

```sh
cargo run -p app --bin clawcode -- stdio
```

Build the WebUI with Node.js 22.12 or newer, then run the same-origin HTTP
server:

```sh
cd web
npm install --ignore-scripts
npm run build
cd ..
cargo run -p app --bin clawcode -- serve --web-root web/dist
```

Open `http://127.0.0.1:3000`. `GET /acp` upgrades to WebSocket when requested
and is otherwise used for SSE; `POST /acp` carries ACP JSON-RPC requests. The
service also exposes `GET /health`.

The unauthenticated WebUI is deliberately loopback-only. The server rejects
wildcard, LAN, and public bind addresses. The active model, Skills, and MCP
configuration are read-only in the browser and are loaded from TOML at startup.
The UI supports text, Markdown, and resource links; it does not upload files or
implement ACP client filesystem or terminal callbacks. If `web/dist` is
missing, UI routes return HTTP 503 while ACP and health routes stay available.

For frontend development, start the Rust server on port 3000, then run
`npm run dev` from `web/`; Vite proxies bootstrap and ACP WebSocket traffic.
Browser acceptance runs against the production `app` backend and the configured
provider. It therefore requires a valid model configuration and credentials.

## Storage

Sessions are stored as pi v4-compatible JSONL files under cwd-encoded
directories. There is no global `index.jsonl`; listing scans v4 headers.
Storage supports lanes, tree navigation, branch reads, branch forks, global
name/label facts, operation records, and torn-tail recovery.

Agent-level transient failures use 2/4/8-second exponential backoff, while
provider request retry is configured independently. Threshold compaction does
not replay a successful Assistant. Explicit overflow and recoverable length
stops remove the failed Assistant from active context and compact-and-retry at
most once. The former Turn-count compaction fields are not backward compatible.

## ACP extensions

Pi concepts without native ACP equivalents use ACP v2 extension methods and
`SessionUpdate::Other` values under the product namespace. Native
`session/new`, `session/list`, `session/resume`, `session/prompt`,
`session/cancel`, and `session/close` remain standard ACP methods. The agent
uses native ACP v2 `available_commands_update` for Builtins, Extension
Commands, Skills, and Prompt Templates. Product metadata records each source,
qualified alias, and command-message status. It does not advertise or invoke
client filesystem or terminal callbacks.

## Validation

```sh
cd web && npm run check && npm run build && cd ..
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

Licensed under either [MIT](./LICENSE-MIT) or
[Apache 2.0](./LICENSE-APACHE), at your option.

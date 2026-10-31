# clawcode

A modular agent backend inspired by pi's Turn lifecycle and session model. The
runtime speaks Agent Client Protocol (ACP) v2 over stdio, HTTP/SSE, and
WebSocket and includes a local, light-themed Agent WebUI.

## Architecture

| Crate | Responsibility |
|---|---|
| `app` | Production composition root, CLI, bootstrap, and WebUI static serving |
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

## Environment requirements

- Rust stable (see `rust-toolchain.toml`)
- Node.js 22.12 or newer (only required to build the WebUI)
- An API key for the configured model

## Configuration

Configuration is loaded once at startup. Search order is:

1. The explicit config-path environment variable.
2. The product directory under the platform config directory.
3. The repository-local TOML file.

Example:

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

[providers.models.extra_param]
thinking = { type = "enabled" }
reasoning_effort = "high"

[app.recovery]
# Caps source Session updates consumed into one outbound ACP frame.
max_batch_size = 512

[kernel]
max_turns = { type = "unlimited" }
# max_turns = { type = "limited", turns = 64 }

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

Omitting `[kernel]` defaults `max_turns` to `unlimited`. Use the `limited`
variant with a positive `turns` value to stop accidental endless Tool loops.

Model `id` is the stable local identifier used by `active_model`, Session
metadata, and model selection. When an upstream API expects a different
`model` value, set the optional `upstream_id`; when omitted, requests use
`id`.

## MCP discovery and output

MCP supports the same four revisions as Pi: `2025-11-25`, `2025-06-18`,
`2025-03-26`, and `2024-11-05`. Omitted `protocol` or `"auto"` requests
`2025-11-25`; an explicit supported version is a preference. Either policy accepts
any supported server selection in one initialize handshake. The UI shows the
actual negotiated version. HTTP session expiry does not silently reinitialize.

Migrate `2026-07-28` configurations to `auto`. Remove `mrtr_max_rounds` and rename
`mrtr_total_timeout_sec` to `request_total_timeout_sec` (default 300 seconds).
The removed keys are rejected explicitly. Normal request progress resets the
idle timeout but never extends the total deadline.

Tools default to `exposure = "direct"`. Use `"deferred"` for local `tool_search`
discovery or `"hidden"` to exclude tools from model access. Exact remote names
can override the server default:

```toml
[[mcp_servers]]
name = "github"
url = "https://example.com/mcp"
exposure = "deferred"

[mcp_servers.tool_exposure]
get_me = "direct"
delete_repository = "hidden"
```

Search loads tools for the **next** model request. It never executes them, and a
call in the same batch remains subject to the original declaration set. Each
search can add at most 32 KiB of complete tool definitions; this is not a lifetime
cap and there is no automatic eviction. Explicit load/disable preferences survive
reconnect, resume, branch navigation, fork, and compaction. Server annotations are
displayed as hints and do not grant approval.

`list_mcp_resources`, `list_mcp_resource_templates`, and `read_mcp_resource` expose
MCP resources to the model. Lists page through the already discovered snapshot
(default 50, maximum 200 records and 20 KiB per page), not remote lazy pagination.
Cursors are bound to the Session, catalog revision, kind and server filter. Reads
send the original URI to the chosen server, including unlisted template URIs.
A Hidden server does not expose resources even if individual tools are Direct.

After result hooks, MCP/search/read outputs share a 20 KiB text budget, including
JSON projection and file references. Media is kept whole, up to four blocks and
8 MiB encoded data. Larger results retain a private temporary JSON file when its
complete serialized size fits 64 MiB. Files follow temporary-file retention and
may later disappear; no download service is created. Save failure is shown
separately and never retries the remote operation. These limits do not bound
transport buffering or guarantee model token/media cost. Progress text is limited
to 1 KiB and never spooled.

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
The UI supports text, Markdown, resource links, and raster image attachments.
Selected images are encoded into ACP prompt blocks in the browser; the UI does
not implement ACP client filesystem or terminal callbacks. If `web/dist` is
missing, UI routes return HTTP 503 while ACP and health routes stay available.

For frontend development, start the Rust server on port 3000, then run
`npm run dev` from `web/`; Vite proxies bootstrap and ACP WebSocket traffic.
Browser acceptance runs against the production `app` backend and the configured
provider. It therefore requires a valid model configuration and credentials.

## Logging

Rig model spans write one local `Rig telemetry closed` summary to stderr when
the span closes. Summaries inherit the request Trace ID and include elapsed
milliseconds, provider/request/response model identities, available request IDs,
streaming mode, and reported input/output/cache/reasoning token counters. Missing
counters are omitted; a reported zero remains zero. Closure alone does not mean
success: use the correlated provider completion/error/cancellation logs for outcome.

The existing `[logging] filter` and overriding `RUST_LOG` control these summaries.
For example, `info,rig::completions=off,rig::modalities=off` disables native Rig
summaries; `warn,rig::completions=info` enables completion summaries while retaining
warnings elsewhere. Keep the application Trace spans enabled when Trace correlation
is needed. No OpenTelemetry collector, external exporter, or extra runtime dependency
is installed. Prompt/response bodies, system instructions, authentication headers
and native/encrypted payloads are excluded even if an SDK caller enables content
telemetry. Other application spans do not gain lifecycle log lines.
This exclusion applies to Rig summaries; existing ACP DEBUG parameter logging
continues to follow its separate recursive credential-redaction policy.

## Storage

Sessions are stored as pi v4-compatible JSONL files under cwd-encoded
directories. There is no global `index.jsonl`; listing scans v4 headers.
Storage supports lanes, tree navigation, branch reads, branch forks, global
name/label facts, operation records, and torn-tail recovery.

Transient failures share one Kernel retry budget. `[retry.provider] max_retries`,
when set, overrides `[retry] max_retries`; `[retry] enabled = false` disables
retries. Server `Retry-After` is honored in full, or rejected when it exceeds
the provider delay cap. `timeout_ms` is the upstream stream idle timeout
(including the first frame); zero disables it and omission uses the upstream default.
A Compaction checkpoint persists its `summary`, `tokensBefore`,
`readFiles`, and `modifiedFiles` alongside full timing. Threshold compaction
does not replay a successful Assistant. Explicit overflow and recoverable length
stops remove the failed Assistant from active context and compact-and-retry at
most once. The former Turn-count compaction fields are not backward compatible.

Provider adapters use the crates.io releases of `rig-core` and `rig-reqwest`
with the range `>=0.46.0-rc.1, <1`. This admits later stable 0.x releases and
newer prereleases of 0.46.0, while retaining the required API baseline.
`Cargo.lock` records the versions validated for this application; ordinary builds
retain them. Run `cargo update -p rig-core -p rig-reqwest` to obtain newer allowed
releases, then run the workspace tests and Clippy. Pre-1.0 minor releases may
require API adaptations; moving to 1.x is an explicit range change.
DeepSeek follows the upstream
text-only capability: configured image input is downgraded to the existing text
placeholder. Other providers use their configured protocol and upstream dialect.
Each session owns its model history cache; HTTP pools and authentication are shared.

Usage follows upstream optional counters. Input tokens already include cache
reads and writes; these are subsets, never additional input. Missing counts display
as unknown, while reported zero remains zero. New JSONL usage records use the
`rig-core-v1` envelope. Old records are interpreted conservatively on read without
rewriting their files; older application versions cannot necessarily read new records.
Native reasoning and provider identities are retained for replay, but are omitted
from browser payloads. Editing assistant content invalidates its native replay data.

Trusted compiled extensions register `HttpMiddleware` factories through
`ExtensionRegistrar::register_http_middleware`. They can inspect and modify full
headers, raw bodies and responses, including authentication headers. The former
provider request/header/response hook points and header whitelist are removed.
Factories receive the request's `ExtensionContext` and run on every attempt;
authentication traffic uses the base transport.

ChatGPT subscription authentication uses upstream `FileTokenStore` in a separate
absolute directory, for example:

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

Do not combine this auth configuration with `api_key`. The old `codex`/`auth_file`
configuration is rejected; existing Codex credentials are neither read nor moved.
The first request requires a fresh device sign-in. Its prompt goes to stderr to
preserve ACP stdio, and refresh/persistence are managed by upstream.

## ACP extensions

Pi concepts without native ACP equivalents use ACP v2 extension methods and
`SessionUpdate::Other` values under the product namespace. Native
`session/new`, `session/list`, `session/resume`, `session/prompt`,
`session/cancel`, and `session/close` remain standard ACP methods. The ACP
extensions cover session Tree navigation, Navigate, Branch, Fork, Compact,
queued follow-ups, Skill invoke/list, and MCP status. Extension Commands,
Skills, and Prompt Templates are advertised through the native ACP v2
`available_commands_update`. Product metadata records each source, qualified
alias, and command-message status. It does not advertise or invoke client
filesystem or terminal callbacks. A recovery extension reconnects an in-flight
Session when a WebSocket client reconnects, confirming the client's last
applied Kernel sequence against a replay cursor.

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

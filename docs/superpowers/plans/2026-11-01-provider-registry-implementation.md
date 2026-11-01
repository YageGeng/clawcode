# Provider Registry Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let every agent switch models freely and let users reach any rig-supported provider or compatible endpoint with minimal configuration, while keeping rig-core inside protocol, provider and extension.

**Architecture:** Configuration stays rig-free; the provider crate turns each provider entry into a rig `ProviderConfig` (registry preset or generic dialect) and fills missing model metadata from `Catalog::builtin()`. Each request builds a fresh `DynModel` through `ConfiguredModel::connect`, which also resolves credentials, so kernel holds no model cache and no credentials. The session thinking level reaches requests through `GenerationOptions`, subagents accept a `model` reference, and a pre-commit hook enforces the rig dependency boundary.

**Tech Stack:** Rust 2024 workspace, rig-core / rig-reqwest 0.46, tokio, serde/toml, typed-builder, pre-commit.

**Spec:** `docs/superpowers/specs/2026-11-01-provider-registry-design.md`

## Global Constraints

- rig-core and rig-reqwest stay at `version = "0.46"` in the root `[workspace.dependencies]`; add no new dependencies.
- Follow CLAUDE.md: English comments, a doc comment on every new function, typed-builder for structs with more than 3 fields (`Option` fields get `#[builder(default)]`), `Arc::clone(&x)`, fully qualified `tracing::*!` macros with format arguments, tests only in crate `tests/` or `#[cfg(test)] mod tests`.
- Never run `git commit` without the user's explicit permission. Each task ends at a checkpoint; when approved, run `pre-commit run --all-files`, read `.gitmessage`, and use the title given in the task.
- The repository `claw.toml` and every existing test configuration must load without edits.
- Thinking level `Off` sends no reasoning option; unsupported levels are not sent (`on_unsupported = Ignore`).
- Only `crates/protocol`, `crates/provider` and `crates/extension` may list rig crates outside `[dev-dependencies]`.

## Review Focus

1. A `provider/model` reference whose model id contains `/` (`openrouter/aion-labs/aion-2.0`) must split at the first `/` — Task 4 test `resolve_reference_splits_at_first_slash`.
2. An extension-contributed provider that names a rig vendor and omits `base_url` must build like a configured one — Task 3 test `extension_provider_uses_vendor_preset`.
3. A ChatGPT provider with `auth` and no `provider_type` must validate and use the Responses route — Task 3 test `chatgpt_without_provider_type_uses_responses`.
4. A compaction `reserve_tokens` at or above the catalog-derived context window of the active model must still stop startup with both numbers in the message — Task 3 test `compaction_reserve_checked_against_resolved_profile`.
5. An old provider with an unknown id and no `provider_type` must keep working on the Responses route — Task 3 test `vendor_resolution_examples` (case `private`).

---

### Task 1: Session thinking level reaches provider requests

**Files:**
- Modify: `crates/protocol/src/extension/events/model.rs` (`ThinkingLevel`)
- Modify: `crates/protocol/src/model.rs:16-21` (`ModelRequestOptions`)
- Modify: `crates/kernel/src/runtime/run.rs:329,548-552`
- Modify: `crates/kernel/src/runtime/compaction.rs:687`, `crates/kernel/src/runtime/mcp.rs:684`
- Modify: `crates/kernel/src/provider.rs` (`ProviderRequest::try_from`)
- Modify: `README.md` (thinking level paragraph)
- Test: `crates/protocol/tests/model_runtime.rs`, `crates/kernel/tests/provider_runtime.rs`, `crates/kernel/tests/turn.rs`

**Interfaces:**
- Produces: `ThinkingLevel: Default` (default `Off`); `ThinkingLevel::generation_options(self) -> Option<rig_core::completion::options::GenerationOptions>`; field `ModelRequestOptions::thinking_level: ThinkingLevel`.

- [ ] **Step 1: Write the failing tests**

In `crates/protocol/tests/model_runtime.rs`:

```rust
/// Off sends nothing; every other level maps to the same-named effort and tolerates unsupported levels.
#[test]
fn thinking_levels_map_to_generation_options() {
    use rig_core::completion::options::{Effort, OnUnsupported, Reasoning};
    assert!(ThinkingLevel::Off.generation_options().is_none());
    for (level, effort) in [
        (ThinkingLevel::Minimal, Effort::Minimal), (ThinkingLevel::Low, Effort::Low),
        (ThinkingLevel::Medium, Effort::Medium), (ThinkingLevel::High, Effort::High),
        (ThinkingLevel::XHigh, Effort::XHigh),
    ] {
        let options = level.generation_options().expect("options");
        assert_eq!(options.reasoning, Some(Reasoning::Effort(effort)));
        assert_eq!(options.unsupported_policy(), OnUnsupported::Ignore);
    }
}
```

In `crates/kernel/tests/provider_runtime.rs`, each test starts `Server::start(vec![(200, Server::completion())])`, builds `ProviderModelFactory::from_config(ConfigHandle::from_config(server.configuration("deepseek", "openai-completions")))`, streams `Server::request("hi")` with `options.thinking_level` set, drains to `Finished`, and reads `server.requests()[0]`:
- `deepseek_high_thinking_sends_reasoning_effort`: High → `body["reasoning_effort"] == "high"` and `body["thinking"]["type"] == "enabled"`.
- `off_and_unsupported_thinking_send_no_reasoning`: for Off and Medium → `body.get("reasoning_effort").is_none()`, `body.get("thinking").is_none()`, and the stream yields `Finished`.
- `extra_param_overrides_mapped_reasoning`: set `providers[0].models[0].extra_param = json!({"reasoning_effort": "low"})`, level High → `body["reasoning_effort"] == "low"`.

In `crates/kernel/tests/turn.rs`, `turn_requests_carry_session_thinking_level`: build the kernel around `SystemPromptCaptureModel` as the existing tests do, call `kernel.set_session_thinking_level(&session, ThinkingLevel::High)`, submit one prompt, assert `requests[0].options.thinking_level == ThinkingLevel::High`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p protocol --test model_runtime thinking_levels && cargo test -p kernel --test provider_runtime thinking && cargo test -p kernel --test turn thinking_level`
Expected: compile errors (`generation_options`, `thinking_level` field do not exist).

- [ ] **Step 3: Implement**

- `ThinkingLevel`: add `Default` to the derive list with `#[default]` on `Off`; add `generation_options` building `GenerationOptions::default().reasoning(effort).on_unsupported(OnUnsupported::Ignore)`.
- `ModelRequestOptions`: add `pub thinking_level: ThinkingLevel` with a doc comment; fix the two struct literals in `compaction.rs` and `mcp.rs` with `..Default::default()` (they stay Off).
- `run.rs:329`: replace `session.model.active()?` with `let (turn_model, thinking_level) = session.model.snapshot()?;` and set `options: ModelRequestOptions { thinking_level, ..Default::default() }` where the Turn request is built (`run.rs:548`).
- `ProviderRequest::try_from`: when `request.options.thinking_level.generation_options()` is `Some(options)`, call `CompletionRequest::options(options)` on the built request.
- README: state that the session thinking level is sent as a portable reasoning effort, Off sends nothing, levels a model does not support are skipped, and `extra_param` keys override by top-level key.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p protocol -p kernel`
Expected: all pass.

- [ ] **Step 5: Checkpoint** — title `feat(kernel): send session thinking level to providers`.

---

### Task 2: Per-request model construction with credentials inside provider

**Files:**
- Modify: `crates/provider/src/factory/mod.rs`, `crates/provider/src/auth.rs`, `crates/provider/src/lib.rs`
- Modify: `crates/provider/examples/factory_say_hi.rs`, `crates/provider/examples/factory_say_hi_stream.rs`
- Modify: `crates/kernel/src/provider.rs` (`ProviderModel`, `stream_request`; delete its `#[cfg(test)] mod tests`)
- Delete: `crates/kernel/tests/prepared_cache.rs`
- Test: `crates/provider/tests/upstream_factory.rs`, `crates/provider/tests/upstream_auth.rs`, `crates/kernel/tests/provider_history.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `ConfiguredModel` fields: `profile`, `upstream_id`, `config: rig_core::providers::registry::ProviderConfig`, `parameters`, `http`, `auth: Option<Authenticator>` (`#[builder(default)]`), `timeout_ms: Option<u64>` (`#[builder(default)]`).
  - `pub async fn connect(&self, http: DynHttpClient) -> Result<DynModel<Completion>, ProviderConnectError>`.
  - Kept: `profile()`, `http_client()`, `prepare()`, `ProviderFactory::{new, resolve}`.
  - Removed: `ProviderCompletion`, `Definition`, `build_model`, public `auth_context`, `ProviderConnectError::MissingAuthentication`.
  - `kernel::ProviderModel { configured: Arc<ConfiguredModel> }` with no cache.

- [ ] **Step 1: Rewrite the tests against `connect`**

- `upstream_factory.rs`: make `configured_protocols_build_unknown_models` a `#[tokio::test]`; replace `build_model(None).expect(..).with_transport(http)` with `configured.connect(http).await.expect("connect")` everywhere; keep every existing assertion.
- `upstream_auth.rs`: rewrite `provider_models_share_one_authenticator` to connect each model with `DynHttpClient::new(RecordingHttpClient::new("{}"))`, send one `call` (ignore its result), and assert the recorded request's `authorization` header is `Bearer first-synthetic` for model `b` after the store was replaced; drop the `MissingAuthentication` assertion from `authentication_diagnostics_redact_untrusted_details`.
- `provider_history.rs:406`: obtain the Messages wire from rig directly: `AnthropicConfig::with_key(&anthropic::wire::ANTHROPIC, "synthetic-key").connect(RecordingHttpClient::new("{}")).completion("audit").wire`.
- Delete `crates/kernel/tests/prepared_cache.rs` and the two cache tests in `crates/kernel/src/provider.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p provider -p kernel --no-run`
Expected: compile errors (`connect` not found).

- [ ] **Step 3: Implement**

- `ConfiguredModel::new` keeps today's `(ProviderId, ProviderType)` dialect match but wraps the result as `ProviderConfig::OpenAi(..)` (with the route applied) or `ProviderConfig::Anthropic(..)`.
- `connect`: if `auth` is set, resolve it with `self.http` (base transport, never the middleware client); clone `config`; on the OpenAI arm write `api_key`, `account_id` and `base_url` from the credential; per family call `.connect(http).completion(&self.upstream_id)`, apply the idle timeout exactly as `build_model` does today (`None` keeps the upstream default, `Some(0)` disables), then `.erase()`. Use an exhaustive `match` over the three `ProviderConfig` variants; the Gemini arm is reachable only after Task 3.
- `auth_context` becomes private to the provider crate; `ProviderConnectError::Display` keeps redacting `AuthError` details.
- `ProviderModel::stream_request`: `tokio::select!` between cancellation and `self.configured.connect(http)`; map `ProviderConnectError` through the existing `connection_failure("authentication", ..)`.
- Update both examples to `configured.connect(configured.http_client()).await?`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p provider -p kernel && cargo clippy --workspace --all-targets -- -Dwarnings`
Expected: all pass, no warnings.

- [ ] **Step 5: Checkpoint** — title `refactor(provider): build models per request and keep credentials in provider`.

---

### Task 3: Configuration through rig registry and catalog

**Files:**
- Modify: `crates/config/src/llm.rs`, `crates/config/src/config.rs`
- Modify: `crates/provider/src/factory/mod.rs`
- Modify: `crates/kernel/src/provider.rs` (`ProviderModelFactory::{create, create_catalog}`)
- Modify: `crates/app/src/lib.rs:120-170`
- Modify: `README.md` (provider configuration section)
- Test: `crates/config/tests/loading.rs`, `crates/provider/tests/upstream_factory.rs`, `crates/provider/tests/upstream_auth.rs`, `crates/kernel/tests/provider_runtime.rs`

**Interfaces:**
- Consumes: Task 2 `ConfiguredModel { config: ProviderConfig, .. }` and `connect`.
- Produces:
  - `LlmProvider` fields `vendor: Option<String>`, `provider_type: Option<ProviderType>`, `base_url: Option<String>` (all `#[serde(default, skip_serializing_if = "Option::is_none")]`); `LlmProvider::vendor(&self) -> &str` (`vendor` or `id.as_str()`).
  - `ProviderType::Gemini` (kebab-case `"gemini"`); `ProviderType` no longer derives `Default`.
  - `LlmModel::input: Option<Vec<ModelInputModality>>`.
  - Removed: `LlmModel::to_profile`, `AppConfig::model_profile`, `ConfigValidationError::{MissingModelLimit, CompactionReserveOverflow}`, the DeepSeek text-only special case.
  - provider: `ConfiguredModel::provider_config(provider: &LlmProvider, key: String) -> Result<ProviderConfig, String>` and `ConfiguredModel::profile_for(provider: &LlmProvider, model: &LlmModel, spec: Option<&ModelSpec>) -> Result<ModelProfile, String>` (associated, private). rig's `providers::registry::ProviderId` is imported as `RigProviderId` to avoid clashing with `config::ProviderId`.

- [ ] **Step 1: Write the failing tests**

`crates/config/tests/loading.rs`:
- `minimal_vendor_config_loads`: providers `[{ id = "deepseek", display_name = "DeepSeek", api_key = "k", models = [{ id = "deepseek-flash" }] }]` with `active_model = "deepseek/deepseek-flash"` → `toml::from_str::<AppConfig>` and `validate()` succeed.
- `chatgpt_auth_follows_vendor`: `id = "work", vendor = "chatgpt"` with `auth = { type = "chatgpt", token_store_dir = <tempdir> }` validates; `id = "chatgpt", vendor = "openai"` with the same auth fails with `InvalidAuthentication`.
- `repository_claw_toml_loads` (B09): inside `figment::Jail`, write the repository `claw.toml` (`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../claw.toml"))`), set `DEEPSEEK_API_KEY=synthetic` and any other env keys it names, and `load_from([PathBuf::from("claw.toml")])` succeeds.
- Delete the four `model_profile()` tests (lines 309-380); their assertions move to the provider tests below.

`crates/provider/tests/upstream_factory.rs` (expected catalog values come from `Catalog::builtin().get(RigProviderId::catalog("deepseek").unwrap(), "deepseek-flash").unwrap().spec`, never literals):
- `catalog_fills_missing_limits` (B01): the minimal config above → `profile.context_tokens == spec.context_window as u64`, `max_output_tokens` and `display_name` likewise.
- `configuration_overrides_catalog` (B02): same model with `context_tokens = 4096, max_output_tokens = 128, input = ["text"], display_name = "Mine"` → profile equals those values.
- `unlisted_model_without_limits_fails` (B03): `id = "corp", provider_type = "openai-completions", base_url = "https://provider.invalid/v1"`, model `local` without limits → `resolve` returns `ProviderBuildError::Invalid` whose reason contains `"context_tokens"` and `"rig catalog"`.
- `vendor_resolution_examples` (B04): connect each with a `RecordingHttpClient`, `call` once, assert the request URI suffix: `deepseek` → `/chat/completions`; `moonshot` + `anthropic` → `/messages`; `ds-proxy` with `vendor = "deepseek"`, `base_url = "https://proxy.invalid/v1"` → `https://proxy.invalid/v1/chat/completions`; `corp` + `openai-completions` + base_url → `/chat/completions`; `litellm` with `vendor = "anthropic"`, `openai-completions`, base_url → `/chat/completions`; `moonshot` and `minimax` without `provider_type` build; `private` with base_url and no `provider_type` → `/responses`; `private` without base_url → `Invalid` reason contains `"base_url"`.
- Update the DeepSeek image test: configured `input = ["text", "image"]` now yields a profile that supports `Image`.

`crates/provider/tests/upstream_auth.rs`:
- `chatgpt_without_provider_type_uses_responses`: chatgpt provider with seeded token store and no `provider_type` → request URI ends with `/responses` and carries `Bearer fresh-synthetic`.

`crates/kernel/tests/provider_runtime.rs` (start from `server.configuration(id, protocol)` and edit the returned `AppConfig` fields, since `Server::url` is private):
- `gemini_and_openrouter_reach_local_server` (B05): `server.configuration("gemini", "gemini")` with its explicit limits, and `server.configuration("openrouter", "openai-completions")` with the model id changed to `aion-labs/aion-2.0`, both limits set to `None` and `active_model` updated; stream once each; the openrouter body has `model == "aion-labs/aion-2.0"` and the server saw two requests.
- `extension_provider_uses_vendor_preset`: a static extension provider `{"display_name":"x","vendor":"deepseek","api_key":"k","models":[{"id":"deepseek-flash"}]}` → `create_catalog` succeeds.
- `compaction_reserve_checked_against_resolved_profile`: `server.configuration("deepseek", "openai-completions")` with the model id changed to `deepseek-flash`, limits `None`, `active_model = "deepseek/deepseek-flash"` and `compaction.reserve_tokens` set to the catalog `context_window` → `create_catalog` fails; message contains both numbers.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p config -p provider -p kernel --no-run`
Expected: compile errors (`vendor`, `ProviderType::Gemini`, optional `input` do not exist).

- [ ] **Step 3: Implement**

- config: apply the field changes; `validate_models` checks duplicate ids, non-empty `upstream_id`, non-zero limits when present, and `ModelInputModalities::try_from` when `input` is present; `validate_auth_source` checks `self.vendor() == "chatgpt"` and `provider_type` `None | Some(Responses)`; `AppConfig::validate` calls `resolve_active_model()` instead of `model_profile()` and drops the compaction comparison.
- `provider_config`, following spec §2.2 in order: format from `provider_type` (`Responses | OpenaiCompletions` → `Format::OpenAi`, `Anthropic` → `Format::Anthropic`, `Gemini` → `Format::Gemini`); `RigProviderId::new(vendor, format)` or `RigProviderId::catalog(vendor)`; `.config(key)`; otherwise require `base_url` and use `openai::wire::OPENAI`, `anthropic::wire::ANTHROPIC` or `GeminiConfig::new(key)` (format defaults to Responses); then `with_base_url` when present and `with_route` for an explicit OpenAI protocol. Error reasons name the provider id, vendor and protocol, never the key.
- `profile_for`: merge per spec §2.3 (config, then `spec.context_window` / `spec.max_output_tokens` / `spec.input.image` / `spec.display_name`, then model id; missing limit → reason `"<field> is not configured and the rig catalog has no value"`). Catalog lookup key is `upstream_id` or `id`, under `RigProviderId::catalog(vendor)`.
- `connect`: add the Gemini arm.
- kernel: `create` resolves ids from `config.resolve_active_model()`; `create_catalog` resolves each `(provider.id.as_str(), model.id)` directly and then fails with `ModelError::Unavailable` when `compaction.reserve_tokens >= active profile context_tokens`.
- app: build `ProviderModelFactory::from_config` before the UI bootstrap, take the profile from `ModelFactory::create(&factory)?.profile()`, and pass the same `Arc` to `KernelFactory`.
- README: document `vendor`, optional `provider_type` / `base_url` / limits / `input`, the §2.2 resolution rules with the five examples, `gemini` (official preset is `vendor = "gcp.gemini"`; otherwise `provider_type = "gemini"` plus `base_url`), and that omitted `provider_type` keeps working configs unchanged.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -Dwarnings`
Expected: all pass, including `repository_claw_toml_loads`.

- [ ] **Step 5: Checkpoint** — title `feat(provider): resolve providers through rig registry and catalog`.

---

### Task 4: Subagents accept a model reference

**Files:**
- Modify: `crates/kernel/src/runtime/model_state.rs`
- Modify: `crates/kernel/src/runtime/ownership.rs` (`create_owned_session`, `OwnedSessionSpawn`)
- Modify: `crates/kernel/src/runtime/subagent/foreground.rs`, `crates/kernel/src/runtime/subagent/background.rs`
- Modify: `README.md` (subagent tools)
- Test: `crates/kernel/src/runtime/model_state.rs` (`mod tests`), `crates/kernel/tests/subagent/foreground.rs`, `crates/kernel/tests/subagent/background.rs`

**Interfaces:**
- Produces:
  - `SessionModelState::resolve_reference(&self, reference: &str) -> Result<Arc<dyn Model>, String>`; splits at the first `/`; the error is `unknown model '<reference>'; configured models: <provider/model, ...>` in catalog order.
  - `Kernel::create_owned_session(&self, ownership: SessionOwnership, title: SessionTitle, model: Option<Arc<dyn Model>>) -> Result<SessionId, KernelError>`; `OwnedSessionSpawn` gains `#[builder(default)] model: Option<Arc<dyn Model>>`, which replaces `AgentSnapshot::model` before `apply`.

- [ ] **Step 1: Write the failing tests**

- `model_state.rs` `resolve_reference_splits_at_first_slash`: catalog containing a `TestModel` with provider `openrouter`, model `aion-labs/aion-2.0` → `resolve_reference("openrouter/aion-labs/aion-2.0")` is `Ok`; `"openrouter/missing"` and `"nomodel"` are `Err` containing `"openrouter/aion-labs/aion-2.0"`.
- `foreground.rs` `subagent_runs_on_requested_model`: the parent calls `subagent` with `{"task":"x","model":"fixture/secondary"}` → the tool result text contains `"secondary answer"`.
- `foreground.rs` `subagent_rejects_unconfigured_model`: `{"task":"x","model":"fixture/missing"}` → error text contains `"fixture/primary"` and `"fixture/secondary"`, and no owned session is created.
- `background.rs` `background_spawn_runs_on_requested_model`: `spawn` with `"model":"fixture/secondary"` → the reported answer contains `"secondary answer"`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p kernel resolve_reference && cargo test -p kernel --test subagent requested_model unconfigured_model`
Expected: compile error for `resolve_reference`, then assertion failures (the model argument is ignored).

- [ ] **Step 3: Implement**

- `subagent` schema gains `"model":{"type":"string","description":"Optional configured model as provider/model; defaults to your current model"}`; resolve it from the owner session before `create_owned_session`, and return `ToolError::InvalidArguments` with the `resolve_reference` message on error.
- `background_subagent`: `Spawn` carries `model: Option<String>`; parse `model` only for `spawn`; resolve before `create_owned_session` and return the message through the existing `Err(String)` path; add `model` to the schema and to the spawn wording in the description.
- Existing callers of `create_owned_session` pass `None`.
- README: document the optional `model` argument.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p kernel`
Expected: all pass, including the existing inheritance tests.

- [ ] **Step 5: Checkpoint** — title `feat(kernel): let subagents choose a configured model`.

---

### Task 5: rig dependency boundary and final verification

**Files:**
- Create: `crates/protocol/src/llm.rs`; Modify: `crates/protocol/src/lib.rs`
- Modify: `crates/extension/src/lib.rs`
- Modify: `crates/kernel/Cargo.toml`, every `rig_core::` use in `crates/kernel/src`
- Modify: `extensions/Cargo.toml`, `extensions/src/available/hook_examples/provider.rs`
- Create: `scripts/check-rig-boundary.sh` (executable); Modify: `.pre-commit-config.yaml`

**Interfaces:**
- Produces: `protocol::llm::{message, completion, streaming}` re-export modules (items listed in spec §9); `extension::http_client` and `extension::WasmBoxedFuture` re-exports.

- [ ] **Step 1: Write the boundary check**

```bash
#!/usr/bin/env bash
# Fails when a workspace crate outside the allowlist depends on or names rig directly.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
allowed=" crates/protocol crates/provider crates/extension "
status=0
for manifest in crates/*/Cargo.toml extensions/Cargo.toml; do
  dir="${manifest%/Cargo.toml}"
  [[ "$allowed" == *" $dir "* ]] && continue
  # Every dependency table except [dev-dependencies]; also catches [dependencies.rig-core].
  awk -v file="$manifest" '
    /^\[/ { normal = ($0 ~ /dependencies(\.[^]]*)?\]$/ && $0 !~ /dev-dependencies/) }
    normal && (/^rig-/ || /^\[.*dependencies\.rig-/) { printf "%s:%d: %s\n", file, NR, $0; bad = 1 }
    END { exit bad }' "$manifest" || status=1
  if grep -rn --include='*.rs' -E 'rig_(core|reqwest)::' "$dir/src"; then status=1; fi
done
exit "$status"
```

- [ ] **Step 2: Run it to verify it fails on the current tree**

Run: `scripts/check-rig-boundary.sh; echo "exit=$?"`
Expected: lines for `crates/kernel/Cargo.toml`, `extensions/Cargo.toml`, `crates/kernel/src/...` and `extensions/src/...`, then `exit=1`.

- [ ] **Step 3: Migrate**

- `protocol::llm`: three submodules re-exporting exactly the spec §9 items; no `pub use rig_core::*`.
- kernel: replace every `rig_core::` path in `src` (including `#[cfg(test)] mod tests`) with `protocol::llm::`; add re-exports only for items kernel actually uses; remove `rig-core` from `[dependencies]` and keep the `[dev-dependencies]` entry.
- extension: `pub use rig_core::http_client;` and `pub use rig_core::wasm_compat::WasmBoxedFuture;` with doc comments; `extensions` imports them from `extension` and drops `rig-core` from `[dependencies]`.
- `.pre-commit-config.yaml`: add a `repo: local` hook `id: rig-boundary`, `name: "rig-core dependency boundary"`, `entry: scripts/check-rig-boundary.sh`, `language: system`, `pass_filenames: false`, `files: '(Cargo\.toml|\.rs)$'`.

- [ ] **Step 4: Verify the boundary both ways (B10)**

Run: `scripts/check-rig-boundary.sh; echo "exit=$?"`
Expected: no output, `exit=0`.

Run: `cp crates/kernel/Cargo.toml /tmp/kernel.toml.bak && sed -i '/^\[dependencies\]/a rig-core = { workspace = true }' crates/kernel/Cargo.toml && scripts/check-rig-boundary.sh; echo "exit=$?"; mv /tmp/kernel.toml.bak crates/kernel/Cargo.toml`
Expected: one `crates/kernel/Cargo.toml:<n>: rig-core = ...` line, `exit=1`, file restored.

Run: `echo 'fn _probe(_: rig_core::message::Message) {}' >> crates/kernel/src/lib.rs && scripts/check-rig-boundary.sh; echo "exit=$?"; sed -i '$d' crates/kernel/src/lib.rs`
Expected: one `crates/kernel/src/lib.rs:<n>:` line, `exit=1`, line removed.

- [ ] **Step 5: Final verification (B09, B11)**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test --workspace && pre-commit run --all-files && git diff --stat`
Expected: everything passes; `claw.toml` is not in the diff.

- [ ] **Step 6: Checkpoint** — title `build(deps): confine rig-core to protocol, provider and extension`.

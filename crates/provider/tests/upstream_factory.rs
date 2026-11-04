use config::{AppConfig, ConfigHandle};
use provider::ProviderFactory;

#[path = "support/capture.rs"]
mod capture;
use capture::SentRequest;

/// Every existing configured protocol stays constructible without catalog membership.
#[tokio::test]
async fn configured_protocols_build_unknown_models() {
    for (id, protocol) in [
        ("openai", "responses"),
        ("openai", "openai-completions"),
        ("chatgpt", "responses"),
        ("deepseek", "openai-completions"),
        ("moonshot", "openai-completions"),
        ("moonshot", "anthropic"),
        ("minimax", "openai-completions"),
        ("minimax", "anthropic"),
        ("xiaomimimo", "openai-completions"),
        ("xiaomimimo", "anthropic"),
        ("anthropic", "anthropic"),
        ("private", "responses"),
        ("private", "openai-completions"),
        ("private", "anthropic"),
        ("private", "gemini"),
        ("gemini", "gemini"),
    ] {
        let config: AppConfig = toml::from_str(&format!(
            r#"
active_model = "{id}/local"
[[providers]]
id = "{id}"
display_name = "audit"
provider_type = "{protocol}"
base_url = "https://provider.invalid/v1"
api_key = "synthetic-key"
[[providers.models]]
id = "local"
upstream_id = "unlisted-model"
context_tokens = 4096
max_output_tokens = 128
"#
        ))
        .expect("parse config");
        let factory = ProviderFactory::new(ConfigHandle::from_config(config));
        let configured = factory
            .resolve(id, "local")
            .expect("resolve configured model");
        let model = configured
            .connect(configured.http_client())
            .await
            .expect("construct upstream model");
        assert_eq!(model.id(), Some("unlisted-model"));
        // Neither configuration nor the rig catalog declares images for this model.
        assert!(
            !configured
                .profile()
                .input
                .supports(protocol::ModelInputModality::Image)
        );
        factory.resolve(id, "missing").unwrap_err();
    }
}

/// Request parameters overlay model defaults without discarding nested defaults.
#[test]
fn request_parameters_overlay_model_defaults() {
    let config: AppConfig = toml::from_str(
        r#"
active_model = "deepseek/local"
[[providers]]
id = "deepseek"
display_name = "audit"
provider_type = "openai-completions"
base_url = "https://provider.invalid/v1"
api_key = "synthetic-key"
[[providers.models]]
id = "local"
context_tokens = 4096
max_output_tokens = 128
extra_param = { a = { x = 1, y = 2 } }
"#,
    )
    .expect("parse config");
    let factory = ProviderFactory::new(ConfigHandle::from_config(config));
    let configured = factory.resolve("deepseek", "local").expect("model");
    let request = configured.prepare(
        rig_core::completion::CompletionRequest::new("hi")
            .additional_params(serde_json::json!({"a":{"x":3}})),
    );
    assert_eq!(
        request.additional_params,
        Some(serde_json::json!({"a":{"x":3,"y":2}}))
    );
}

/// The configured upstream id and custom endpoint survive real upstream request encoding.
#[tokio::test]
async fn configured_request_reaches_the_upstream_wire() {
    let config: AppConfig = toml::from_str(
        r#"
active_model = "deepseek/local"
[[providers]]
id = "deepseek"
display_name = "audit"
provider_type = "openai-completions"
base_url = "https://provider.invalid/custom"
api_key = "synthetic-key"
[[providers.models]]
id = "local"
upstream_id = "unlisted-model"
context_tokens = 4096
max_output_tokens = 128
input = ["text", "image"]
"#,
    )
    .expect("parse config");
    let factory = ProviderFactory::new(ConfigHandle::from_config(config));
    let configured = factory.resolve("deepseek", "local").expect("model");
    // Configured input wins over the catalog, including for DeepSeek.
    assert!(
        configured
            .profile()
            .input
            .supports(protocol::ModelInputModality::Image)
    );
    let capture = rig_core::test_utils::RecordingHttpClient::new(serde_json::json!({
        "id":"r","object":"chat.completion","created":0,"model":"unlisted-model",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}
    }).to_string());
    let model = configured
        .connect(rig_core::http_client::DynHttpClient::new(capture.clone()))
        .await
        .expect("model");
    let response = model
        .call(rig_core::completion::CompletionRequest::new("hi"))
        .await
        .expect("completion");
    assert_eq!(response.text(), "ok");
    let requests = capture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].uri,
        "https://provider.invalid/custom/chat/completions"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(body["model"], "unlisted-model");
}

/// Builds a provider factory from one TOML document.
fn factory_from(document: &str) -> ProviderFactory {
    ProviderFactory::new(ConfigHandle::from_config(
        toml::from_str::<AppConfig>(document).expect("parse config"),
    ))
}

/// Returns the rig catalog entry the provider crate is expected to read.
fn catalog_spec(
    preset: &str,
    model: &str,
) -> &'static rig_core::catalog::ModelSpec {
    rig_core::catalog::Catalog::builtin()
        .get(
            rig_core::providers::registry::ProviderId::catalog(preset)
                .expect("known preset"),
            model,
        )
        .expect("catalog entry")
        .spec
}

/// A provider naming only its preset takes model limits and names from the rig catalog.
#[test]
fn catalog_fills_missing_limits() {
    let factory = factory_from(
        r#"
active_model = "deepseek/deepseek-flash"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
api_key = "synthetic-key"
[[providers.models]]
id = "deepseek-flash"
"#,
    );
    let spec = catalog_spec("deepseek", "deepseek-flash");
    let configured = factory
        .resolve("deepseek", "deepseek-flash")
        .expect("catalog model");
    let profile = configured.profile();
    assert_eq!(
        Some(profile.context_tokens),
        spec.context_window.map(u64::from)
    );
    assert_eq!(
        Some(profile.max_output_tokens),
        spec.max_output_tokens.map(u64::from)
    );
    assert_eq!(profile.display_name, spec.display_name);
    assert_eq!(
        profile.input.supports(protocol::ModelInputModality::Image),
        spec.input.image
    );
}

/// Configured metadata takes priority over the rig catalog.
#[test]
fn configuration_overrides_catalog() {
    let factory = factory_from(
        r#"
active_model = "deepseek/deepseek-flash"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
api_key = "synthetic-key"
[[providers.models]]
id = "deepseek-flash"
display_name = "Mine"
context_tokens = 4096
max_output_tokens = 128
input = ["text"]
"#,
    );
    let configured = factory
        .resolve("deepseek", "deepseek-flash")
        .expect("configured model");
    let profile = configured.profile();
    assert_eq!(profile.context_tokens, 4096);
    assert_eq!(profile.max_output_tokens, 128);
    assert_eq!(profile.display_name, "Mine");
    assert!(!profile.input.supports(protocol::ModelInputModality::Image));
}

/// A model unknown to the catalog must declare its limits.
#[test]
fn unlisted_model_without_limits_fails() {
    let factory = factory_from(
        r#"
active_model = "corp/local"
[[providers]]
id = "corp"
display_name = "Corp"
provider_type = "openai-completions"
base_url = "https://provider.invalid/v1"
api_key = "synthetic-key"
[[providers.models]]
id = "local"
upstream_id = "big-model"
"#,
    );
    let reason = factory
        .resolve("corp", "local")
        .expect_err("limits are required")
        .to_string();
    assert!(reason.contains("context_tokens"), "{reason}");
    assert!(reason.contains("rig catalog"), "{reason}");
    // The reason names the catalog key that was looked up.
    assert!(reason.contains("corp/big-model"), "{reason}");
}

/// Preset and protocol pick the rig preset, a dialect pair, or a generic dialect.
#[tokio::test]
async fn preset_resolution_examples() {
    let document = |fields: &str| {
        format!(
            r#"
active_model = "p/m"
[[providers]]
{fields}
display_name = "audit"
api_key = "synthetic-key"
[[providers.models]]
id = "m"
context_tokens = 4096
max_output_tokens = 128
"#
        )
    };
    for (id, fields, expected) in [
        ("deepseek", r#"id = "deepseek""#, "/chat/completions"),
        (
            "moonshot",
            "id = \"moonshot\"\nprovider_type = \"anthropic\"",
            "/messages",
        ),
        (
            "ds-proxy",
            "id = \"ds-proxy\"\npreset = \"deepseek\"\nbase_url = \"https://proxy.invalid/v1\"",
            "https://proxy.invalid/v1/chat/completions",
        ),
        (
            "corp",
            "id = \"corp\"\nprovider_type = \"openai-completions\"\nbase_url = \"https://corp.invalid/v1\"",
            "https://corp.invalid/v1/chat/completions",
        ),
        (
            "litellm",
            "id = \"litellm\"\npreset = \"anthropic\"\nprovider_type = \"openai-completions\"\nbase_url = \"https://litellm.invalid/v1\"",
            "https://litellm.invalid/v1/chat/completions",
        ),
        ("moonshot", r#"id = "moonshot""#, "/chat/completions"),
        ("minimax", r#"id = "minimax""#, "/messages"),
        (
            "private",
            "id = \"private\"\nbase_url = \"https://private.invalid/v1\"",
            "https://private.invalid/v1/responses",
        ),
        // README upgrade note: a rig preset id now uses that preset's default route...
        (
            "openrouter",
            "id = \"openrouter\"\nbase_url = \"https://gateway.invalid/v1\"",
            "https://gateway.invalid/v1/chat/completions",
        ),
        // ...and an unknown preset name restores the previous generic Responses behavior.
        (
            "openrouter",
            "id = \"openrouter\"\npreset = \"custom\"\nbase_url = \"https://gateway.invalid/v1\"",
            "https://gateway.invalid/v1/responses",
        ),
    ] {
        let factory = factory_from(&document(fields));
        let configured = factory.resolve(id, "m").expect(fields);
        let uri = configured.sent_request().await.uri;
        assert!(uri.ends_with(expected), "{fields}: {uri}");
    }
    // The error names the protocol that has no preset, including the default.
    for (fields, protocol) in [
        (r#"id = "private""#, "responses (default)"),
        ("id = \"private\"\nprovider_type = \"gemini\"", "gemini"),
    ] {
        let reason = factory_from(&document(fields))
            .resolve("private", "m")
            .expect_err("unknown preset needs a base URL")
            .to_string();
        assert!(reason.contains("base_url"), "{reason}");
        assert!(reason.contains(protocol), "{reason}");
    }
}

/// The catalog's input cap, not its whole window, bounds the context a profile reports.
#[test]
fn catalog_input_limit_bounds_context() {
    let factory = factory_from(
        r#"
active_model = "openai/gpt-5"
[[providers]]
id = "openai"
display_name = "OpenAI"
api_key = "synthetic-key"
[[providers.models]]
id = "gpt-5"
"#,
    );
    let spec = catalog_spec("openai", "gpt-5");
    let input_limit = spec.input_limit().expect("catalog input limit");
    assert!(
        spec.context_window
            .is_some_and(|window| input_limit < window),
        "the fixture model must cap input below its window"
    );
    let configured = factory.resolve("openai", "gpt-5").expect("catalog model");
    assert_eq!(configured.profile().context_tokens, u64::from(input_limit));
}

/// Builds a one-model factory whose output limits come from `model_lines`.
fn budget_factory(model_lines: &str) -> ProviderFactory {
    factory_from(&format!(
        r#"
active_model = "corp/local"
[[providers]]
id = "corp"
display_name = "Corp"
provider_type = "openai-completions"
base_url = "https://provider.invalid/v1"
api_key = "synthetic-key"
[[providers.models]]
id = "local"
context_tokens = 100000
{model_lines}
"#
    ))
}

/// Without a configured budget, a request asks for the output capability bounded by the reserve.
#[test]
fn request_budget_defaults_to_reserve_bounded_output() {
    for (max_output, expected) in [(64_000_u64, 16_384_u64), (8_000, 8_000)] {
        let factory =
            budget_factory(&format!("max_output_tokens = {max_output}"));
        let configured = factory.resolve("corp", "local").expect("model");
        assert_eq!(configured.profile().request_max_tokens, expected);
        assert_eq!(
            configured
                .prepare(rig_core::completion::CompletionRequest::new("hi"))
                .max_tokens,
            Some(expected)
        );
    }
}

/// A configured budget is sent as written.
#[test]
fn configured_request_budget_is_sent() {
    let factory =
        budget_factory("max_output_tokens = 64000\nrequest_max_tokens = 4000");
    let configured = factory.resolve("corp", "local").expect("model");
    assert_eq!(configured.profile().request_max_tokens, 4000);
    assert_eq!(
        configured
            .prepare(rig_core::completion::CompletionRequest::new("hi"))
            .max_tokens,
        Some(4000)
    );
}

/// A budget above the model's output capability fails the model build.
#[test]
fn request_budget_above_output_capability_fails() {
    let reason =
        budget_factory("max_output_tokens = 4000\nrequest_max_tokens = 8000")
            .resolve("corp", "local")
            .expect_err("budget above capability")
            .to_string();
    assert!(reason.contains("request_max_tokens"), "{reason}");
    assert!(reason.contains("4000"), "{reason}");
}

/// Thinking levels snap to the effort tiers the catalog lists for the model.
#[test]
fn thinking_levels_snap_to_catalog_efforts() {
    use protocol::ThinkingLevel;
    use rig_core::catalog::ReasoningSupport;
    use rig_core::completion::options::{Effort, Reasoning};
    let document = |preset: &str, model: &str| {
        format!(
            r#"
active_model = "{preset}/{model}"
[[providers]]
id = "{preset}"
display_name = "audit"
api_key = "synthetic-key"
[[providers.models]]
id = "{model}"
"#
        )
    };
    // DeepSeek takes low, high and max; Anthropic's Opus also takes xhigh.
    for (preset, model, cases) in [
        (
            "deepseek",
            "deepseek-flash",
            [
                (ThinkingLevel::Minimal, Effort::Low),
                (ThinkingLevel::Low, Effort::Low),
                (ThinkingLevel::Medium, Effort::Low),
                (ThinkingLevel::High, Effort::High),
                (ThinkingLevel::XHigh, Effort::High),
            ],
        ),
        (
            "anthropic",
            "claude-opus-5-5",
            [
                (ThinkingLevel::Minimal, Effort::Low),
                (ThinkingLevel::Low, Effort::Low),
                (ThinkingLevel::Medium, Effort::Medium),
                (ThinkingLevel::High, Effort::High),
                (ThinkingLevel::XHigh, Effort::XHigh),
            ],
        ),
    ] {
        assert!(
            matches!(
                catalog_spec(preset, model).reasoning,
                ReasoningSupport::Listed { .. }
            ),
            "{preset}/{model} must list its effort tiers"
        );
        let factory = factory_from(&document(preset, model));
        let configured = factory.resolve(preset, model).expect("catalog model");
        for (level, expected) in cases {
            let request = configured.prepare(
                rig_core::completion::CompletionRequest::new("hi").options(
                    protocol::ModelRequestOptions::builder()
                        .thinking_level(level)
                        .build()
                        .into_generation_options()
                        .expect("level"),
                ),
            );
            assert_eq!(
                request.options.reasoning,
                Some(Reasoning::Effort(expected)),
                "{preset}/{model} {level:?}"
            );
        }
    }
}

/// The official Gemini preset needs neither a base URL nor limits for a catalog model.
#[tokio::test]
async fn gcp_gemini_preset_uses_catalog_and_official_endpoint() {
    let factory = factory_from(
        r#"
active_model = "gemini/gemini-2.0-flash"
[[providers]]
id = "gemini"
preset = "gcp.gemini"
display_name = "Gemini"
api_key = "synthetic-key"
[[providers.models]]
id = "gemini-2.0-flash"
"#,
    );
    let configured = factory
        .resolve("gemini", "gemini-2.0-flash")
        .expect("catalog model");
    let spec = catalog_spec("gcp.gemini", "gemini-2.0-flash");
    assert_eq!(
        Some(configured.profile().context_tokens),
        spec.input_limit().map(u64::from)
    );
    let uri = configured.sent_request().await.uri;
    assert!(uri.contains("generativelanguage.googleapis.com"), "{uri}");
    assert!(uri.contains("gemini-2.0-flash:generateContent"), "{uri}");
}

/// Every model in the repository configuration builds, with its documented budget.
#[test]
fn repository_claw_toml_builds_every_model() {
    let mut config: AppConfig = toml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../claw.toml"
    )))
    .expect("repository configuration");
    // Keys come from the environment in real use; a literal keeps the test offline.
    for provider in &mut config.providers {
        provider.api_key =
            Some(config::ApiKeyConfig::Plaintext("synthetic-key".into()));
    }
    let reserve = config.compaction.reserve_tokens;
    let models = config
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|model| {
                (provider.id.as_str().to_owned(), model.id.clone())
            })
        })
        .collect::<Vec<_>>();
    assert!(!models.is_empty());
    let factory = ProviderFactory::new(ConfigHandle::from_config(config));
    for (provider, model) in models {
        let profile = factory
            .resolve(&provider, &model)
            .unwrap_or_else(|error| panic!("{provider}/{model}: {error}"))
            .profile()
            .clone();
        assert_eq!(
            profile.request_max_tokens,
            profile.max_output_tokens.min(reserve),
            "{provider}/{model}"
        );
    }
}

/// A prompt-cache tier the catalog says a model cannot honour is dropped once at startup,
/// so it never reaches rig to be skipped with a warning on every request.
#[test]
fn prompt_cache_keeps_only_catalog_supported_tiers() {
    use protocol::PromptCacheRetention::{Long, Short};
    for (preset, model, tier, configured, expected) in [
        ("deepseek", "deepseek-flash", "short", Short, Some(Short)),
        ("deepseek", "deepseek-flash", "long", Long, None),
        ("chatgpt", "gpt-5.3-codex", "short", Short, None),
        ("anthropic", "claude-fable-5", "long", Long, Some(Long)),
        // The catalog does not know this model, so nothing is refused.
        ("deepseek", "unlisted-audit", "long", Long, Some(Long)),
    ] {
        if model != "unlisted-audit" {
            // Guards against a catalog change silently making the case vacuous.
            assert!(
                !catalog_spec(preset, model).caching.retention.is_empty(),
                "{preset}/{model} must list its cache tiers"
            );
        }
        let document = format!(
            r#"
active_model = "{preset}/{model}"
[[providers]]
id = "{preset}"
display_name = "audit"
api_key = "synthetic-key"
prompt_cache = "{tier}"
[[providers.models]]
id = "{model}"
context_tokens = 32768
max_output_tokens = 128
"#
        );
        let configured_model = factory_from(&document)
            .resolve(preset, model)
            .unwrap_or_else(|error| panic!("{preset}/{model}: {error}"));
        assert_eq!(
            configured_model.profile().prompt_cache,
            expected,
            "{preset}/{model} configured {configured:?}"
        );
    }
}

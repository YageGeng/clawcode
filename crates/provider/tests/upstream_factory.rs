use config::{AppConfig, ConfigHandle};
use provider::ProviderFactory;

/// Every existing configured protocol stays constructible without catalog membership.
#[test]
fn configured_protocols_build_unknown_models() {
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
            .build_model(None)
            .expect("construct upstream model")
            .with_transport(configured.http_client());
        assert_eq!(model.id(), Some("unlisted-model"));
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
    assert!(
        !configured
            .profile()
            .input
            .supports(protocol::ModelInputModality::Image)
    );
    let capture = rig_core::test_utils::RecordingHttpClient::new(serde_json::json!({
        "id":"r","object":"chat.completion","created":0,"model":"unlisted-model",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}
    }).to_string());
    let model = configured.build_model(None).expect("model").with_transport(
        rig_core::http_client::DynHttpClient::new(capture.clone()),
    );
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

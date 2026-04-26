use llm::providers::{
    ApiKeyConfig, ApiProtocol, LlmCompletionModel, LlmConfig, LlmModel, LlmModelFactory,
    LlmModelRequest, LlmProvider,
};
use serde_json::json;

/// Builds a minimal model entry used by provider construction tests.
fn test_model(id: &str) -> LlmModel {
    LlmModel {
        id: id.to_string(),
        display_name: Some(id.to_string()),
        context_tokens: Some(128_000),
        max_output_tokens: Some(8_192),
        extra_param: serde_json::Value::Null,
    }
}

/// Verifies the factory can be created from a complete LLM configuration.
#[test]
fn factory_loads_complete_llm_config() {
    let config = LlmConfig {
        providers: vec![test_provider("openai", ApiProtocol::OpenAI, "gpt-test")],
    };
    let factory = LlmModelFactory::try_from_config(config).expect("valid config should load");

    assert!(factory.provider("openai").is_some());
}

/// Verifies config loading rejects duplicate provider ids before model construction.
#[test]
fn factory_rejects_duplicate_provider_ids() {
    let config = LlmConfig {
        providers: vec![
            test_provider("openai", ApiProtocol::OpenAI, "gpt-a"),
            test_provider("openai", ApiProtocol::OpenAI, "gpt-b"),
        ],
    };

    let error =
        LlmModelFactory::try_from_config(config).expect_err("duplicate provider ids should fail");

    assert!(error.to_string().contains("duplicate provider"));
}

/// Verifies config loading rejects duplicate model ids within one provider.
#[test]
fn factory_rejects_duplicate_model_ids_within_provider() {
    let mut provider = test_provider("openai", ApiProtocol::OpenAI, "gpt-test");
    provider.models.push(test_model("gpt-test"));
    let config = LlmConfig {
        providers: vec![provider],
    };

    let error =
        LlmModelFactory::try_from_config(config).expect_err("duplicate model ids should fail");

    assert!(error.to_string().contains("duplicate model"));
}

/// Verifies the high-level factory API accepts `{provider}/{model}` selectors.
#[test]
fn factory_builds_model_from_provider_model_ref() {
    let factory = LlmModelFactory::new(vec![test_provider(
        "deepseek",
        ApiProtocol::DeepSeek,
        "deepseek-chat",
    )]);

    let model = factory
        .completion_model_ref("deepseek/deepseek-chat")
        .expect("factory should build model from provider/model ref");

    let _dynamic_model: LlmCompletionModel = model.clone();
}

/// Verifies model references split only on the first slash so model ids may contain slashes.
#[test]
fn factory_model_ref_allows_slashes_inside_model_id() {
    let factory = LlmModelFactory::new(vec![test_provider(
        "openrouter",
        ApiProtocol::OpenAI,
        "openai/gpt-4o",
    )]);

    let model = factory
        .model_ref("openrouter/openai/gpt-4o")
        .expect("model ref should allow provider-prefixed model ids");

    assert_eq!(model.id, "openai/gpt-4o");
}

/// Verifies malformed model references fail before provider lookup.
#[test]
fn factory_rejects_invalid_model_ref() {
    let factory = LlmModelFactory::new(vec![test_provider(
        "openai",
        ApiProtocol::OpenAI,
        "gpt-test",
    )]);

    let error = match factory.completion_model_ref("openai") {
        Ok(_) => panic!("invalid model ref should fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("invalid model reference"));
}

/// Builds a minimal provider entry used by factory tests.
fn test_provider(id: &str, protocol: ApiProtocol, model_id: &str) -> LlmProvider {
    let base_url = match protocol {
        ApiProtocol::OpenAI => "https://api.openai.com/v1",
        ApiProtocol::DeepSeek => "https://api.deepseek.com",
    };

    LlmProvider {
        id: id.to_string(),
        display_name: id.to_string(),
        protocols: vec![protocol],
        base_url: base_url.to_string(),
        api_key: ApiKeyConfig::Key {
            value: "test-key".to_string(),
        },
        models: vec![test_model(model_id)],
    }
}

/// Verifies tagged provider auth config deserializes from the public TOML shape.
#[test]
fn deserializes_auth_api_key_config() {
    let config: ApiKeyConfig = toml::from_str(
        r#"
type = "auth"
"#,
    )
    .expect("auth api key config should deserialize");

    assert_eq!(config, ApiKeyConfig::Auth);
}

/// Verifies model-level extra params can be loaded from TOML as a JSON object.
#[test]
fn deserializes_model_extra_param_as_json_value() {
    let model: LlmModel = toml::from_str(
        r#"
id = "deepseek-reasoner"
display_name = "DeepSeek Reasoner"
context_tokens = 64000
max_output_tokens = 8192

[extra_param]
reasoning_effort = "high"

[extra_param.thinking]
type = "enabled"
"#,
    )
    .expect("model metadata should deserialize");

    assert_eq!(
        model.extra_param,
        json!({
            "reasoning_effort": "high",
            "thinking": {
                "type": "enabled",
            },
        })
    );
}

/// Verifies OpenAI protocol with managed auth builds an OAuth-backed Codex dynamic model.
#[test]
fn builds_openai_auth_model_as_oauth_backed_codex() {
    let provider = LlmProvider {
        id: "codex".to_string(),
        display_name: "Codex".to_string(),
        protocols: vec![ApiProtocol::OpenAI],
        base_url: "https://chatgpt.com/backend-api/codex".to_string(),
        api_key: ApiKeyConfig::Auth,
        models: vec![test_model("gpt-5.3-codex")],
    };

    let model = provider
        .completion_model(None, "gpt-5.3-codex")
        .expect("provider should build an OAuth-backed Codex completion model");

    let _dynamic_model: LlmCompletionModel = model.clone();
}

/// Verifies the model factory builds a dynamic model from provider and model identifiers.
#[test]
fn factory_builds_model_by_provider_and_model_id() {
    let factory = LlmModelFactory::new(vec![
        test_provider("openai", ApiProtocol::OpenAI, "gpt-test"),
        test_provider("deepseek", ApiProtocol::DeepSeek, "deepseek-chat"),
    ]);

    let model = factory
        .completion_model(LlmModelRequest {
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-chat".to_string(),
            protocol: None,
        })
        .expect("factory should build selected provider model");

    let _dynamic_model: LlmCompletionModel = model.clone();
}

/// Verifies unknown provider selections fail before protocol or client construction.
#[test]
fn factory_rejects_unknown_provider() {
    let factory = LlmModelFactory::new(vec![test_provider(
        "openai",
        ApiProtocol::OpenAI,
        "gpt-test",
    )]);

    let error = match factory.completion_model(LlmModelRequest {
        provider_id: "missing".to_string(),
        model_id: "gpt-test".to_string(),
        protocol: None,
    }) {
        Ok(_) => panic!("unknown provider should fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("does not define provider"));
}

/// Verifies the provider convenience method remains a wrapper around factory construction.
#[test]
fn provider_completion_model_delegates_to_factory() {
    let provider = test_provider("openai", ApiProtocol::OpenAI, "gpt-test");

    let model = provider
        .completion_model(None, "gpt-test")
        .expect("provider convenience method should still build a model");

    let _dynamic_model: LlmCompletionModel = model.clone();
}

/// Verifies provider construction uses the first configured protocol by default.
#[test]
fn builds_openai_model_from_key_auth_with_default_protocol() {
    let provider = LlmProvider {
        id: "openai".to_string(),
        display_name: "OpenAI".to_string(),
        protocols: vec![ApiProtocol::OpenAI],
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: ApiKeyConfig::Key {
            value: "test-key".to_string(),
        },
        models: vec![test_model("gpt-test")],
    };

    let model = provider
        .completion_model(None, "gpt-test")
        .expect("provider should build an OpenAI completion model");

    let _dynamic_model: LlmCompletionModel = model.clone();
}

/// Verifies unsupported protocol selections fail before any provider client is built.
#[test]
fn rejects_protocol_not_advertised_by_provider() {
    let provider = LlmProvider {
        id: "deepseek".to_string(),
        display_name: "DeepSeek".to_string(),
        protocols: vec![ApiProtocol::DeepSeek],
        base_url: "https://api.deepseek.com".to_string(),
        api_key: ApiKeyConfig::Key {
            value: "test-key".to_string(),
        },
        models: vec![test_model("deepseek-chat")],
    };

    let error = match provider.completion_model(Some(ApiProtocol::OpenAI), "deepseek-chat") {
        Ok(_) => panic!("unsupported protocol should fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("does not support protocol"));
}

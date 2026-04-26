mod common;

use std::{env, error::Error, io};

use llm::{
    completion::{CompletionModel as _, Message},
    providers::{
        ApiKeyConfig, ApiProtocol, LlmConfig, LlmModel, LlmModelFactory, LlmProvider,
        chatgpt::GPT_5_3_CODEX,
        deepseek::{DEEPSEEK_API_BASE_URL, DEEPSEEK_PRO},
        openai::{OPENAI_API_BASE_URL, codex::OPENAI_CODEX_API_BASE_URL},
    },
};
use serde_json::json;

/// Reads an optional environment variable, falling back to a static default.
fn env_or_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Reads the example protocol selector from the environment.
fn selected_protocol() -> Result<ApiProtocol, io::Error> {
    match env_or_default("LLM_PROVIDER_PROTOCOL", "openai")
        .to_ascii_lowercase()
        .as_str()
    {
        "openai" => Ok(ApiProtocol::OpenAI),
        "deepseek" => Ok(ApiProtocol::DeepSeek),
        other => Err(io::Error::other(format!(
            "unsupported LLM_PROVIDER_PROTOCOL `{other}`; use `openai` or `deepseek`"
        ))),
    }
}

/// Selects the OpenAI auth source used by the example provider.
fn selected_openai_auth() -> ApiKeyConfig {
    if env_or_default("LLM_PROVIDER_AUTH", "env").eq_ignore_ascii_case("auth") {
        ApiKeyConfig::Auth
    } else {
        ApiKeyConfig::Env {
            name: "OPENAI_API_KEY".to_string(),
        }
    }
}

/// Builds the example provider configuration for the selected protocol.
fn provider_config(protocol: ApiProtocol) -> LlmProvider {
    match protocol {
        ApiProtocol::OpenAI => {
            let api_key = selected_openai_auth();
            let is_managed_auth = matches!(api_key, ApiKeyConfig::Auth);
            let model_id = if is_managed_auth {
                env_or_default("CHATGPT_MODEL", GPT_5_3_CODEX)
            } else {
                env_or_default("OPENAI_MODEL", common::MODEL)
            };
            LlmProvider {
                id: if is_managed_auth { "codex" } else { "openai" }.to_string(),
                display_name: if is_managed_auth { "Codex" } else { "OpenAI" }.to_string(),
                protocols: vec![ApiProtocol::OpenAI],
                base_url: if is_managed_auth {
                    env_or_default("CHATGPT_BASE_URL", OPENAI_CODEX_API_BASE_URL)
                } else {
                    env_or_default("OPENAI_BASE_URL", OPENAI_API_BASE_URL)
                },
                api_key,
                models: vec![LlmModel {
                    id: model_id,
                    display_name: Some("OpenAI example model".to_string()),
                    context_tokens: None,
                    max_output_tokens: None,
                    extra_param: serde_json::Value::Null,
                }],
            }
        }
        ApiProtocol::DeepSeek => {
            let model_id = env_or_default("DEEPSEEK_MODEL", DEEPSEEK_PRO);
            LlmProvider {
                id: "deepseek".to_string(),
                display_name: "DeepSeek".to_string(),
                protocols: vec![ApiProtocol::DeepSeek],
                base_url: env_or_default("DEEPSEEK_BASE_URL", DEEPSEEK_API_BASE_URL),
                api_key: ApiKeyConfig::Env {
                    name: "DEEPSEEK_API_KEY".to_string(),
                },
                models: vec![LlmModel {
                    id: model_id,
                    display_name: Some("DeepSeek example model".to_string()),
                    context_tokens: None,
                    max_output_tokens: None,
                    extra_param: json!({
                        "reasoning_effort": "high",
                        "thinking": {
                            "type": "enabled",
                        },
                    }),
                }],
            }
        }
    }
}

/// Runs one request through the config-driven dynamic LLM provider interface.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let protocol = selected_protocol()?;
    let provider = provider_config(protocol);
    let model_id = provider
        .models
        .first()
        .map(|model| model.id.clone())
        .ok_or_else(|| io::Error::other("example provider has no configured models"))?;

    let model_ref = format!("{}/{model_id}", provider.id);
    let factory = LlmModelFactory::try_from_config(LlmConfig {
        providers: vec![provider.clone()],
    })?;
    let model = factory.completion_model_ref(&model_ref)?;
    let response = model
        .completion_request(Message::user(
            "In one short sentence, explain what a config-driven LLM provider does.",
        ))
        .send()
        .await?;

    println!("provider: {}", provider.display_name);
    println!("model: {model_id}");
    println!("text:\n{}", common::assistant_text(&response.choice));
    println!("usage: {:?}", response.usage);

    Ok(())
}

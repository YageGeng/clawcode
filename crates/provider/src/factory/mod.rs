//! Dynamic LLM factory: pre-builds configured providers into a cache,
//! dispatches provider_id/model_id → Arc<dyn Llm> via O(1) lookup.

mod event;

pub use event::{DynLlmStream, LlmCompletion, LlmStreamEvent};

use std::collections::HashMap;
use std::sync::Arc;

use config::{LlmProvider, ProviderId, ProviderType};
use futures::StreamExt;
use serde::Serialize;

use crate::client::CompletionClient;
use crate::completion::{CompletionError, CompletionModel, CompletionRequest, GetTokenUsage};
use crate::streaming::StreamingCompletionResponse;
use crate::wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync};

// ── Llm trait ───────────────────────────────────────────────────────────────

/// Dynamic LLM abstraction supporting both completion and streaming.
///
/// Implementations wrap a concrete [`CompletionModel`] behind this
/// object-safe facade so that callers can route requests without
/// knowing the provider type at compile time.
pub trait Llm: std::fmt::Debug + WasmCompatSend + WasmCompatSync {
    /// Returns the configured provider id (e.g. `"openai"`, `"deepseek"`).
    fn provider_id(&self) -> &str;

    /// Returns the configured model id (e.g. `"gpt-5"`, `"deepseek-v4-flash"`).
    fn model_id(&self) -> &str;

    /// Execute a provider-agnostic completion request.
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<event::LlmCompletion, CompletionError>>;

    /// Start a provider-agnostic streaming request.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<event::DynLlmStream, CompletionError>>;
}

/// Shared handle for dynamic LLM dispatch.
pub type ArcLlm = Arc<dyn Llm>;

// ── ProviderBackedLlm adapter ───────────────────────────────────────────────

/// Adapter that wraps a concrete [`CompletionModel`] behind the object-safe
/// [`Llm`] trait.
///
/// `M` can be any completion-model type whose response types are serializable.
struct ProviderBackedLlm<M> {
    provider_id: String,
    model_id: String,
    inner: M,
}

impl<M> ProviderBackedLlm<M> {
    fn new(provider_id: String, model_id: String, inner: M) -> Self {
        Self {
            provider_id,
            model_id,
            inner,
        }
    }
}

impl<M> std::fmt::Debug for ProviderBackedLlm<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderBackedLlm")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .finish()
    }
}

impl<M> Llm for ProviderBackedLlm<M>
where
    M: CompletionModel + WasmCompatSend + WasmCompatSync + 'static,
    M::Response: Serialize,
    M::StreamingResponse: GetTokenUsage + Serialize,
{
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn completion(
        &self,
        request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<event::LlmCompletion, CompletionError>> {
        Box::pin({
            let m = self.inner.clone();
            async move {
                let resp = m.completion(request).await?;
                Ok(event::LlmCompletion {
                    choice: resp.choice,
                    usage: resp.usage,
                    raw_response: serde_json::to_value(&resp.raw_response)
                        .map_err(CompletionError::JsonError)?,
                    message_id: resp.message_id,
                })
            }
        })
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<event::DynLlmStream, CompletionError>> {
        Box::pin({
            let m = self.inner.clone();
            async move {
                let stream_resp: StreamingCompletionResponse<M::StreamingResponse> =
                    m.stream(request).await?;
                let mapped = stream_resp.map(|item| item.and_then(event::LlmStreamEvent::try_from));
                Ok(Box::pin(mapped) as event::DynLlmStream)
            }
        })
    }
}

// ── LlmFactory ──────────────────────────────────────────────────────────────

/// Factory that pre-builds all configured providers at construction time,
/// then dispatches `provider_id` / `model_id` via O(1) cache lookup.
pub struct LlmFactory {
    cache: HashMap<String, ArcLlm>,
}

impl LlmFactory {
    /// Build all providers and models from the active configuration into a
    /// shared cache. Entries that fail to construct (unsupported protocol,
    /// missing env var, etc.) are logged and skipped.
    pub fn new(config: config::ConfigHandle) -> Self {
        let cfg = config.current();
        let mut cache = HashMap::new();

        for provider in &cfg.providers {
            for model in &provider.models {
                let key = Self::cache_key(provider.id.as_str(), &model.id);
                if cache.contains_key(&key) {
                    continue;
                }
                match Self::build_one(provider, &model.id) {
                    Ok(llm) => {
                        cache.insert(key, llm);
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "clawcode::factory",
                            "skip provider={} model={}: {}",
                            provider.id.as_str(),
                            model.id,
                            e
                        );
                    }
                }
            }
        }

        Self { cache }
    }

    /// Look up a pre-built LLM handle by provider and model id.
    ///
    /// Returns `None` when the combination was not found in configuration
    /// or failed to build at construction time.
    pub fn get(&self, provider_id: &str, model_id: &str) -> Option<ArcLlm> {
        self.cache
            .get(&Self::cache_key(provider_id, model_id))
            .map(Arc::clone)
    }

    /// Cache key combining provider and model identifiers.
    fn cache_key(provider_id: &str, model_id: &str) -> String {
        format!("{}/{}", provider_id, model_id)
    }

    /// Build a single client for the given provider and model id.
    fn build_one(provider: &LlmProvider, model_id: &str) -> Result<ArcLlm, BuildError> {
        let pid = provider.id.as_str();
        let api_key = provider
            .api_key
            .resolve()
            .map_err(|source| BuildError::ApiKeyResolve {
                provider_id: pid.to_string(),
                source,
            })?;
        let base_url = provider.base_url.clone();

        match provider.provider_type {
            #[cfg(not(target_arch = "wasm32"))]
            ProviderType::Responses => match &provider.id {
                ProviderId::Openai => {
                    use crate::client::BearerAuth;
                    use crate::providers::openai;
                    let client = openai::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Chatgpt => {
                    use crate::providers::chatgpt;
                    let client = chatgpt::Client::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Other(_) => {
                    use crate::client::BearerAuth;
                    use crate::providers::openai;
                    let client = openai::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                _ => Err(BuildError::UnsupportedProtocol {
                    provider_id: pid.to_string(),
                    provider_type: ProviderType::Responses,
                }),
            },
            #[cfg(not(target_arch = "wasm32"))]
            ProviderType::OpenaiCompletions => match &provider.id {
                ProviderId::Openai => {
                    use crate::client::BearerAuth;
                    use crate::providers::openai;
                    let client = openai::CompletionsClient::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Deepseek => {
                    use crate::client::BearerAuth;
                    use crate::providers::deepseek;
                    let client = deepseek::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Moonshot => {
                    use crate::client::BearerAuth;
                    use crate::providers::moonshot;
                    let client = moonshot::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Minimax => {
                    use crate::client::BearerAuth;
                    use crate::providers::minimax;
                    let client = minimax::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Xiaomimimo => {
                    use crate::client::BearerAuth;
                    use crate::providers::xiaomimimo;
                    let client = xiaomimimo::Client::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Other(_) => {
                    use crate::client::BearerAuth;
                    use crate::providers::openai;
                    let client = openai::CompletionsClient::builder()
                        .api_key(BearerAuth::from(api_key))
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                _ => Err(BuildError::UnsupportedProtocol {
                    provider_id: pid.to_string(),
                    provider_type: ProviderType::OpenaiCompletions,
                }),
            },
            #[cfg(not(target_arch = "wasm32"))]
            ProviderType::Anthropic => match &provider.id {
                ProviderId::Anthropic => {
                    use crate::providers::anthropic;
                    let client = anthropic::Client::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Moonshot => {
                    use crate::providers::moonshot;
                    let client = moonshot::AnthropicClient::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Minimax => {
                    use crate::providers::minimax;
                    let client = minimax::AnthropicClient::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Xiaomimimo => {
                    use crate::providers::xiaomimimo;
                    let client = xiaomimimo::AnthropicClient::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                ProviderId::Other(_) => {
                    use crate::providers::anthropic;
                    let client = anthropic::Client::builder()
                        .api_key(api_key)
                        .base_url(base_url)
                        .build()
                        .map_err(|e| BuildError::ClientBuild(e.to_string()))?;
                    Ok(wrap(pid, model_id, client.completion_model(model_id)))
                }
                _ => Err(BuildError::UnsupportedProtocol {
                    provider_id: pid.to_string(),
                    provider_type: ProviderType::Anthropic,
                }),
            },
        }
    }
}

// ── Per-provider client construction ────────────────────────────────────────

/// Internal error type for per-entry build failures.
#[derive(Debug, thiserror::Error)]
enum BuildError {
    #[error("client build error: {0}")]
    ClientBuild(String),
    #[error("api key resolve error for provider `{provider_id}`: {source}")]
    ApiKeyResolve {
        provider_id: String,
        #[source]
        source: std::env::VarError,
    },
    #[error("unsupported provider type `{provider_type:?}` for provider `{provider_id}`")]
    UnsupportedProtocol {
        provider_id: String,
        provider_type: ProviderType,
    },
}

/// Wrap a concrete completion model in an [`ArcLlm`] adapter.
fn wrap<M>(provider_id: &str, model_id: &str, model: M) -> ArcLlm
where
    M: CompletionModel + WasmCompatSend + WasmCompatSync + 'static,
    M::Response: Serialize,
    M::StreamingResponse: GetTokenUsage + Serialize,
{
    Arc::new(ProviderBackedLlm::new(
        provider_id.to_string(),
        model_id.to_string(),
        model,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::AppConfig;

    /// get returns None for unknown provider ids.
    #[test]
    fn get_returns_none_for_unknown_provider() {
        let factory = LlmFactory::new(config::ConfigHandle::from_config(AppConfig::default()));
        assert!(factory.get("nonexistent", "gpt-5").is_none());
    }

    /// get returns None when the model id is not in configuration.
    #[test]
    fn get_returns_none_for_unknown_model() {
        let toml = r#"
[[providers]]
id = "openai"
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "sk-test"
"#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        let factory = LlmFactory::new(config::ConfigHandle::from_config(cfg));
        assert!(factory.get("openai", "nonexistent").is_none());
    }

    /// An unsupported provider_type / id combo is skipped at build time.
    #[test]
    fn unsupported_protocol_is_skipped() {
        let toml = r#"
[[providers]]
id = "openai"
display_name = "OpenAI"
provider_type = "anthropic"
base_url = "https://api.openai.com/v1"
api_key = "sk-test"

[[providers.models]]
id = "gpt-5"
"#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        let factory = LlmFactory::new(config::ConfigHandle::from_config(cfg));
        assert!(factory.get("openai", "gpt-5").is_none());
    }

    /// DeepSeek under openai-completions type is built and cached.
    #[test]
    fn deepseek_is_cached_under_openai_completions_type() {
        let toml = r#"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
provider_type = "openai-completions"
base_url = "https://api.deepseek.com"
api_key = "sk-test"

[[providers.models]]
id = "deepseek-v4-flash"
"#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        let factory = LlmFactory::new(config::ConfigHandle::from_config(cfg));
        let llm = factory.get("deepseek", "deepseek-v4-flash").unwrap();
        assert_eq!(llm.provider_id(), "deepseek");
        assert_eq!(llm.model_id(), "deepseek-v4-flash");
    }

    /// API keys from environment references are resolved at build time.
    #[test]
    fn api_key_from_env_is_resolved() {
        #[allow(clippy::result_large_err)]
        figment::Jail::expect_with(|jail| {
            jail.set_env("DEEPSEEK_API_KEY", "sk-env");
            let toml = r#"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
provider_type = "openai-completions"
base_url = "https://api.deepseek.com"

[providers.api_key]
env = "DEEPSEEK_API_KEY"

[[providers.models]]
id = "deepseek-v4-flash"
"#;
            let cfg: AppConfig = toml::from_str(toml).unwrap();
            let factory = LlmFactory::new(config::ConfigHandle::from_config(cfg));
            let llm = factory.get("deepseek", "deepseek-v4-flash").unwrap();
            assert_eq!(llm.provider_id(), "deepseek");
            Ok(())
        });
    }

    /// Unknown provider ids fall back to the default client for each protocol.
    #[test]
    fn other_provider_id_falls_back_to_protocol_default() {
        let toml = r#"
[[providers]]
id = "custom-openai"
display_name = "Custom OpenAI"
provider_type = "openai-completions"
base_url = "https://example.com/v1"
api_key = "sk-test"

[[providers.models]]
id = "custom-model"
"#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        let factory = LlmFactory::new(config::ConfigHandle::from_config(cfg));
        let llm = factory.get("custom-openai", "custom-model").unwrap();
        assert_eq!(llm.provider_id(), "custom-openai");
        assert_eq!(llm.model_id(), "custom-model");
    }
}

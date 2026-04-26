use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snafu::{OptionExt, ResultExt, Snafu, ensure};

use crate::{
    client::FinalCompletionResponse,
    completion::{
        CompletionError, CompletionRequest, CompletionResponse as CoreCompletionResponse,
        request::CompletionModel,
    },
    providers::{chatgpt, deepseek, openai},
    streaming::{
        RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent,
        StreamingCompletionResponse,
    },
    usage::GetTokenUsage,
};

/// Boxed future returned by the object-safe LLM completion interface.
pub type BoxLlmFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Supported provider API dialects that can be used to build a unified completion model.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum ApiProtocol {
    /// OpenAI-compatible chat completions API.
    #[serde(rename = "openai")]
    OpenAI,
    /// DeepSeek chat completions API.
    #[serde(rename = "deepseek")]
    DeepSeek,
}

/// API key or session configuration used by an LLM provider.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiKeyConfig {
    /// Reads a bearer token from the named environment variable.
    Env { name: String },
    /// Uses the provided bearer token directly.
    Key { value: String },
    /// Uses provider-managed authentication such as OAuth/device-code login.
    Auth,
}

/// User-visible model metadata attached to an [`LlmProvider`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmModel {
    /// Provider model identifier sent in request payloads.
    pub id: String,
    /// Optional display name for user interfaces.
    pub display_name: Option<String>,
    /// Optional context window size in tokens.
    pub context_tokens: Option<u64>,
    /// Optional maximum output size in tokens.
    pub max_output_tokens: Option<u64>,
    /// Provider-specific parameters merged into every request for this model.
    #[serde(default)]
    pub extra_param: serde_json::Value,
}

/// Provider metadata plus enough auth and model information to build a completion model.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmProvider {
    /// Stable provider identifier used by configuration.
    pub id: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// API dialects supported by this provider.
    pub protocols: Vec<ApiProtocol>,
    /// Base URL used for provider requests.
    pub base_url: String,
    /// Required API key or auth-session source.
    pub api_key: ApiKeyConfig,
    /// Models available through this provider.
    pub models: Vec<LlmModel>,
}

/// Complete LLM configuration loaded by the model factory.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmConfig {
    /// All configured LLM providers.
    pub providers: Vec<LlmProvider>,
}

/// Request used by [`LlmModelFactory`] to select and build a model.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmModelRequest {
    /// Provider identifier to select from the factory registry.
    pub provider_id: String,
    /// Model identifier to select from the provider metadata.
    pub model_id: String,
    /// Optional API protocol override; defaults to the provider's first protocol.
    pub protocol: Option<ApiProtocol>,
}

/// Factory that builds dynamic LLM models from provider configuration.
#[derive(Debug, Clone)]
pub struct LlmModelFactory {
    providers: Vec<LlmProvider>,
    provider_index: BTreeMap<String, usize>,
}

/// Errors raised while resolving provider config into a concrete model.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum LlmProviderError {
    /// Model reference did not use the expected `{provider}/{model}` shape.
    #[snafu(display(
        "invalid model reference `{model_ref}` at `{stage}`; expected `{{provider}}/{{model}}`"
    ))]
    InvalidModelRef { model_ref: String, stage: String },
    /// Requested provider is not present in the factory registry.
    #[snafu(display("model factory does not define provider `{provider_id}` at `{stage}`"))]
    UnknownProvider { provider_id: String, stage: String },
    /// Config contains more than one provider with the same id.
    #[snafu(display("duplicate provider `{provider_id}` at `{stage}`"))]
    DuplicateProvider { provider_id: String, stage: String },
    /// Config contains more than one model with the same id under one provider.
    #[snafu(display("duplicate model `{model_id}` for provider `{provider_id}` at `{stage}`"))]
    DuplicateModel {
        provider_id: String,
        model_id: String,
        stage: String,
    },
    /// Provider has no configured protocol to select by default.
    #[snafu(display("provider `{provider_id}` has no configured API protocols at `{stage}`"))]
    MissingProtocol { provider_id: String, stage: String },
    /// Requested protocol is not listed in the provider's supported protocols.
    #[snafu(display(
        "provider `{provider_id}` does not support protocol `{protocol:?}` at `{stage}`"
    ))]
    UnsupportedProtocol {
        provider_id: String,
        protocol: ApiProtocol,
        stage: String,
    },
    /// Requested model is not listed in the provider's available models.
    #[snafu(display("provider `{provider_id}` does not define model `{model_id}` at `{stage}`"))]
    UnknownModel {
        provider_id: String,
        model_id: String,
        stage: String,
    },
    /// Environment variable auth failed while resolving the provider key.
    #[snafu(display(
        "failed to read API key environment variable `{name}` at `{stage}`: {source}"
    ))]
    EnvKey {
        name: String,
        source: std::env::VarError,
        stage: String,
    },
    /// Managed auth was selected for a protocol that does not implement it yet.
    #[snafu(display("managed auth is not supported for protocol `{protocol:?}` at `{stage}`"))]
    ManagedAuthUnsupported {
        protocol: ApiProtocol,
        stage: String,
    },
    /// Provider client construction failed.
    #[snafu(display("failed to build provider client at `{stage}`: {source}"))]
    Client {
        source: crate::http_client::Error,
        stage: String,
    },
}

/// Result alias for provider resolution operations.
pub type Result<T> = std::result::Result<T, LlmProviderError>;

/// Object-safe interface used by callers that do not need provider-specific model types.
pub trait LlmCompletion: Send + Sync {
    /// Sends one completion request through the configured provider.
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> BoxLlmFuture<
        '_,
        std::result::Result<CoreCompletionResponse<serde_json::Value>, CompletionError>,
    >;

    /// Starts one streaming completion request through the configured provider.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> BoxLlmFuture<
        '_,
        std::result::Result<StreamingCompletionResponse<FinalCompletionResponse>, CompletionError>,
    >;
}

/// Shared dynamic completion model returned by [`LlmProvider`].
pub type LlmCompletionModel = Arc<dyn LlmCompletion>;

/// Generic wrapper that merges model config before delegating to a concrete provider model.
struct ProviderCompletionModel<M> {
    inner: M,
    extra_param: serde_json::Value,
}

impl LlmProvider {
    /// Returns the model metadata for `model_id` when it is configured on this provider.
    pub fn find_model(&self, model_id: &str) -> Option<&LlmModel> {
        self.models.iter().find(|model| model.id == model_id)
    }

    /// Selects an explicit protocol or falls back to the provider's first configured protocol.
    pub fn select_protocol(&self, protocol: Option<ApiProtocol>) -> Result<ApiProtocol> {
        let selected = match protocol {
            Some(protocol) => protocol,
            None => *self.protocols.first().context(MissingProtocolSnafu {
                provider_id: self.id.clone(),
                stage: "select-provider-protocol".to_string(),
            })?,
        };

        ensure!(
            self.protocols.contains(&selected),
            UnsupportedProtocolSnafu {
                provider_id: self.id.clone(),
                protocol: selected,
                stage: "validate-provider-protocol".to_string(),
            }
        );

        Ok(selected)
    }

    /// Resolves the configured key source into a bearer token string.
    pub fn resolve_api_key(&self) -> Result<String> {
        let protocol = self
            .protocols
            .first()
            .copied()
            .unwrap_or(ApiProtocol::OpenAI);
        self.resolve_api_key_for(protocol)
    }

    /// Resolves a static key source for a protocol that does not support managed auth.
    fn resolve_api_key_for(&self, protocol: ApiProtocol) -> Result<String> {
        match &self.api_key {
            ApiKeyConfig::Env { name } => std::env::var(name).context(EnvKeySnafu {
                name: name.clone(),
                stage: "resolve-env-api-key".to_string(),
            }),
            ApiKeyConfig::Key { value } => Ok(value.clone()),
            ApiKeyConfig::Auth => ManagedAuthUnsupportedSnafu {
                protocol,
                stage: "resolve-managed-auth-api-key".to_string(),
            }
            .fail(),
        }
    }

    /// Builds a unified completion model for the selected protocol and configured model.
    pub fn completion_model(
        &self,
        protocol: Option<ApiProtocol>,
        model_id: &str,
    ) -> Result<LlmCompletionModel> {
        LlmModelFactory::new(vec![self.clone()]).completion_model(LlmModelRequest {
            provider_id: self.id.clone(),
            model_id: model_id.to_string(),
            protocol,
        })
    }

    /// Resolves ChatGPT/Codex auth without forcing OAuth configs to contain an access token.
    fn codex_auth(&self) -> Result<chatgpt::ChatGPTAuth> {
        match &self.api_key {
            ApiKeyConfig::Env { name } => Ok(chatgpt::ChatGPTAuth::AccessToken {
                access_token: std::env::var(name).context(EnvKeySnafu {
                    name: name.clone(),
                    stage: "resolve-codex-env-access-token".to_string(),
                })?,
            }),
            ApiKeyConfig::Key { value } => Ok(chatgpt::ChatGPTAuth::AccessToken {
                access_token: value.clone(),
            }),
            ApiKeyConfig::Auth => Ok(chatgpt::ChatGPTAuth::OAuth),
        }
    }
}

impl LlmModelFactory {
    /// Builds a model factory from configured providers.
    pub fn new(providers: Vec<LlmProvider>) -> Self {
        let provider_index = providers
            .iter()
            .enumerate()
            .map(|(index, provider)| (provider.id.clone(), index))
            .collect();
        Self {
            providers,
            provider_index,
        }
    }

    /// Builds a validated model factory from a complete LLM configuration.
    pub fn try_from_config(config: LlmConfig) -> Result<Self> {
        validate_providers(&config.providers)?;
        Ok(Self::new(config.providers))
    }

    /// Builds a model factory from a trusted complete LLM configuration.
    pub fn from_config(config: LlmConfig) -> Self {
        Self::try_from_config(config)
            .expect("LlmConfig should not contain duplicate provider or model ids")
    }

    /// Returns the configured provider with the requested identifier.
    pub fn provider(&self, provider_id: &str) -> Option<&LlmProvider> {
        self.provider_index
            .get(provider_id)
            .and_then(|index| self.providers.get(*index))
    }

    /// Returns model metadata for a provider/model identifier pair.
    pub fn model(&self, provider_id: &str, model_id: &str) -> Result<&LlmModel> {
        let provider = self.provider(provider_id).context(UnknownProviderSnafu {
            provider_id: provider_id.to_string(),
            stage: "find-factory-provider".to_string(),
        })?;

        provider.find_model(model_id).context(UnknownModelSnafu {
            provider_id: provider.id.clone(),
            model_id: model_id.to_string(),
            stage: "find-factory-model".to_string(),
        })
    }

    /// Returns model metadata from a `{provider}/{model}` reference.
    pub fn model_ref(&self, model_ref: &str) -> Result<&LlmModel> {
        let request = self.parse_model_ref(model_ref)?;
        self.model(&request.provider_id, &request.model_id)
    }

    /// Builds a dynamic completion model from a `{provider}/{model}` reference.
    pub fn completion_model_ref(&self, model_ref: &str) -> Result<LlmCompletionModel> {
        let request = self.parse_model_ref(model_ref)?;
        self.completion_model(request)
    }

    /// Builds a dynamic completion model from provider, model and protocol selection.
    pub fn completion_model(&self, request: LlmModelRequest) -> Result<LlmCompletionModel> {
        let provider = self
            .provider(&request.provider_id)
            .context(UnknownProviderSnafu {
                provider_id: request.provider_id.clone(),
                stage: "find-factory-provider".to_string(),
            })?;
        let model = provider
            .find_model(&request.model_id)
            .context(UnknownModelSnafu {
                provider_id: provider.id.clone(),
                model_id: request.model_id.clone(),
                stage: "find-factory-model".to_string(),
            })?;

        let protocol = provider.select_protocol(request.protocol)?;
        let extra_param = model.extra_param.clone();

        match protocol {
            ApiProtocol::OpenAI => {
                if matches!(provider.api_key, ApiKeyConfig::Auth) {
                    Self::build_codex_model(provider, &request.model_id, extra_param)
                } else {
                    Self::build_openai_model(provider, &request.model_id, protocol, extra_param)
                }
            }
            ApiProtocol::DeepSeek => {
                let api_key = provider.resolve_api_key_for(protocol)?;
                let client = deepseek::Client::builder()
                    .base_url(&provider.base_url)
                    .api_key(api_key)
                    .build()
                    .context(ClientSnafu {
                        stage: "build-deepseek-client".to_string(),
                    })?;

                Ok(Arc::new(ProviderCompletionModel {
                    inner: deepseek::completion::CompletionModel::new(client, &request.model_id),
                    extra_param,
                }))
            }
        }
    }

    /// Builds a standard OpenAI chat-completions model from static API-key auth.
    fn build_openai_model(
        provider: &LlmProvider,
        model_id: &str,
        protocol: ApiProtocol,
        extra_param: serde_json::Value,
    ) -> Result<LlmCompletionModel> {
        let api_key = provider.resolve_api_key_for(protocol)?;
        let client = openai::Client::builder()
            .base_url(&provider.base_url)
            .api_key(api_key)
            .build()
            .context(ClientSnafu {
                stage: "build-openai-client".to_string(),
            })?;

        Ok(Arc::new(ProviderCompletionModel {
            inner: openai::completion::CompletionModel::with_model(
                client.completions_api(),
                model_id,
            ),
            extra_param,
        }))
    }

    /// Builds a ChatGPT/Codex subscription model when OpenAI protocol uses managed auth.
    fn build_codex_model(
        provider: &LlmProvider,
        model_id: &str,
        extra_param: serde_json::Value,
    ) -> Result<LlmCompletionModel> {
        let builder = chatgpt::Client::builder().base_url(&provider.base_url);
        let client = match provider.codex_auth()? {
            chatgpt::ChatGPTAuth::AccessToken { access_token } => builder
                .api_key(chatgpt::ChatGPTAuth::AccessToken { access_token })
                .build(),
            chatgpt::ChatGPTAuth::OAuth => builder.oauth().build(),
        }
        .context(ClientSnafu {
            stage: "build-codex-client".to_string(),
        })?;

        Ok(Arc::new(ProviderCompletionModel {
            inner: chatgpt::WsCompletionModel::new(chatgpt::ResponsesCompletionModel::new(
                client, model_id,
            )),
            extra_param,
        }))
    }

    /// Parses a `{provider}/{model}` reference, preserving slashes inside the model id.
    fn parse_model_ref(&self, model_ref: &str) -> Result<LlmModelRequest> {
        let (provider_id, model_id) = model_ref.split_once('/').context(InvalidModelRefSnafu {
            model_ref: model_ref.to_string(),
            stage: "parse-model-ref".to_string(),
        })?;

        ensure!(
            !provider_id.trim().is_empty() && !model_id.trim().is_empty(),
            InvalidModelRefSnafu {
                model_ref: model_ref.to_string(),
                stage: "validate-model-ref".to_string(),
            }
        );

        Ok(LlmModelRequest {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            protocol: None,
        })
    }
}

impl TryFrom<LlmConfig> for LlmModelFactory {
    type Error = LlmProviderError;

    /// Builds a validated model factory from a complete LLM configuration.
    fn try_from(value: LlmConfig) -> std::result::Result<Self, Self::Error> {
        Self::try_from_config(value)
    }
}

/// Validates that provider and per-provider model identifiers are unambiguous.
fn validate_providers(providers: &[LlmProvider]) -> Result<()> {
    let mut provider_ids = std::collections::BTreeSet::new();
    for provider in providers {
        ensure!(
            provider_ids.insert(provider.id.clone()),
            DuplicateProviderSnafu {
                provider_id: provider.id.clone(),
                stage: "validate-provider-ids".to_string(),
            }
        );

        let mut model_ids = std::collections::BTreeSet::new();
        for model in &provider.models {
            ensure!(
                model_ids.insert(model.id.clone()),
                DuplicateModelSnafu {
                    provider_id: provider.id.clone(),
                    model_id: model.id.clone(),
                    stage: "validate-model-ids".to_string(),
                }
            );
        }
    }

    Ok(())
}

impl<M> LlmCompletion for ProviderCompletionModel<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: Serialize,
    M::StreamingResponse:
        Clone + Unpin + Serialize + DeserializeOwned + GetTokenUsage + Send + 'static,
{
    /// Sends one completion request through the selected provider model.
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> BoxLlmFuture<
        '_,
        std::result::Result<CoreCompletionResponse<serde_json::Value>, CompletionError>,
    > {
        Box::pin(async move {
            let request = apply_extra_params(request, &self.extra_param);
            normalize_completion_response(self.inner.completion(request).await)
        })
    }

    /// Starts one streaming completion request through the selected provider model.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> BoxLlmFuture<
        '_,
        std::result::Result<StreamingCompletionResponse<FinalCompletionResponse>, CompletionError>,
    > {
        Box::pin(async move {
            let request = apply_extra_params(request, &self.extra_param);
            let stream = self.inner.stream(request).await?;
            Ok(normalize_streaming_response(stream))
        })
    }
}

impl CompletionModel for LlmCompletionModel {
    type Response = serde_json::Value;
    type StreamingResponse = FinalCompletionResponse;
    type Client = LlmProvider;

    /// Builds a dynamic model from a provider using its default protocol.
    ///
    /// This compatibility hook must satisfy the infallible [`CompletionModel::make`] contract,
    /// so invalid provider config will panic here. Config-driven callers should prefer
    /// [`LlmModelFactory::try_from_config`] plus [`LlmModelFactory::completion_model_ref`].
    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        let model = model.into();
        client
            .completion_model(None, &model)
            .expect("LlmProvider::completion_model should build from valid provider config")
    }

    /// Delegates a request through the object-safe completion interface.
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> std::result::Result<CoreCompletionResponse<Self::Response>, CompletionError> {
        LlmCompletion::completion(self.as_ref(), request).await
    }

    /// Delegates a streaming request through the object-safe completion interface.
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> std::result::Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>
    {
        LlmCompletion::stream(self.as_ref(), request).await
    }
}

/// Adds configured model parameters to a request before provider conversion.
fn apply_extra_params(
    mut request: CompletionRequest,
    extra_param: &serde_json::Value,
) -> CompletionRequest {
    request.additional_params = merge_extra_params(request.additional_params, extra_param);
    request
}

/// Merges model-level extra parameters into request-level additional parameters.
fn merge_extra_params(
    additional_params: Option<serde_json::Value>,
    extra_param: &serde_json::Value,
) -> Option<serde_json::Value> {
    if extra_param.is_null() {
        return additional_params;
    }

    Some(match additional_params {
        Some(existing) => crate::json_utils::merge(extra_param.clone(), existing),
        None => extra_param.clone(),
    })
}

/// Converts provider-specific raw completion responses into a JSON raw response.
fn normalize_completion_response<R>(
    response: std::result::Result<CoreCompletionResponse<R>, CompletionError>,
) -> std::result::Result<CoreCompletionResponse<serde_json::Value>, CompletionError>
where
    R: Serialize,
{
    let response = response?;
    let raw_response = serde_json::to_value(response.raw_response).map_err(|source| {
        CompletionError::Serialize {
            source,
            stage: "serialize-unified-raw-response".to_string(),
        }
    })?;

    Ok(CoreCompletionResponse {
        choice: response.choice,
        usage: response.usage,
        raw_response,
        message_id: response.message_id,
    })
}

/// Converts a provider-specific stream into the unified stream response type.
fn normalize_streaming_response<R>(
    mut stream: StreamingCompletionResponse<R>,
) -> StreamingCompletionResponse<FinalCompletionResponse>
where
    R: Clone + Unpin + Serialize + DeserializeOwned + GetTokenUsage + Send + 'static,
{
    let mapped = async_stream::stream! {
        while let Some(item) = stream.next().await {
            yield normalize_stream_item(item);
        }

        if let Some(message_id) = stream.message_id.clone() {
            yield Ok(RawStreamingChoice::MessageId(message_id));
        }
    };

    StreamingCompletionResponse::stream(Box::pin(mapped))
}

/// Converts one streamed provider item into the unified raw streaming choice.
fn normalize_stream_item<R>(
    item: std::result::Result<StreamedAssistantContent<R>, CompletionError>,
) -> std::result::Result<RawStreamingChoice<FinalCompletionResponse>, CompletionError>
where
    R: Clone + Unpin + GetTokenUsage,
{
    match item? {
        StreamedAssistantContent::Text(text) => Ok(RawStreamingChoice::Message(text.text)),
        StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id,
        } => Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall {
            id: tool_call.id,
            internal_call_id,
            call_id: tool_call.call_id,
            name: tool_call.function.name,
            arguments: tool_call.function.arguments,
            signature: tool_call.signature,
            additional_params: tool_call.additional_params,
        })),
        StreamedAssistantContent::ToolCallDelta {
            id,
            internal_call_id,
            content,
        } => Ok(RawStreamingChoice::ToolCallDelta {
            id,
            internal_call_id,
            content,
        }),
        StreamedAssistantContent::Reasoning(reasoning) => {
            let content =
                reasoning
                    .content
                    .into_iter()
                    .next()
                    .ok_or_else(|| CompletionError::Response {
                        msg: "reasoning stream item did not include content".to_string(),
                    })?;

            Ok(RawStreamingChoice::Reasoning {
                id: reasoning.id,
                content,
            })
        }
        StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
            Ok(RawStreamingChoice::ReasoningDelta { id, reasoning })
        }
        StreamedAssistantContent::Final(response) => {
            Ok(RawStreamingChoice::FinalResponse(FinalCompletionResponse {
                usage: response.token_usage(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::merge_extra_params;

    /// Verifies model-level extra parameters are forwarded without requiring fixed field names.
    #[test]
    fn merge_extra_params_lets_request_params_override_model_defaults() {
        let merged = merge_extra_params(
            Some(json!({
                "stream": true,
                "reasoning_effort": "low",
                "metadata": {
                    "source": "request"
                }
            })),
            &json!({
                "reasoning_effort": "high",
                "thinking": {
                    "type": "enabled",
                },
            }),
        );

        assert_eq!(
            merged,
            Some(json!({
                "stream": true,
                "metadata": {
                    "source": "request"
                },
                "reasoning_effort": "low",
                "thinking": {
                    "type": "enabled",
                },
            }))
        );
    }
}

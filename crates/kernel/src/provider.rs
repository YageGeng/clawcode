use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use protocol::{
    AgentMessage, ContentBlock, MessageContent, ModelFailure, ModelFinal,
    ModelProfile, ModelRequest, ModelRetryDisposition, ModelStreamEvent,
    ModelUsage, Role, StopReason, ToolCall as AgentToolCall, ToolCallId,
};
use provider::OneOrMany;
use provider::completion::{
    CompletionError, CompletionRequest, RetryDisposition,
    ToolDefinition as ProviderToolDefinition,
};
use provider::factory::{
    ArcLlm, LlmFactory, LlmStreamEvent, ProviderFinal, ProviderFinishReason,
};
use provider::message::{
    AssistantContent, ImageMediaType, Message, MimeType, ReasoningContent,
    ToolResult as ProviderToolResult, ToolResultContent, UserContent,
};

use crate::{Model, ModelCatalog, ModelError, ModelFactory, ModelStream};
use tokio_util::sync::CancellationToken;

/// Resolves the configured active model through the retained provider factory.
pub struct ProviderModelFactory {
    config: config::ConfigHandle,
    providers: Arc<LlmFactory>,
}

impl ProviderModelFactory {
    /// Creates an adapter factory over an already initialized provider cache.
    #[must_use]
    pub fn new(
        config: config::ConfigHandle,
        providers: Arc<LlmFactory>,
    ) -> Self {
        Self { config, providers }
    }

    /// Constructs the retained provider cache from the immutable TOML config.
    #[must_use]
    pub fn from_config(config: config::ConfigHandle) -> Self {
        let providers = Arc::new(LlmFactory::new(config.clone()));
        Self::new(config, providers)
    }
}

impl ModelFactory for ProviderModelFactory {
    /// Resolves `provider/model` once so one kernel lifecycle has a stable model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        let config = self.config.current();
        let profile = config
            .model_profile()
            .map_err(|error| ModelError::Unavailable(error.to_string()))?;
        let llm = self
            .providers
            .resolve(&profile.provider_id, &profile.model_id)
            .map_err(|error| ModelError::Unavailable(error.to_string()))?;
        Ok(Arc::new(ProviderModel::new(
            profile,
            llm,
            config.retry.provider.clone(),
        )))
    }

    /// Builds all configured and statically contributed models into one catalog.
    fn create_catalog(
        &self,
        registration: &protocol::StaticExtensionRegistration,
    ) -> Result<ModelCatalog, ModelError> {
        let mut merged = self.config.current().as_ref().clone();
        for contributed in &registration.providers {
            if merged
                .providers
                .iter()
                .any(|provider| provider.id.as_str() == contributed.name)
            {
                return Err(ModelError::Unavailable(format!(
                    "extension provider duplicates configured provider: {}",
                    contributed.name
                )));
            }
            let mut value = contributed.config.clone();
            let object = value.as_object_mut().ok_or_else(|| {
                ModelError::Unavailable(format!(
                    "extension provider '{}' config must be an object",
                    contributed.name
                ))
            })?;
            if let Some(id) = object.get("id").and_then(|id| id.as_str()) {
                if id != contributed.name {
                    return Err(ModelError::Unavailable(format!(
                        "extension provider name '{}' does not match config id '{id}'",
                        contributed.name
                    )));
                }
            } else {
                object.insert(
                    "id".to_string(),
                    serde_json::Value::String(contributed.name.clone()),
                );
            }
            object.entry("display_name").or_insert_with(|| {
                serde_json::Value::String(contributed.name.clone())
            });
            let provider: config::LlmProvider = serde_json::from_value(value)
                .map_err(|error| {
                ModelError::Unavailable(format!(
                    "extension provider '{}' is invalid: {error}",
                    contributed.name
                ))
            })?;
            merged.providers.push(provider);
        }
        merged
            .validate()
            .map_err(|error| ModelError::Unavailable(error.to_string()))?;

        // Rebuild only when static providers exist; otherwise retain the mature
        // provider cache supplied by the embedding application.
        let providers = if registration.providers.is_empty() {
            Arc::clone(&self.providers)
        } else {
            Arc::new(LlmFactory::new(config::ConfigHandle::from_config(
                merged.clone(),
            )))
        };
        let mut models = Vec::new();
        for provider in &merged.providers {
            for model in &provider.models {
                let profile = model.to_profile(provider).map_err(|error| {
                    ModelError::Unavailable(error.to_string())
                })?;
                let llm = providers
                    .resolve(&profile.provider_id, &profile.model_id)
                    .map_err(|error| {
                        ModelError::Unavailable(error.to_string())
                    })?;
                models.push(Arc::new(ProviderModel::new(
                    profile,
                    llm,
                    merged.retry.provider.clone(),
                )) as Arc<dyn Model>);
            }
        }
        let (active_provider_id, active_model_id) =
            merged.active_model.split_once('/').ok_or_else(|| {
                ModelError::Unavailable(format!(
                    "active model '{}' must use provider/model format",
                    merged.active_model
                ))
            })?;
        ModelCatalog::new(active_provider_id, active_model_id, models)
    }
}

/// Production model adapter over one immutable dynamic provider handle.
pub struct ProviderModel {
    profile: ModelProfile,
    llm: ArcLlm,
    retry: config::ProviderRetryConfig,
}

impl ProviderModel {
    /// Creates a model adapter with immutable profile and request retry policy.
    #[must_use]
    pub fn new(
        profile: ModelProfile,
        llm: ArcLlm,
        retry: config::ProviderRetryConfig,
    ) -> Self {
        Self {
            profile,
            llm,
            retry,
        }
    }

    /// Converts and starts one provider request with request-local hooks attached.
    async fn stream_request(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        hooks: Option<Arc<dyn provider::completion::CompletionRequestHooks>>,
    ) -> Result<ModelStream, ModelError> {
        let provider_name =
            format!("{}/{}", self.profile.provider_id, self.profile.model_id);
        let provider_span = tracing::info_span!("provider_request");
        provider_span.in_scope(|| {
            tracing::info!("started Provider request {}", provider_name);
        });
        let started_at = Instant::now();
        let mut provider_request = ProviderRequest::try_from(request)?.0;
        provider_request.hooks = hooks;
        let options = ProviderRequestOptions::from((
            self.retry.clone(),
            cancellation.clone(),
        ));
        // The Provider span is attached to both acquisition and returned stream
        // polling so detached streaming work keeps the operation Trace.
        let acquisition = tracing::Instrument::instrument(
            options.acquire(&self.llm, provider_request),
            provider_span.clone(),
        )
        .await;
        let mut provider_stream = match acquisition {
            Ok(stream) => stream,
            Err(error) => {
                provider_span.in_scope(|| {
                    tracing::warn!(
                        "failed to start Provider request {} in {} ms",
                        provider_name,
                        started_at.elapsed().as_millis()
                    );
                });
                return Err(error);
            }
        };
        let stream = async_stream::try_stream! {
            let mut received_final = false;
            while let Some(event) = provider_stream.next().await {
                if cancellation.is_cancelled() {
                    tracing::warn!(
                        "cancelled Provider request {} after {} ms",
                        provider_name,
                        started_at.elapsed().as_millis()
                    );
                    Err(ModelError::Cancelled)?;
                }
                if received_final {
                    tracing::warn!(
                        "Provider request {} emitted data after its terminal event",
                        provider_name
                    );
                    Err(ModelError::Protocol(
                        "provider emitted data after Final".to_string(),
                    ))?;
                }
                match event.map_err(|error| {
                    tracing::warn!(
                        "Provider request {} stream failed after {} ms",
                        provider_name,
                        started_at.elapsed().as_millis()
                    );
                    ModelError::Stream(error.model_failure())
                })? {
                    LlmStreamEvent::Text(text) => {
                        yield ModelStreamEvent::TextDelta(text.text);
                    }
                    LlmStreamEvent::ToolCall { tool_call, .. } => {
                        let id = ToolCallId::try_from(tool_call.id)
                            .map_err(|error| ModelError::Protocol(error.to_string()))?;
                        yield ModelStreamEvent::ToolCall(AgentToolCall {
                            tool_call_id: id,
                            name: tool_call.function.name,
                            arguments: tool_call.function.arguments,
                        });
                    }
                    LlmStreamEvent::Reasoning(reasoning) => {
                        let text = reasoning.display_text();
                        if !text.is_empty() {
                            yield ModelStreamEvent::ReasoningDelta(text);
                        }
                    }
                    LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                        yield ModelStreamEvent::ReasoningDelta(reasoning);
                    }
                    LlmStreamEvent::ToolCallDelta { .. } => {}
                    LlmStreamEvent::Final(final_) => {
                        received_final = true;
                        tracing::info!(
                            "completed Provider request {} in {} ms",
                            provider_name,
                            started_at.elapsed().as_millis()
                        );
                        yield ModelStreamEvent::Finished(
                            final_.into_model_final(),
                        );
                    }
                }
            }
            if !received_final {
                tracing::warn!(
                    "Provider request {} ended without a terminal event after {} ms",
                    provider_name,
                    started_at.elapsed().as_millis()
                );
                Err(ModelError::Stream(ModelFailure {
                    summary: "provider stream ended without Final".to_string(),
                    status: None,
                    retry_disposition: ModelRetryDisposition::Retryable,
                }))?;
            }
        };
        Ok(Box::pin(tracing_futures::Instrument::instrument(
            stream,
            provider_span,
        )))
    }
}

#[async_trait]
impl Model for ProviderModel {
    /// Returns the immutable provider/model profile resolved at construction.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Verifies the already-constructed provider handle without inference.
    async fn preflight(&self) -> Result<(), ModelError> {
        self.llm
            .preflight()
            .map_err(|error| ModelError::Preflight(error.model_failure()))
    }

    /// Converts runtime transcript data at the provider boundary and streams it back.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        self.stream_request(request, cancellation, None).await
    }

    /// Attaches extension-composed provider hooks to the concrete request.
    async fn stream_with_hooks(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        hooks: Option<Arc<dyn provider::completion::CompletionRequestHooks>>,
    ) -> Result<ModelStream, ModelError> {
        self.stream_request(request, cancellation, hooks).await
    }
}

/// Converts retained provider terminal data into the shared model protocol.
trait IntoModelFinal {
    /// Consumes provider data and returns its provider-neutral representation.
    fn into_model_final(self) -> ModelFinal;
}

impl IntoModelFinal for ProviderFinal {
    /// Converts a provider terminal result into the shared model domain type.
    fn into_model_final(self) -> ModelFinal {
        let stop_reason = match self.finish_reason {
            ProviderFinishReason::Stop => StopReason::EndTurn,
            ProviderFinishReason::ToolUse => StopReason::ToolUse,
            ProviderFinishReason::Length => StopReason::MaxTokens,
            ProviderFinishReason::Refusal => StopReason::Refusal,
            ProviderFinishReason::Other(_) => StopReason::EndTurn,
        };
        let usage = self.usage.unwrap_or_default();
        ModelFinal {
            stop_reason,
            raw_stop_reason: self.raw_finish_reason,
            usage: ModelUsage::builder()
                .input_tokens(usage.input_tokens)
                .output_tokens(usage.output_tokens)
                .cache_read_tokens(usage.cached_input_tokens)
                .cache_write_tokens(usage.cache_creation_input_tokens)
                .reasoning_tokens(usage.reasoning_tokens)
                .total_tokens(usage.total_tokens)
                .build(),
        }
    }
}

/// Converts retained provider failures into safe shared failure details.
trait CompletionFailure {
    /// Returns provider-neutral failure data without credentials or headers.
    fn model_failure(&self) -> ModelFailure;
}

impl CompletionFailure for CompletionError {
    /// Converts a provider error into safe structured model failure data.
    fn model_failure(&self) -> ModelFailure {
        ModelFailure {
            summary: self.to_string(),
            status: self.status_code(),
            retry_disposition: match self.retry_disposition() {
                RetryDisposition::Retryable { .. } => {
                    ModelRetryDisposition::Retryable
                }
                RetryDisposition::NonRetryable => {
                    ModelRetryDisposition::NonRetryable
                }
            },
        }
    }
}

#[derive(typed_builder::TypedBuilder)]
struct ProviderRequestOptions {
    timeout_ms: Option<u64>,
    max_retries: u32,
    max_retry_delay_ms: u64,
    cancellation: CancellationToken,
}

impl From<(config::ProviderRetryConfig, CancellationToken)>
    for ProviderRequestOptions
{
    /// Builds per-request retry options from immutable config and cancellation.
    fn from(
        (config, cancellation): (
            config::ProviderRetryConfig,
            CancellationToken,
        ),
    ) -> Self {
        Self::builder()
            .timeout_ms(config.timeout_ms)
            .max_retries(config.max_retries.unwrap_or(0))
            .max_retry_delay_ms(config.max_retry_delay_ms)
            .cancellation(cancellation)
            .build()
    }
}

impl ProviderRequestOptions {
    /// Acquires a provider stream with cancellable timeout and request retries.
    async fn acquire(
        &self,
        llm: &ArcLlm,
        request: CompletionRequest,
    ) -> Result<provider::factory::DynLlmStream, ModelError> {
        let mut retry_index = 0_u32;
        loop {
            if self.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let acquisition = tokio::select! {
                () = self.cancellation.cancelled() => {
                    return Err(ModelError::Cancelled);
                }
                result = async {
                    match self.timeout_ms {
                        Some(timeout_ms) if timeout_ms > 0 => {
                            tokio::time::timeout(
                                std::time::Duration::from_millis(timeout_ms),
                                llm.stream(request.clone()),
                            )
                            .await
                            .map_err(|_elapsed| None)
                            .and_then(|result| result.map_err(Some))
                        }
                        _ => llm.stream(request.clone()).await.map_err(Some),
                    }
                } => result,
            };

            let error = match acquisition {
                Ok(stream) => return Ok(stream),
                Err(error) => error,
            };
            let (failure, retry_after_ms) = match error {
                Some(error) => {
                    let retry_after_ms = match error.retry_disposition() {
                        RetryDisposition::Retryable { retry_after_ms } => {
                            retry_after_ms
                        }
                        RetryDisposition::NonRetryable => {
                            return Err(ModelError::Request(
                                error.model_failure(),
                            ));
                        }
                    };
                    (error.model_failure(), retry_after_ms)
                }
                None => (
                    ModelFailure {
                        summary: "provider request timed out".to_string(),
                        status: None,
                        retry_disposition: ModelRetryDisposition::Retryable,
                    },
                    None,
                ),
            };
            if retry_index >= self.max_retries {
                return Err(ModelError::Request(failure));
            }

            let delay_ms = if let Some(delay_ms) = retry_after_ms {
                if self.max_retry_delay_ms > 0
                    && delay_ms > self.max_retry_delay_ms
                {
                    return Err(ModelError::Request(ModelFailure {
                        summary: format!(
                            "provider requested {delay_ms}ms retry delay, exceeding {}ms limit",
                            self.max_retry_delay_ms
                        ),
                        status: failure.status,
                        retry_disposition: ModelRetryDisposition::NonRetryable,
                    }));
                }
                delay_ms
            } else {
                let base_delay = 500_u64
                    .saturating_mul(2_u64.saturating_pow(retry_index))
                    .min(8_000);
                // Integer permille jitter avoids precision-loss casts while
                // preserving the original 75%-100% retry range.
                let jitter_permille = fastrand::u64(750..=1_000);
                base_delay.saturating_mul(jitter_permille) / 1_000
            };
            retry_index += 1;
            match failure.status {
                Some(status) => tracing::warn!(
                    "retrying Provider request after attempt {} in {} ms following HTTP status {}",
                    retry_index,
                    delay_ms,
                    status
                ),
                None => tracing::warn!(
                    "retrying Provider request after attempt {} in {} ms without an HTTP status",
                    retry_index,
                    delay_ms
                ),
            }
            tokio::select! {
                () = self.cancellation.cancelled() => {
                    return Err(ModelError::Cancelled);
                }
                () = tokio::time::sleep(
                    std::time::Duration::from_millis(delay_ms),
                ) => {}
            }
        }
    }
}

struct ProviderRequest(CompletionRequest);

impl TryFrom<ModelRequest> for ProviderRequest {
    type Error = ModelError;

    /// Converts the model-visible runtime transcript and tool schema to provider types.
    fn try_from(request: ModelRequest) -> Result<Self, Self::Error> {
        let messages = request
            .messages
            .into_iter()
            .filter_map(ProviderMessage::try_from_agent)
            .collect::<Result<Vec<_>, _>>()?;
        let chat_history =
            OneOrMany::many(messages.into_iter().map(|message| message.0))
                .map_err(|error| ModelError::Protocol(error.to_string()))?;
        let tools = request
            .tools
            .into_iter()
            .map(|tool| ProviderToolDefinition {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
            })
            .collect();
        Ok(Self(
            CompletionRequest::builder()
                .chat_history(chat_history)
                .tools(tools)
                .build(),
        ))
    }
}

struct ProviderMessage(Message);

impl ProviderMessage {
    /// Converts one runtime message, omitting opaque extension messages without a model mapping.
    fn try_from_agent(
        message: AgentMessage,
    ) -> Option<Result<Self, ModelError>> {
        match message.content {
            MessageContent::System { blocks } => {
                Some(Self::try_from_role(Role::System, blocks))
            }
            MessageContent::User { blocks } => {
                Some(Self::try_from_role(Role::User, blocks))
            }
            MessageContent::Assistant { blocks, .. } => {
                Some(Self::try_from_role(Role::Assistant, blocks))
            }
            MessageContent::ToolResult {
                tool_call_id,
                blocks,
                ..
            } => Some(Self::try_from_tool_result(tool_call_id, blocks)),
            MessageContent::BashExecution { bash }
                if !bash.exclude_from_context =>
            {
                Some(Self::try_from_role(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: bash.model_text(),
                    }],
                ))
            }
            MessageContent::BashExecution { .. } => None,
            MessageContent::Extension { extension }
                if extension.include_in_context =>
            {
                Some(Self::try_from_role(Role::User, extension.blocks))
            }
            MessageContent::Extension { .. } => None,
        }
    }

    /// Converts role content while rejecting blocks unsupported for that role.
    fn try_from_role(
        role: Role,
        blocks: Vec<ContentBlock>,
    ) -> Result<Self, ModelError> {
        match role {
            Role::System => Ok(Self(Message::System {
                content: Self::join_text_blocks(blocks)?,
            })),
            Role::User => {
                let content = blocks
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => Ok(UserContent::text(text)),
                        ContentBlock::Image { data, mime_type } => {
                            let media_type = ImageMediaType::from_mime_type(
                                &mime_type,
                            )
                            .ok_or_else(|| {
                                ModelError::Protocol(format!(
                                    "unsupported user image MIME type: {mime_type}"
                                ))
                            })?;
                            Ok(UserContent::image_base64(
                                data,
                                Some(media_type),
                                None,
                            ))
                        }
                        ContentBlock::Reasoning { .. }
                        | ContentBlock::ToolCall { .. } => Err(ModelError::Protocol(
                            "user messages cannot contain reasoning or tool calls".to_string(),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self(Message::User {
                    content: OneOrMany::many(content).map_err(|error| {
                        ModelError::Protocol(error.to_string())
                    })?,
                }))
            }
            Role::Assistant => {
                let content = blocks
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => {
                            Ok(AssistantContent::text(text))
                        }
                        ContentBlock::Reasoning { text } => {
                            Ok(AssistantContent::Reasoning(
                                provider::message::Reasoning {
                                    id: None,
                                    content: vec![ReasoningContent::Text {
                                        text,
                                        signature: None,
                                    }],
                                },
                            ))
                        }
                        ContentBlock::Image { data, mime_type } => {
                            let media_type = ImageMediaType::from_mime_type(
                                &mime_type,
                            )
                            .ok_or_else(|| {
                                ModelError::Protocol(format!(
                                    "unsupported assistant image MIME type: {mime_type}"
                                ))
                            })?;
                            Ok(AssistantContent::image_base64(
                                data,
                                Some(media_type),
                                None,
                            ))
                        }
                        ContentBlock::ToolCall {
                            tool_call_id,
                            name,
                            arguments,
                        } => Ok(AssistantContent::ToolCall(
                            provider::message::ToolCall::new(
                                tool_call_id.to_string(),
                                provider::message::ToolFunction::new(
                                    name, arguments,
                                ),
                            ),
                        )),
                    })
                    .collect::<Result<Vec<_>, ModelError>>()?;
                Ok(Self(Message::Assistant {
                    id: None,
                    content: OneOrMany::many(content).map_err(|error| {
                        ModelError::Protocol(error.to_string())
                    })?,
                }))
            }
        }
    }

    /// Converts one tool result into provider user-content correlation.
    fn try_from_tool_result(
        tool_call_id: ToolCallId,
        blocks: Vec<ContentBlock>,
    ) -> Result<Self, ModelError> {
        let content = blocks
            .into_iter()
            .map(|block| match block {
                ContentBlock::Text { text } => {
                    Ok(ToolResultContent::text(text))
                }
                ContentBlock::Image { data, mime_type } => {
                    let media_type = ImageMediaType::from_mime_type(&mime_type)
                        .ok_or_else(|| {
                            ModelError::Protocol(format!(
                                "unsupported tool-result image MIME type: {mime_type}"
                            ))
                        })?;
                    Ok(ToolResultContent::image_base64(
                        data,
                        Some(media_type),
                        None,
                    ))
                }
                ContentBlock::Reasoning { .. }
                | ContentBlock::ToolCall { .. } => Err(ModelError::Protocol(
                    "tool results currently support text and image blocks only"
                        .to_string(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(Message::User {
            content: OneOrMany::one(UserContent::ToolResult(
                ProviderToolResult {
                    id: tool_call_id.to_string(),
                    call_id: None,
                    content: OneOrMany::many(content).map_err(|error| {
                        ModelError::Protocol(error.to_string())
                    })?,
                },
            )),
        }))
    }

    /// Joins system text and reasoning blocks without inventing tool-call syntax.
    fn join_text_blocks(
        blocks: Vec<ContentBlock>,
    ) -> Result<String, ModelError> {
        blocks
            .into_iter()
            .map(|block| match block {
                ContentBlock::Text { text }
                | ContentBlock::Reasoning { text } => Ok(text),
                ContentBlock::Image { .. } | ContentBlock::ToolCall { .. } => {
                    Err(ModelError::Protocol(
                        "system messages cannot contain images or tool calls"
                            .to_string(),
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.join("\n"))
    }
}

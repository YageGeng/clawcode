mod content;

use async_trait::async_trait;
use futures::StreamExt;
use protocol::{
    AgentMessage, ContentBlock, MessageContent, ModelFailure, ModelFinal,
    ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage, Role, StopReason,
    ToolCall as AgentToolCall,
};
use provider::{
    ConfiguredModel, ProviderCompletion, ProviderConnectError, ProviderFactory,
};
use rig_core::{
    completion::{CompletionRequest, ToolDefinition},
    http_client::DynHttpClient,
    message::{
        AssistantContent, AssistantMessage, CallId, ImageMediaType, Message,
        MimeType, Reasoning, ToolFunction, ToolName, ToolResult,
        ToolResultContent, UserContent,
    },
    streaming::{Item, StreamEvent},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{Model, ModelCatalog, ModelError, ModelFactory, ModelStream};
pub(crate) use content::AssistantProjection;
use content::ProviderContentProjection;

/// Frozen contributed definitions share transports and authentication without sharing history.
struct ContributedDefinitions {
    registration: Vec<protocol::ExtensionProviderRegistration>,
    providers: Arc<ProviderFactory>,
}

/// Resolves the configured active model through the retained provider factory.
pub struct ProviderModelFactory {
    config: config::ConfigHandle,
    providers: Arc<ProviderFactory>,
    contributed: std::sync::Mutex<Option<ContributedDefinitions>>,
}

impl ProviderModelFactory {
    /// Creates an adapter factory over an already initialized provider cache.
    #[must_use]
    pub fn new(
        config: config::ConfigHandle,
        providers: Arc<ProviderFactory>,
    ) -> Self {
        Self {
            config,
            providers,
            contributed: std::sync::Mutex::new(None),
        }
    }

    /// Constructs the retained provider cache from the immutable TOML config.
    #[must_use]
    pub fn from_config(config: config::ConfigHandle) -> Self {
        let providers = Arc::new(ProviderFactory::new(config.clone()));
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
        Ok(Arc::new(ProviderModel::new(llm)))
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

        // Configuration/authentication are shared, but every catalog below owns fresh session models.
        let providers = if registration.providers.is_empty() {
            Arc::clone(&self.providers)
        } else {
            let mut retained = self.contributed.lock().map_err(|_error| {
                ModelError::Unavailable(
                    "provider definition cache poisoned".into(),
                )
            })?;
            if let Some(cached) = retained
                .as_ref()
                .filter(|cached| cached.registration == registration.providers)
            {
                Arc::clone(&cached.providers)
            } else {
                let providers = Arc::new(ProviderFactory::new(
                    config::ConfigHandle::from_config(merged.clone()),
                ));
                *retained = Some(ContributedDefinitions {
                    registration: registration.providers.clone(),
                    providers: Arc::clone(&providers),
                });
                providers
            }
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
                models
                    .push(Arc::new(ProviderModel::new(llm)) as Arc<dyn Model>);
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

/// One session/model binding owns its upstream Prepared state and current credential.
pub struct ProviderModel {
    configured: Arc<ConfiguredModel>,
    cached: Mutex<Option<CachedCompletion>>,
}

/// The credential under which one concrete session model was prepared.
struct CachedCompletion {
    auth: Option<rig_core::providers::chatgpt::auth::AuthContext>,
    model: Arc<ProviderCompletion>,
}

impl ProviderModel {
    /// Starts an independent session binding over the shared provider definition.
    pub fn new(configured: Arc<ConfiguredModel>) -> Self {
        Self {
            configured,
            cached: Mutex::new(None),
        }
    }

    /// Resolves credentials before taking the short session-cache lock.
    async fn connect(
        &self,
        http: DynHttpClient,
    ) -> Result<rig_core::DynModel<rig_core::operation::Completion>, ModelError>
    {
        let auth = self.configured.auth_context().await.map_err(|error| {
            self.connection_failure("authentication", error)
        })?;
        let model = {
            let mut cache = self.cached.lock().map_err(|_error| {
                tracing::warn!("Provider {}/{} connection failed: session model cache poisoned", self.profile().provider_id, self.profile().model_id);
                ModelError::Unavailable("session model cache poisoned".into())
            })?;
            if let Some(cached) =
                cache.as_ref().filter(|cached| cached.auth == auth)
            {
                Arc::clone(&cached.model)
            } else {
                let model = Arc::new(
                    self.configured.build_model(auth.as_ref()).map_err(
                        |error| {
                            self.connection_failure("model construction", error)
                        },
                    )?,
                );
                *cache = Some(CachedCompletion {
                    auth,
                    model: Arc::clone(&model),
                });
                model
            }
        };
        Ok(model.as_ref().clone().with_transport(http))
    }

    /// Logs safe connection categories with the model and failing phase before returning them.
    fn connection_failure(
        &self,
        phase: &str,
        error: ProviderConnectError,
    ) -> ModelError {
        let summary = format!(
            "provider {}/{} {phase} failed: {error}",
            self.profile().provider_id,
            self.profile().model_id
        );
        tracing::warn!("{}", summary);
        ModelError::Unavailable(summary)
    }

    /// Opens a lazy upstream stream; cancellation also covers authentication and idle reads.
    async fn stream_request(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        middleware: Option<extension::ProviderMiddleware>,
    ) -> Result<ModelStream, ModelError> {
        let request = self
            .configured
            .prepare(ProviderRequest::try_from(request)?.0);
        let base = self.configured.http_client();
        let http = match middleware {
            Some(middleware) => middleware.apply(base),
            None => base,
        };
        let model = tokio::select! {
            () = cancellation.cancelled() => return Err(ModelError::Cancelled),
            model = self.connect(http) => model?,
        };
        let identity = format!(
            "{}/{}",
            self.profile().provider_id,
            self.profile().model_id
        );
        let mut upstream = model.stream(request).map_err(|error| {
            let failure = ModelFailure::from(error);
            tracing::warn!(
                "Provider request {} preparation failed: {}",
                identity,
                failure.summary
            );
            ModelError::Request(failure)
        })?;
        let span = tracing::info_span!("provider_request");
        let stream = async_stream::try_stream! {
            let started = Instant::now();
            tracing::info!("started Provider request {}", identity);
            loop {
                let event = tokio::select! {
                    () = cancellation.cancelled() => {
                        tracing::info!("cancelled Provider request {}", identity);
                        Err(ModelError::Cancelled)
                    }
                    event = upstream.next() => Ok(event),
                }?;
                let Some(event) = event else { break; };
                let event = event.map_err(|error| {
                    let failure = ModelFailure::from(error);
                    tracing::warn!("Provider request {} stream failed: {}", identity, failure.summary);
                    ModelError::Stream(failure)
                })?;
                match event {
                    Item::Event(StreamEvent::Text { text, .. }) => yield ModelStreamEvent::TextDelta(text),
                    Item::Event(StreamEvent::Reasoning { text, .. }) => yield ModelStreamEvent::ReasoningDelta(text),
                    Item::Event(StreamEvent::End { content: AssistantContent::ToolCall(call), .. }) => {
                        yield ModelStreamEvent::ToolCall(AgentToolCall::try_from(&call)?);
                    }
                    _ => {}
                }
            }
            // EOF alone cannot prove success; finish checks the provider's terminal frame.
            let response = upstream.finish().await.map_err(|error| {
                let failure = ModelFailure::from(error);
                tracing::warn!("Provider request {} finalization failed: {}", identity, failure.summary);
                ModelError::Stream(failure)
            })?;
            if let Some(failure) = ModelFailure::from_response(&response) {
                tracing::warn!("Provider request {} terminal response failed: {}", identity, failure.summary);
                Err(ModelError::Stream(failure))?;
            }
            let native = match response.message() {
                Some(Message::Assistant(message)) => message,
                _ => Err(ModelError::Protocol("provider returned no assistant message".into()))?,
            };
            let stop_reason = AssistantProjection::from(&native).stop_reason();
            let raw_stop_reason = response.finish_reason().map(|reason| match reason {
                rig_core::completion::FinishReason::Stop => "stop".to_owned(),
                rig_core::completion::FinishReason::Length => "length".to_owned(),
                rig_core::completion::FinishReason::ToolCalls => "tool_calls".to_owned(),
                rig_core::completion::FinishReason::ContentFilter => "content_filter".to_owned(),
                rig_core::completion::FinishReason::Other(reason) => reason,
                // Preserve future upstream variants in stored diagnostics, not ordinary logs.
                reason => format!("{reason:?}"),
            });
            tracing::info!("completed Provider request {} in {} ms", identity, started.elapsed().as_millis());
            yield ModelStreamEvent::Finished(ModelFinal::builder().stop_reason(stop_reason)
                .raw_stop_reason(raw_stop_reason).usage(ModelUsage::from(response.usage))
                .model_message(Some(Box::new(native))).build());
        };
        Ok(Box::pin(tracing_futures::Instrument::instrument(
            stream, span,
        )))
    }
}

#[async_trait]
impl Model for ProviderModel {
    /// Returns shared immutable configuration without duplicating the profile.
    fn profile(&self) -> &ModelProfile {
        self.configured.profile()
    }

    /// Readiness checks never start a paid request or an interactive login.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Streams without extension middleware for non-extension callers.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        self.stream_request(request, cancellation, None).await
    }

    /// Attaches request-local native middleware while retaining the session model cache.
    async fn stream_with_middleware(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        middleware: Option<extension::ProviderMiddleware>,
    ) -> Result<ModelStream, ModelError> {
        self.stream_request(request, cancellation, middleware).await
    }
}

/// Request conversion owns the transient call-correlation index for exactly one history.
struct ProviderRequest(CompletionRequest);

impl TryFrom<ModelRequest> for ProviderRequest {
    type Error = ModelError;

    /// Converts active history in order and retains the final message as the prompt.
    fn try_from(request: ModelRequest) -> Result<Self, Self::Error> {
        let mut projection = ProviderMessage {
            calls: HashMap::new(),
        };
        let mut messages = Vec::new();
        for message in request.messages {
            if let Some(message) = projection.convert(message)? {
                messages.push(message);
            }
        }
        let prompt = messages.pop().ok_or_else(|| {
            ModelError::Protocol("model history is empty".into())
        })?;
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                Ok(ToolDefinition::new(
                    ToolName::try_from(tool.name).map_err(|error| {
                        ModelError::Protocol(error.to_string())
                    })?,
                    tool.description,
                    tool.parameters,
                ))
            })
            .collect::<Result<Vec<_>, ModelError>>()?;
        Ok(Self(
            CompletionRequest::new(prompt)
                .messages(messages)
                .tools(tools)
                .temperature(request.options.temperature)
                .max_tokens(request.options.max_tokens),
        ))
    }
}

/// Correlates results with canonical or native call identities as history is traversed.
struct ProviderMessage {
    calls: HashMap<String, (CallId, ToolName)>,
}

impl ProviderMessage {
    /// Projects one project message without inventing provider provenance or tool names.
    fn convert(
        &mut self,
        message: AgentMessage,
    ) -> Result<Option<Message>, ModelError> {
        let message = match message.content {
            MessageContent::System { blocks } => {
                Self::role(Role::System, blocks)?
            }
            MessageContent::User { blocks } => Self::role(Role::User, blocks)?,
            MessageContent::ExpandedUser { expansion } => {
                Self::role(Role::User, expansion.model_blocks)?
            }
            MessageContent::Assistant {
                blocks,
                metadata,
                model_message,
                ..
            } => {
                let retained = match model_message {
                    Some(native)
                        if AssistantProjection::from(native.as_ref())
                            .matches(&blocks, metadata.stop_reason)? =>
                    {
                        Some(native)
                    }
                    _ => None,
                };
                if let Some(native) = retained {
                    Message::Assistant(*native)
                } else {
                    let Message::Assistant(mut assistant) =
                        Self::role(Role::Assistant, blocks)?
                    else {
                        return Err(ModelError::Protocol(
                            "assistant role was not preserved".into(),
                        ));
                    };
                    assistant.stop = Some(match metadata.stop_reason {
                        StopReason::EndTurn => {
                            rig_core::message::StopReason::Stop
                        }
                        StopReason::ToolUse => {
                            rig_core::message::StopReason::ToolUse
                        }
                        StopReason::MaxTokens => {
                            rig_core::message::StopReason::Length
                        }
                        StopReason::Cancelled => {
                            rig_core::message::StopReason::Aborted(
                                "cancelled".into(),
                            )
                        }
                        _ => rig_core::message::StopReason::Error(
                            "failed attempt".into(),
                        ),
                    });
                    Message::Assistant(assistant)
                }
            }
            MessageContent::ToolResult {
                tool_call_id,
                blocks,
                is_error,
                ..
            } => {
                // A result with no retained call is orphaned in this stateless history.
                let Some((call, name)) =
                    self.calls.get(tool_call_id.as_str()).cloned()
                else {
                    return Ok(None);
                };
                let mut content = blocks.into_iter().map(|block| match block {
                    ContentBlock::Text { text } => Ok(ToolResultContent::text(text)),
                    ContentBlock::Image { data, mime_type } => Ok(ToolResultContent::image_base64(data, Some(Self::image_type(&mime_type)?), None)),
                    ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => Err(ModelError::Protocol("tool results cannot contain reasoning or tool calls".into())),
                    block => Ok(ToolResultContent::text(block.into_provider_text()?)),
                }).collect::<Result<Vec<_>,ModelError>>()?;
                if content.is_empty() {
                    content.push(ToolResultContent::text(if is_error {
                        "Tool failed without returning content."
                    } else {
                        ""
                    }));
                }
                Message::User {
                    content: vec![UserContent::ToolResult(ToolResult {
                        call,
                        name,
                        content,
                        is_error,
                    })],
                }
            }
            MessageContent::BashExecution { bash }
                if !bash.exclude_from_context =>
            {
                Message::from(bash.model_text())
            }
            MessageContent::Extension { extension }
                if extension.include_in_context =>
            {
                Self::role(Role::User, extension.blocks)?
            }
            MessageContent::CompactionSummary { compaction } => {
                Message::from(compaction.model_text())
            }
            MessageContent::BashExecution { .. }
            | MessageContent::Extension { .. }
            | MessageContent::SlashCommand { .. } => return Ok(None),
        };
        if let Message::Assistant(assistant) = &message {
            for call in assistant.tool_calls() {
                self.calls.insert(
                    call.id.wire().into_owned(),
                    (call.id.clone(), call.function.name.clone()),
                );
            }
        }
        Ok(Some(message))
    }

    /// Validates media types once at the boundary shared by every role.
    fn image_type(mime: &str) -> Result<ImageMediaType, ModelError> {
        ImageMediaType::from_mime_type(mime).ok_or_else(|| {
            ModelError::Protocol(format!("unsupported image MIME type: {mime}"))
        })
    }

    /// Converts canonical role content when no unchanged native message is available.
    fn role(
        role: Role,
        blocks: Vec<ContentBlock>,
    ) -> Result<Message, ModelError> {
        match role {
            Role::System => Ok(Message::system(blocks.into_iter().map(|block| match block {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text } => Ok(text),
                ContentBlock::Image { .. } | ContentBlock::ToolCall { .. } => Err(ModelError::Protocol("system messages cannot contain images or calls".into())),
                block => block.into_provider_text(),
            }).collect::<Result<Vec<_>,_>>()?.join("\n"))),
            Role::User => Ok(Message::User { content: blocks.into_iter().map(|block| match block {
                ContentBlock::Text { text } => Ok(UserContent::text(text)),
                ContentBlock::Image { data, mime_type } => Ok(UserContent::image_base64(data, Some(Self::image_type(&mime_type)?), None)),
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => Err(ModelError::Protocol("user messages cannot contain reasoning or calls".into())),
                block => Ok(UserContent::text(block.into_provider_text()?)),
            }).collect::<Result<Vec<_>,ModelError>>()? }),
            Role::Assistant => Ok(Message::Assistant(AssistantMessage::new(blocks.into_iter().map(|block| match block {
                ContentBlock::Text { text } => Ok(AssistantContent::text(text)),
                ContentBlock::Reasoning { text } => Ok(AssistantContent::Reasoning(Reasoning::new(text))),
                ContentBlock::Image { data, mime_type } => Ok(AssistantContent::image_base64(data, Some(Self::image_type(&mime_type)?), None)),
                ContentBlock::ToolCall { tool_call_id, name, arguments, invalid_arguments , .. } => {
                    let name = ToolName::try_from(name).map_err(|error| ModelError::Protocol(error.to_string()))?;
                    let arguments = invalid_arguments.map_or(arguments, Value::String);
                    Ok(AssistantContent::ToolCall(rig_core::message::ToolCall::new(CallId::from_wire(tool_call_id.to_string()), ToolFunction::new(name, arguments))))
                }
                block => Ok(AssistantContent::text(block.into_provider_text()?)),
            }).collect::<Result<Vec<_>,ModelError>>()?))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeated requests retain only their session's concrete upstream model.
    #[tokio::test]
    async fn session_caches_share_definitions_but_not_prepared_owners() {
        let config: config::AppConfig = toml::from_str(
            r#"
active_model="openai/audit"
[[providers]]
id="openai"
display_name="audit"
provider_type="responses"
base_url="https://provider.invalid"
api_key="synthetic-key"
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
"#,
        )
        .unwrap();
        let factory =
            ProviderFactory::new(config::ConfigHandle::from_config(config));
        let definition = factory.resolve("openai", "audit").unwrap();
        let first = ProviderModel::new(Arc::clone(&definition));
        let second = ProviderModel::new(Arc::clone(&definition));
        drop(first.connect(definition.http_client()).await.unwrap());
        let first_cached =
            Arc::clone(&first.cached.lock().unwrap().as_ref().unwrap().model);
        drop(second.connect(definition.http_client()).await.unwrap());
        let second_cached =
            Arc::clone(&second.cached.lock().unwrap().as_ref().unwrap().model);
        drop(first.connect(definition.http_client()).await.unwrap());
        assert!(Arc::ptr_eq(
            &first_cached,
            &first.cached.lock().unwrap().as_ref().unwrap().model
        ));
        assert!(!Arc::ptr_eq(&first_cached, &second_cached));
        let weak = Arc::downgrade(&first_cached);
        drop(first_cached);
        drop(first);
        assert!(weak.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &second_cached,
            &second.cached.lock().unwrap().as_ref().unwrap().model
        ));
    }
    /// A credential refresh invalidates only the requesting session's prepared model.
    #[tokio::test]
    async fn changed_credentials_replace_only_the_local_cache() {
        use rig_core::providers::chatgpt::auth::{FileTokenStore, TokenStore};
        let directory = tempfile::tempdir().expect("isolated credentials");
        FileTokenStore::new(directory.path()).save("chatgpt",&serde_json::json!({
            "access_token":"fresh-synthetic","account_id":"account","expires_at":i64::MAX
        }).to_string()).await.expect("seed upstream credential");
        let config: config::AppConfig = toml::from_str(&format!(
            r#"
active_model="chatgpt/audit"
[[providers]]
id="chatgpt"
display_name="audit"
provider_type="responses"
base_url="https://provider.invalid"
auth={{type="chatgpt",token_store_dir={path:?}}}
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
"#,
            path = directory.path().to_str().expect("store path")
        ))
        .expect("config");
        let factory =
            ProviderFactory::new(config::ConfigHandle::from_config(config));
        let definition =
            factory.resolve("chatgpt", "audit").expect("definition");
        let first = ProviderModel::new(Arc::clone(&definition));
        let second = ProviderModel::new(Arc::clone(&definition));
        drop(
            first
                .connect(definition.http_client())
                .await
                .expect("first model"),
        );
        drop(
            second
                .connect(definition.http_client())
                .await
                .expect("second model"),
        );
        let first_model = Arc::clone(
            &first
                .cached
                .lock()
                .expect("cache")
                .as_ref()
                .expect("cached model")
                .model,
        );
        let second_model = Arc::clone(
            &second
                .cached
                .lock()
                .expect("cache")
                .as_ref()
                .expect("cached model")
                .model,
        );
        // Simulate the old credential on an existing binding after shared auth has refreshed.
        first
            .cached
            .lock()
            .expect("cache")
            .as_mut()
            .expect("cached model")
            .auth
            .as_mut()
            .expect("OAuth context")
            .api_key = "expired-synthetic".into();
        drop(
            first
                .connect(definition.http_client())
                .await
                .expect("refreshed model"),
        );
        assert!(!Arc::ptr_eq(
            &first_model,
            &first
                .cached
                .lock()
                .expect("cache")
                .as_ref()
                .expect("cached model")
                .model
        ));
        assert!(Arc::ptr_eq(
            &second_model,
            &second
                .cached
                .lock()
                .expect("cache")
                .as_ref()
                .expect("cached model")
                .model
        ));
    }
}

//! DeepSeek API client and Rig integration
//!
//! # Example
//! ```no_run
//! use provider::{client::CompletionClient, providers::deepseek};
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = deepseek::Client::new("DEEPSEEK_API_KEY")?;
//!
//! let deepseek_chat = client.completion_model(deepseek::DEEPSEEK_V4_FLASH);
//! # Ok(())
//! # }
//! ```

use bytes::Bytes;
use http::Request;
use tracing::Instrument;

use crate::client::{
    self, BearerAuth, Capabilities, Capable, DebugExt, ModelLister, Provider,
    ProviderBuilder, ProviderClient,
};
use crate::completion::{GetFinishReason, GetTokenUsage, ProviderFinishReason};
use crate::http_client::{self, HttpClientExt};
use crate::message::{Document, DocumentSourceKind, MimeType, TryIntoMany};
use crate::model::{Model, ModelList, ModelListingError};
use crate::providers::internal::openai_chat_completions_compatible::{
    self, CompatibleChoiceData, CompatibleChunk, CompatibleFinishReason,
    CompatibleStreamProfile,
};
use crate::{
    OneOrMany,
    completion::{self, CompletionError, CompletionRequest},
    json_utils, message,
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};
use serde::{Deserialize, Serialize};

use super::openai::completion::streaming::StreamingToolCall;

// ================================================================
// Main DeepSeek Client
// ================================================================
const DEEPSEEK_API_BASE_URL: &str = "https://api.deepseek.com";

/// Wire-level tool choice used by DeepSeek's function-list encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, rename_all = "snake_case")]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    Function(Vec<ToolChoiceFunctionKind>),
}

/// Function entry used by the DeepSeek tool-choice wire format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "function")]
pub enum ToolChoiceFunctionKind {
    Function { name: String },
}

impl TryFrom<message::ToolChoice> for ToolChoice {
    type Error = CompletionError;

    fn try_from(value: message::ToolChoice) -> Result<Self, Self::Error> {
        // DeepSeek expects named tool choices to be serialized as a list of function objects.
        let tool_choice = match value {
            message::ToolChoice::None => Self::None,
            message::ToolChoice::Auto => Self::Auto,
            message::ToolChoice::Required => Self::Required,
            message::ToolChoice::Specific { function_names } => Self::Function(
                function_names
                    .into_iter()
                    .map(|name| ToolChoiceFunctionKind::Function { name })
                    .collect(),
            ),
        };

        Ok(tool_choice)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DeepSeekExt;
#[derive(Debug, Default, Clone, Copy)]
pub struct DeepSeekExtBuilder;

type DeepSeekApiKey = BearerAuth;

impl Provider for DeepSeekExt {
    type Builder = DeepSeekExtBuilder;
    const VERIFY_PATH: &'static str = "/user/balance";
}

impl<H> Capabilities<H> for DeepSeekExt {
    type Completion = Capable<CompletionModel<H>>;
    type ModelListing = Capable<DeepSeekModelLister<H>>;
}

impl DebugExt for DeepSeekExt {}

impl ProviderBuilder for DeepSeekExtBuilder {
    type Extension<H>
        = DeepSeekExt
    where
        H: HttpClientExt;
    type ApiKey = DeepSeekApiKey;

    const BASE_URL: &'static str = DEEPSEEK_API_BASE_URL;

    fn build<H>(
        _builder: &client::ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt,
    {
        Ok(DeepSeekExt)
    }
}

pub type Client<H = reqwest::Client> = client::Client<DeepSeekExt, H>;
pub type ClientBuilder<H = crate::markers::Missing> =
    client::ClientBuilder<DeepSeekExtBuilder, String, H>;

impl ProviderClient for Client {
    type Input = DeepSeekApiKey;
    type Error = crate::client::ProviderClientError;

    // If you prefer the environment variable approach:
    fn from_env() -> Result<Self, Self::Error> {
        let api_key = crate::client::required_env_var("DEEPSEEK_API_KEY")?;
        let mut client_builder = Self::builder();
        client_builder.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        let client_builder = client_builder.api_key(&api_key);
        client_builder.build().map_err(Into::into)
    }

    fn from_val(input: Self::Input) -> Result<Self, Self::Error> {
        Self::new(input).map_err(Into::into)
    }
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiResponse<T> {
    Ok(T),
    Err(ApiErrorResponse),
}

impl From<ApiErrorResponse> for CompletionError {
    fn from(err: ApiErrorResponse) -> Self {
        CompletionError::ProviderError(err.message)
    }
}

/// The response shape from the DeepSeek API
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    // We'll match the JSON:
    pub choices: Vec<Choice>,
    pub usage: Usage,
    // you may want other fields
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Usage {
    pub completion_tokens: u32,
    pub prompt_tokens: u32,
    pub prompt_cache_hit_tokens: u32,
    pub prompt_cache_miss_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

impl GetTokenUsage for Usage {
    fn token_usage(&self) -> Option<crate::completion::Usage> {
        Some(crate::providers::internal::completion_usage(
            self.prompt_tokens as u64,
            self.completion_tokens as u64,
            self.total_tokens as u64,
            self.cached_input_tokens(),
        ))
    }
}

impl Usage {
    /// Return DeepSeek prompt cache hits, falling back to OpenAI-compatible details.
    fn cached_input_tokens(&self) -> u64 {
        if self.prompt_cache_hit_tokens > 0 {
            return self.prompt_cache_hit_tokens as u64;
        }

        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .map(u64::from)
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CompletionTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PromptTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub index: usize,
    pub message: Message,
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: String,
}

/// DeepSeek user-message content, preserving the legacy text form when no image exists.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(untagged)]
pub enum UserMessageContent {
    /// Plain text accepted by every DeepSeek chat model.
    Text(String),
    /// OpenAI-compatible content blocks required by DeepSeek vision models.
    Blocks(Vec<UserContent>),
}

/// OpenAI-compatible user content blocks accepted by DeepSeek vision models.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    /// Plain text placed alongside one or more images.
    Text { text: String },
    /// Image passed through an external or inline data URL.
    ImageUrl { image_url: ImageUrl },
}

/// URL payload and optional processing detail for one DeepSeek image block.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ImageUrl {
    /// External HTTP URL or an inline Base64 data URL.
    pub url: String,
    /// Optional image-detail preference supported by the compatible API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<message::ImageDetail>,
}

impl TryFrom<message::UserContent> for UserContent {
    type Error = message::MessageError;

    /// Converts one provider-neutral block into DeepSeek's vision wire format.
    fn try_from(value: message::UserContent) -> Result<Self, Self::Error> {
        match value {
            message::UserContent::Text(message::Text { text }) => {
                Ok(Self::Text { text })
            }
            message::UserContent::Image(message::Image {
                data: DocumentSourceKind::Base64(data),
                media_type,
                detail,
                ..
            }) => {
                let media_type = media_type.ok_or_else(|| {
                    tracing::warn!(
                        "Rejected DeepSeek Base64 image without a media type"
                    );
                    message::MessageError::ConversionError(
                        "DeepSeek Base64 images require a media type".into(),
                    )
                })?;
                if !matches!(
                    media_type,
                    message::ImageMediaType::JPEG
                        | message::ImageMediaType::PNG
                        | message::ImageMediaType::GIF
                        | message::ImageMediaType::WEBP
                ) {
                    tracing::warn!(
                        "Rejected unsupported DeepSeek Base64 image format {}",
                        media_type.to_mime_type()
                    );
                    return Err(message::MessageError::ConversionError(
                        format!(
                            "DeepSeek does not support Base64 images with media type {}",
                            media_type.to_mime_type()
                        ),
                    ));
                }
                Ok(Self::ImageUrl {
                    image_url: ImageUrl {
                        url: format!(
                            "data:{};base64,{}",
                            media_type.to_mime_type(),
                            data
                        ),
                        detail,
                    },
                })
            }
            message::UserContent::Image(message::Image {
                data: DocumentSourceKind::Url(url),
                detail,
                ..
            }) => Ok(Self::ImageUrl {
                image_url: ImageUrl { url, detail },
            }),
            message::UserContent::Image(_) => {
                tracing::warn!("Rejected unsupported DeepSeek image source");
                Err(message::MessageError::ConversionError(
                    "DeepSeek image source is not supported".into(),
                ))
            }
            message::UserContent::Document(Document {
                data:
                    DocumentSourceKind::Base64(text)
                    | DocumentSourceKind::String(text),
                ..
            }) => Ok(Self::Text { text }),
            message::UserContent::Document(_) => {
                tracing::warn!(
                    "Rejected unsupported DeepSeek document source in image content blocks"
                );
                Err(message::MessageError::ConversionError(
                    "DeepSeek document source is not supported in image content blocks"
                        .into(),
                ))
            }
            message::UserContent::ToolResult(_) => {
                tracing::warn!(
                    "Rejected DeepSeek tool result during user content block conversion"
                );
                Err(message::MessageError::ConversionError(
                    "DeepSeek tool results must be converted into tool messages"
                        .into(),
                ))
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: UserMessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(
            default,
            deserialize_with = "json_utils::null_or_vec",
            skip_serializing_if = "Vec::is_empty"
        )]
        tool_calls: Vec<ToolCall>,
        /// only exists on reasoning-capable DeepSeek models at time of addition
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
    },
    #[serde(rename = "tool")]
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: &str) -> Self {
        Message::System {
            content: content.to_owned(),
            name: None,
        }
    }
}

impl From<message::ToolResult> for Message {
    fn from(tool_result: message::ToolResult) -> Self {
        let content = match tool_result.content.first() {
            message::ToolResultContent::Text(text) => text.text,
            message::ToolResultContent::Image(_) => String::from("[Image]"),
        };

        Message::ToolResult {
            tool_call_id: tool_result.id,
            content,
        }
    }
}

impl From<message::ToolCall> for ToolCall {
    fn from(tool_call: message::ToolCall) -> Self {
        Self {
            id: tool_call.id,
            // TODO: update index when we have it
            index: 0,
            r#type: ToolType::Function,
            function: Function {
                name: tool_call.function.name,
                arguments: tool_call.function.arguments,
            },
        }
    }
}

impl TryIntoMany<Message> for message::Message {
    type Error = message::MessageError;

    fn try_into_many(self) -> Result<Vec<Message>, Self::Error> {
        match self {
            message::Message::System { content } => Ok(vec![Message::System {
                content,
                name: None,
            }]),
            message::Message::User { content } => {
                // extract tool results
                let mut messages = vec![];

                let tool_results = content
                    .clone()
                    .into_iter()
                    .filter_map(|content| match content {
                        message::UserContent::ToolResult(tool_result) => {
                            Some(Message::from(tool_result))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();

                messages.extend(tool_results);

                let has_image = content
                    .iter()
                    .any(|item| matches!(item, message::UserContent::Image(_)));

                if has_image {
                    let blocks = content
                        .into_iter()
                        .filter(|item| {
                            !matches!(item, message::UserContent::ToolResult(_))
                        })
                        .map(UserContent::try_from)
                        .collect::<Result<Vec<_>, _>>()?;
                    if !blocks.is_empty() {
                        messages.push(Message::User {
                            content: UserMessageContent::Blocks(blocks),
                            name: None,
                        });
                    }
                    return Ok(messages);
                }

                let text_content: String = content
                    .into_iter()
                    .filter_map(|content| match content {
                        message::UserContent::Text(text) => Some(text.text),
                        message::UserContent::Document(Document {
                            data:
                                DocumentSourceKind::Base64(content)
                                | DocumentSourceKind::String(content),
                            ..
                        }) => Some(content),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if !text_content.is_empty() {
                    messages.push(Message::User {
                        content: UserMessageContent::Text(text_content),
                        name: None,
                    });
                }

                Ok(messages)
            }
            message::Message::Assistant { content, .. } => {
                let mut text_content = String::new();
                let mut reasoning_content = String::new();
                let mut tool_calls = Vec::new();

                for item in content.iter() {
                    match item {
                        message::AssistantContent::Text(text) => {
                            text_content.push_str(text.text());
                        }
                        message::AssistantContent::Reasoning(reasoning) => {
                            reasoning_content
                                .push_str(&reasoning.display_text());
                        }
                        message::AssistantContent::ToolCall(tool_call) => {
                            tool_calls.push(ToolCall::from(tool_call.clone()));
                        }
                        _ => {}
                    }
                }

                let reasoning = if reasoning_content.is_empty() {
                    None
                } else {
                    Some(reasoning_content)
                };

                Ok(vec![Message::Assistant {
                    content: text_content,
                    name: None,
                    tool_calls,
                    reasoning_content: reasoning,
                }])
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ToolCall {
    pub id: String,
    pub index: usize,
    #[serde(default)]
    pub r#type: ToolType,
    pub function: Function,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Function {
    pub name: String,
    #[serde(with = "json_utils::stringified_json")]
    pub arguments: serde_json::Value,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    #[default]
    Function,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: completion::ToolDefinition,
}

impl From<crate::completion::ToolDefinition> for ToolDefinition {
    fn from(tool: crate::completion::ToolDefinition) -> Self {
        Self {
            r#type: "function".into(),
            function: tool,
        }
    }
}

impl TryFrom<CompletionResponse>
    for completion::CompletionResponse<CompletionResponse>
{
    type Error = CompletionError;

    fn try_from(response: CompletionResponse) -> Result<Self, Self::Error> {
        let choice = response.choices.first().ok_or_else(|| {
            CompletionError::ResponseError(
                "Response contained no choices".to_owned(),
            )
        })?;
        let content = match &choice.message {
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
                ..
            } => {
                let mut content = if content.trim().is_empty() {
                    vec![]
                } else {
                    vec![completion::AssistantContent::text(content)]
                };

                content.extend(
                    tool_calls
                        .iter()
                        .map(|call| {
                            completion::AssistantContent::tool_call(
                                &call.id,
                                &call.function.name,
                                call.function.arguments.clone(),
                            )
                        })
                        .collect::<Vec<_>>(),
                );

                if let Some(reasoning_content) = reasoning_content {
                    content.push(completion::AssistantContent::reasoning(
                        reasoning_content,
                    ));
                }

                Ok(content)
            }
            _ => Err(CompletionError::ResponseError(
                "Response did not contain a valid message or tool call".into(),
            )),
        }?;

        let choice = OneOrMany::many(content).map_err(|_e| {
            CompletionError::ResponseError(
                "Response contained no message or tool call (empty)".to_owned(),
            )
        })?;

        let usage = completion::Usage {
            input_tokens: response.usage.prompt_tokens as u64,
            output_tokens: response.usage.completion_tokens as u64,
            total_tokens: response.usage.total_tokens as u64,
            cached_input_tokens: response.usage.cached_input_tokens(),
            cache_creation_input_tokens: 0,
            reasoning_tokens: response
                .usage
                .completion_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens)
                .map(u64::from),
        };

        Ok(completion::CompletionResponse {
            choice,
            usage,
            raw_response: response,
            message_id: None,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct DeepseekCompletionRequest {
    model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

impl TryFrom<(&str, CompletionRequest)> for DeepseekCompletionRequest {
    type Error = CompletionError;

    fn try_from(
        (model, req): (&str, CompletionRequest),
    ) -> Result<Self, Self::Error> {
        if req.output_schema.is_some() {
            tracing::warn!(
                "Structured outputs currently not supported for DeepSeek"
            );
        }
        let model = req.model.clone().unwrap_or_else(|| model.to_string());
        let mut full_history: Vec<Message> = match &req.preamble {
            Some(preamble) => vec![Message::system(preamble)],
            None => vec![],
        };

        let chat_history: Vec<Message> = req
            .chat_history
            .clone()
            .into_iter()
            .map(|message| message.try_into_many())
            .collect::<Result<Vec<Vec<Message>>, _>>()?
            .into_iter()
            .flatten()
            .collect();

        full_history.extend(chat_history);

        let tool_choice = req
            .tool_choice
            .clone()
            .map(ToolChoice::try_from)
            .transpose()?;

        Ok(Self {
            model: model.to_string(),
            messages: full_history,
            temperature: req.temperature,
            tools: req
                .tools
                .clone()
                .into_iter()
                .map(ToolDefinition::from)
                .collect::<Vec<_>>(),
            tool_choice,
            additional_params: req.additional_params,
        })
    }
}

/// The struct implementing the `CompletionModel` trait
#[derive(Clone)]
pub struct CompletionModel<T = reqwest::Client> {
    pub client: Client<T>,
    pub model: String,
}

impl<T> completion::CompletionModel for CompletionModel<T>
where
    T: HttpClientExt + Clone + Default + std::fmt::Debug + Send + 'static,
{
    type Response = CompletionResponse;
    type StreamingResponse = StreamingCompletionResponse;

    type Client = Client<T>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self {
            client: client.clone(),
            model: model.into().to_string(),
        }
    }

    async fn completion(
        &self,
        completion_request: CompletionRequest,
    ) -> Result<
        completion::CompletionResponse<CompletionResponse>,
        crate::completion::CompletionError,
    > {
        let request_hooks = completion_request.hooks.clone();
        let span = if tracing::Span::current().is_disabled() {
            tracing::info_span!(
                target: protocol::ProductIdentity::TRACING_COMPLETIONS_TARGET,
                "chat",
                gen_ai.operation.name = "chat",
                gen_ai.provider.name = "deepseek",
                gen_ai.request.model = self.model,
                gen_ai.system_instructions = tracing::field::Empty,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        span.record("gen_ai.system_instructions", &completion_request.preamble);

        let request = DeepseekCompletionRequest::try_from((
            self.model.as_ref(),
            completion_request,
        ))?;

        tracing::debug!(target: protocol::ProductIdentity::TRACING_COMPLETIONS_TARGET,
            "Sending DeepSeek completion request for model {} with {} messages",
            self.model,
            request.messages.len()
        );

        let (body, prepared_hooks) =
            completion::prepare_json_request(&request, request_hooks).await?;
        let mut req = self
            .client
            .post("/chat/completions")?
            .body(body)
            .map_err(|e| CompletionError::HttpError(e.into()))?;
        if let Some(hooks) = prepared_hooks {
            hooks.attach(&mut req).await?;
        }

        async move {
            let response = self.client.send::<_, Bytes>(req).await?;
            let status = response.status();
            let response_body =
                response.into_body().into_future().await?.to_vec();
            tracing::debug!(target: protocol::ProductIdentity::TRACING_COMPLETIONS_TARGET,
                "Received DeepSeek completion response for model {} with status {}",
                self.model,
                status
            );

            if status.is_success() {
                match serde_json::from_slice::<ApiResponse<CompletionResponse>>(
                    &response_body,
                )? {
                    ApiResponse::Ok(response) => {
                        let span = tracing::Span::current();
                        span.record(
                            "gen_ai.usage.input_tokens",
                            response.usage.prompt_tokens,
                        );
                        span.record(
                            "gen_ai.usage.output_tokens",
                            response.usage.completion_tokens,
                        );
                        span.record(
                            "gen_ai.usage.cache_read.input_tokens",
                            response.usage.cached_input_tokens(),
                        );
                        response.try_into()
                    }
                    ApiResponse::Err(err) => {
                        tracing::warn!(
                            "DeepSeek completion API returned an error: {}",
                            err.message
                        );
                        Err(CompletionError::ProviderError(err.message))
                    }
                }
            } else {
                tracing::warn!(
                    "DeepSeek completion API returned status {} with {} response bytes",
                    status,
                    response_body.len()
                );
                Err(CompletionError::ProviderError(
                    String::from_utf8_lossy(&response_body).to_string(),
                ))
            }
        }
        .instrument(span)
        .await
    }

    async fn stream(
        &self,
        completion_request: CompletionRequest,
    ) -> Result<
        crate::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        CompletionError,
    > {
        let request_hooks = completion_request.hooks.clone();
        let preamble = completion_request.preamble.clone();
        let mut request = DeepseekCompletionRequest::try_from((
            self.model.as_ref(),
            completion_request,
        ))?;

        let params = json_utils::merge(
            request.additional_params.unwrap_or(serde_json::json!({})),
            serde_json::json!({"stream": true, "stream_options": {"include_usage": true} }),
        );

        request.additional_params = Some(params);

        tracing::debug!(target: protocol::ProductIdentity::TRACING_COMPLETIONS_TARGET,
            "Sending DeepSeek streaming completion request for model {} with {} messages",
            self.model,
            request.messages.len()
        );

        let (body, prepared_hooks) =
            completion::prepare_json_request(&request, request_hooks).await?;

        let mut req = self
            .client
            .post("/chat/completions")?
            .body(body)
            .map_err(|e| CompletionError::HttpError(e.into()))?;
        if let Some(hooks) = prepared_hooks {
            hooks.attach(&mut req).await?;
        }

        let span = if tracing::Span::current().is_disabled() {
            tracing::info_span!(
                target: protocol::ProductIdentity::TRACING_COMPLETIONS_TARGET,
                "chat_streaming",
                gen_ai.operation.name = "chat_streaming",
                gen_ai.provider.name = "deepseek",
                gen_ai.request.model = self.model,
                gen_ai.system_instructions = preamble,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        let result = tracing::Instrument::instrument(
            send_compatible_streaming_request(self.client.clone(), req),
            span,
        )
        .await;
        match &result {
            Ok(_) => tracing::debug!(
                "Established DeepSeek streaming completion for model {}",
                self.model
            ),
            Err(error) => tracing::warn!(
                "Failed to establish DeepSeek streaming completion for model {}: {}",
                self.model,
                error
            ),
        }
        result
    }
}

#[derive(Deserialize, Debug)]
pub struct StreamingDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default, deserialize_with = "json_utils::null_or_vec")]
    tool_calls: Vec<StreamingToolCall>,
    reasoning_content: Option<String>,
}

#[derive(Deserialize, Debug)]
struct StreamingChoice {
    delta: StreamingDelta,
    finish_reason: Option<String>,
}

impl StreamingChoice {
    /// Converts a DeepSeek terminal string into the compatible stream reason.
    fn compatible_finish_reason(&self) -> Option<CompatibleFinishReason> {
        self.finish_reason.as_deref().map(|reason| match reason {
            "tool_calls" | "function_call" => CompatibleFinishReason::ToolCalls,
            "stop" => CompatibleFinishReason::Stop,
            "length" => CompatibleFinishReason::Length,
            "content_filter" => CompatibleFinishReason::Refusal,
            reason => CompatibleFinishReason::Other(reason.to_string()),
        })
    }
}

#[derive(Deserialize, Debug)]
struct StreamingCompletionChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<StreamingChoice>,
    usage: Option<Usage>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct StreamingCompletionResponse {
    pub usage: Usage,
    finish_reason: ProviderFinishReason,
    raw_finish_reason: String,
}

impl GetTokenUsage for StreamingCompletionResponse {
    fn token_usage(&self) -> Option<crate::completion::Usage> {
        self.usage.token_usage()
    }
}

impl GetFinishReason for StreamingCompletionResponse {
    /// Returns the normalized DeepSeek terminal reason.
    fn finish_reason(&self) -> ProviderFinishReason {
        self.finish_reason.clone()
    }

    /// Returns the DeepSeek terminal reason string.
    fn raw_finish_reason(&self) -> Option<String> {
        Some(self.raw_finish_reason.clone())
    }
}

#[derive(Clone, Copy)]
struct DeepSeekCompatibleProfile;

impl CompatibleStreamProfile for DeepSeekCompatibleProfile {
    type Usage = Usage;
    type Detail = ();
    type FinalResponse = StreamingCompletionResponse;

    fn normalize_chunk(
        &self,
        data: &str,
    ) -> Result<
        Option<CompatibleChunk<Self::Usage, Self::Detail>>,
        CompletionError,
    > {
        let data = match serde_json::from_str::<StreamingCompletionChunk>(data)
        {
            Ok(data) => data,
            Err(error) => {
                tracing::debug!(
                    "Couldn't parse SSE payload as StreamingCompletionChunk: {:?}",
                    error
                );
                return Ok(None);
            }
        };

        Ok(Some(
            openai_chat_completions_compatible::normalize_first_choice_chunk(
                data.id,
                data.model,
                data.usage,
                &data.choices,
                |choice| CompatibleChoiceData {
                    finish_reason: choice.compatible_finish_reason(),
                    text: choice.delta.content.clone(),
                    reasoning: choice.delta.reasoning_content.clone(),
                    tool_calls:
                        openai_chat_completions_compatible::tool_call_chunks(
                            &choice.delta.tool_calls,
                        ),
                    details: Vec::new(),
                },
            ),
        ))
    }

    fn build_final_response(
        &self,
        usage: Self::Usage,
        finish_reason: CompatibleFinishReason,
    ) -> Self::FinalResponse {
        StreamingCompletionResponse {
            usage,
            finish_reason: finish_reason.provider_finish_reason(),
            raw_finish_reason: finish_reason.raw_finish_reason(),
        }
    }

    fn uses_distinct_tool_call_eviction(&self) -> bool {
        true
    }

    fn emits_complete_single_chunk_tool_calls(&self) -> bool {
        true
    }
}

pub async fn send_compatible_streaming_request<T>(
    http_client: T,
    req: Request<Vec<u8>>,
) -> Result<
    crate::streaming::StreamingCompletionResponse<StreamingCompletionResponse>,
    CompletionError,
>
where
    T: HttpClientExt + Clone + 'static,
{
    openai_chat_completions_compatible::send_compatible_streaming_request(
        http_client,
        req,
        DeepSeekCompatibleProfile,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct ListModelsResponse {
    data: Vec<ListModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ListModelEntry {
    id: String,
    owned_by: String,
}

impl From<ListModelEntry> for Model {
    fn from(value: ListModelEntry) -> Self {
        let mut model = Model::from_id(value.id);
        model.owned_by = Some(value.owned_by);
        model
    }
}

/// [`ModelLister`] implementation for the DeepSeek API (`GET /models`).
#[derive(Clone)]
pub struct DeepSeekModelLister<H = reqwest::Client> {
    client: Client<H>,
}

impl<H> ModelLister<H> for DeepSeekModelLister<H>
where
    H: HttpClientExt + WasmCompatSend + WasmCompatSync + 'static,
{
    type Client = Client<H>;

    fn new(client: Self::Client) -> Self {
        Self { client }
    }

    async fn list_all(&self) -> Result<ModelList, ModelListingError> {
        let path = "/models";
        let req = self.client.get(path)?.body(http_client::NoBody)?;
        let response =
            self.client.send::<_, Vec<u8>>(req).await.map_err(|error| {
                match error {
                    http_client::Error::InvalidStatusCodeWithMessage {
                        status,
                        message,
                        ..
                    } => ModelListingError::api_error_with_context(
                        "DeepSeek",
                        path,
                        status.as_u16(),
                        message.as_bytes(),
                    ),
                    other => ModelListingError::from(other),
                }
            })?;

        if !response.status().is_success() {
            let status_code = response.status().as_u16();
            let body = response.into_body().await?;
            return Err(ModelListingError::api_error_with_context(
                "DeepSeek",
                path,
                status_code,
                &body,
            ));
        }

        let body = response.into_body().await?;
        let api_resp: ListModelsResponse = serde_json::from_slice(&body)
            .map_err(|error| {
                ModelListingError::parse_error_with_context(
                    "DeepSeek", path, &error, &body,
                )
            })?;

        let models = api_resp.data.into_iter().map(Model::from).collect();

        Ok(ModelList::new(models))
    }
}

// ================================================================
// DeepSeek Completion API
// ================================================================
pub const DEEPSEEK_V4_FLASH: &str = "deepseek-v4-flash";
/// Experimental DeepSeek V4 Flash model that accepts vision input.
pub const DEEPSEEK_V4_FLASH_VISION_EXP: &str = "deepseek-v4-flash-vision-exp";
pub const DEEPSEEK_V4_PRO: &str = "deepseek-v4-pro";

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies DeepSeek-native cache-hit tokens are exposed as cached input tokens.
    #[test]
    fn usage_maps_prompt_cache_hit_tokens_to_cached_input_tokens() {
        let usage = Usage {
            completion_tokens: 7,
            prompt_tokens: 20,
            prompt_cache_hit_tokens: 15,
            prompt_cache_miss_tokens: 5,
            total_tokens: 27,
            completion_tokens_details: None,
            prompt_tokens_details: None,
        };

        let mapped = usage.token_usage().expect("token usage");

        assert_eq!(mapped.input_tokens, 20);
        assert_eq!(mapped.output_tokens, 7);
        assert_eq!(mapped.total_tokens, 27);
        assert_eq!(mapped.cached_input_tokens, 15);
    }

    /// Ensures a Base64 image remains in the DeepSeek user-message payload.
    #[test]
    fn user_message_serializes_base64_image_as_content_block() {
        let input = message::Message::User {
            content: OneOrMany::many([
                message::UserContent::text("Describe this image."),
                message::UserContent::image_base64(
                    "Q0xBVw==",
                    Some(message::ImageMediaType::PNG),
                    Some(message::ImageDetail::High),
                ),
            ])
            .expect("non-empty user content"),
        };

        let messages: Vec<Message> =
            input.try_into_many().expect("DeepSeek messages");
        let payload = serde_json::to_value(&messages[0]).expect("JSON payload");

        assert_eq!(
            payload,
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe this image."},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "data:image/png;base64,Q0xBVw==",
                            "detail": "high"
                        }
                    }
                ]
            })
        );
    }

    /// Ensures an external image URL remains in the DeepSeek user-message payload.
    #[test]
    fn user_message_serializes_external_image_url_as_content_block() {
        let input = message::Message::User {
            content: OneOrMany::many([
                message::UserContent::text("Inspect this diagram."),
                message::UserContent::image_url(
                    "https://example.com/diagram.webp",
                    Some(message::ImageMediaType::WEBP),
                    None,
                ),
            ])
            .expect("non-empty user content"),
        };

        let messages: Vec<Message> =
            input.try_into_many().expect("DeepSeek messages");
        let payload = serde_json::to_value(&messages[0]).expect("JSON payload");

        assert_eq!(
            payload,
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "Inspect this diagram."},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/diagram.webp"
                        }
                    }
                ]
            })
        );
    }

    /// Keeps text-only DeepSeek messages in the backward-compatible string form.
    #[test]
    fn user_message_preserves_text_only_content_shape() {
        let input = message::Message::user("Keep this as plain text.");

        let messages: Vec<Message> =
            input.try_into_many().expect("DeepSeek messages");
        let payload = serde_json::to_value(&messages[0]).expect("JSON payload");

        assert_eq!(
            payload,
            serde_json::json!({
                "role": "user",
                "content": "Keep this as plain text."
            })
        );
    }

    /// Rejects image formats that the DeepSeek vision endpoint cannot decode.
    #[test]
    fn user_message_rejects_unsupported_base64_image_format() {
        let input = message::Message::User {
            content: OneOrMany::one(message::UserContent::image_base64(
                "Q0xBVw==",
                Some(message::ImageMediaType::HEIC),
                None,
            )),
        };

        let result: Result<Vec<Message>, message::MessageError> =
            input.try_into_many();

        assert!(matches!(
            result,
            Err(message::MessageError::ConversionError(_))
        ));
    }
}

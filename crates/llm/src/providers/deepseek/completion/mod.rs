// ================================================================
// DeepSeek Completion API
// ================================================================

use crate::completion::{
    ClientSnafu, CompletionRequest as CoreCompletionRequest, ProviderSnafu, ResponseSnafu,
    SerializeSnafu,
};
use crate::http_client::HttpSnafu;
use crate::telemetry::{ProviderResponseExt, SpanCombinator};
use crate::{
    completion::{
        self, CompletionError,
        message::{self, DocumentSourceKind},
    },
    http_client::{self, HttpClientExt},
    json_utils,
    one_or_many::OneOrMany,
    providers::deepseek::client::Client,
};
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, whatever};
use std::fmt;
use streaming::StreamingCompletionResponse;
use tracing::{Instrument, Level, enabled, info_span};

use super::client::ApiResponse;

pub mod streaming;

/// Message types for the DeepSeek Chat Completions API (OpenAI-compatible format).
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: String,
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
        /// Reasoning content from the deepseek-reasoner model.
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

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(default)]
    pub r#type: ToolType,
    pub function: Function,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    #[default]
    Function,
}

/// Function definition for a tool call.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

/// Normalizes a tool name for providers that only allow `[A-Za-z0-9_-]`.
fn sanitize_deepseek_tool_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "tool".to_string()
    } else {
        sanitized
    }
}

impl From<completion::ToolDefinition> for ToolDefinition {
    fn from(tool: completion::ToolDefinition) -> Self {
        Self {
            r#type: "function".into(),
            function: FunctionDefinition {
                // DeepSeek validates function names against `[A-Za-z0-9_-]`, so
                // provider-incompatible aliases like `fs/read_text_file` must be normalized.
                name: sanitize_deepseek_tool_name(&tool.name),
                description: tool.description,
                parameters: tool.parameters,
            },
        }
    }
}

#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
}

impl TryFrom<crate::completion::message::ToolChoice> for ToolChoice {
    type Error = CompletionError;
    fn try_from(value: crate::completion::message::ToolChoice) -> Result<Self, Self::Error> {
        let res = match value {
            crate::completion::message::ToolChoice::Specific { .. } => {
                return Err(ProviderSnafu {
                    msg: "Provider doesn't support only using specific tools",
                }
                .build());
            }
            crate::completion::message::ToolChoice::Auto => Self::Auto,
            crate::completion::message::ToolChoice::None => Self::None,
            crate::completion::message::ToolChoice::Required => Self::Required,
        };

        Ok(res)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Function {
    pub name: String,
    #[serde(with = "json_utils::stringified_json")]
    pub arguments: serde_json::Value,
}

impl TryFrom<message::ToolResult> for Message {
    type Error = message::MessageError;

    fn try_from(value: message::ToolResult) -> Result<Self, Self::Error> {
        let text = value
            .content
            .into_iter()
            .map(|content| match content {
                message::ToolResultContent::Text(message::Text { text }) => Ok(text),
                other => serde_json::to_string(&other).map_err(|_| {
                    message::ConversionSnafu {
                        msg: "Tool result content could not be serialized for DeepSeek conversion",
                    }
                    .build()
                }),
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");

        Ok(Message::ToolResult {
            tool_call_id: value.id,
            content: text,
        })
    }
}

impl TryFrom<message::Message> for Vec<Message> {
    type Error = message::MessageError;

    fn try_from(message: message::Message) -> Result<Self, Self::Error> {
        match message {
            message::Message::System { content } => Ok(vec![Message::system(&content)]),
            message::Message::User { content } => {
                let (tool_results, other_content): (Vec<_>, Vec<_>) = content
                    .into_iter()
                    .partition(|content| matches!(content, message::UserContent::ToolResult(_)));

                if !tool_results.is_empty() {
                    tool_results
                        .into_iter()
                        .map(|content| match content {
                            message::UserContent::ToolResult(tool_result) => tool_result.try_into(),
                            _ => unreachable!(),
                        })
                        .collect::<Result<Vec<_>, _>>()
                } else {
                    let text_content: String = other_content
                        .into_iter()
                        .filter_map(|content| match content {
                            message::UserContent::Text(message::Text { text }) => Some(text),
                            message::UserContent::Document(message::Document {
                                data:
                                    DocumentSourceKind::Base64(content)
                                    | DocumentSourceKind::String(content),
                                ..
                            }) => Some(content),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if text_content.is_empty() {
                        Ok(vec![])
                    } else {
                        Ok(vec![Message::User {
                            content: text_content,
                            name: None,
                        }])
                    }
                }
            }
            message::Message::Assistant { content, .. } => {
                let mut text_content = Vec::new();
                let mut tool_calls = Vec::new();
                let mut reasoning_content = None;

                for content in content {
                    match content {
                        message::AssistantContent::Text(text) => text_content.push(text.text),
                        message::AssistantContent::ToolCall(tool_call) => {
                            tool_calls.push(tool_call)
                        }
                        message::AssistantContent::Reasoning(reasoning) => {
                            reasoning_content = Some(reasoning.display_text());
                        }
                        message::AssistantContent::Image(_) => {
                            panic!(
                                "DeepSeek API doesn't support image content in assistant messages!"
                            );
                        }
                    }
                }

                if text_content.is_empty() && tool_calls.is_empty() && reasoning_content.is_none() {
                    return Ok(vec![]);
                }

                Ok(vec![Message::Assistant {
                    content: text_content.join("\n"),
                    name: None,
                    tool_calls: tool_calls
                        .into_iter()
                        .map(|tool_call| tool_call.into())
                        .collect::<Vec<_>>(),
                    reasoning_content,
                }])
            }
        }
    }
}

impl From<message::ToolCall> for ToolCall {
    fn from(tool_call: message::ToolCall) -> Self {
        Self {
            id: tool_call.id,
            r#type: ToolType::default(),
            function: Function {
                name: tool_call.function.name,
                arguments: tool_call.function.arguments,
            },
        }
    }
}

impl From<ToolCall> for message::ToolCall {
    fn from(tool_call: ToolCall) -> Self {
        Self {
            id: tool_call.id,
            call_id: None,
            function: message::ToolFunction {
                name: tool_call.function.name,
                arguments: tool_call.function.arguments,
            },
            signature: None,
            additional_params: None,
        }
    }
}

/// Response body from the DeepSeek Chat Completions API.
#[derive(Debug, Deserialize, Serialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

impl TryFrom<CompletionResponse> for completion::CompletionResponse<CompletionResponse> {
    type Error = CompletionError;

    fn try_from(response: CompletionResponse) -> Result<Self, Self::Error> {
        let choice = response.choices.first().context(ResponseSnafu {
            msg: "Response contained no choices",
        })?;

        let content = match &choice.message {
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
                ..
            } => {
                let mut result_content = Vec::new();

                if let Some(reasoning) = reasoning_content
                    && !reasoning.is_empty()
                {
                    result_content.push(completion::AssistantContent::reasoning(reasoning));
                }

                if !content.is_empty() {
                    result_content.push(completion::AssistantContent::text(content));
                }

                result_content.extend(
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
                Ok(result_content)
            }
            _ => Err(ResponseSnafu {
                msg: "Response did not contain a valid message or tool call",
            }
            .build()),
        }?;

        let choice = OneOrMany::many(content).map_err(|_| {
            ResponseSnafu {
                msg: "Response contained no message or tool call (empty)",
            }
            .build()
        })?;

        let usage = response
            .usage
            .as_ref()
            .map(|usage| crate::usage::Usage {
                input_tokens: usage.prompt_tokens as u64,
                output_tokens: usage.completion_tokens as u64,
                total_tokens: usage.total_tokens as u64,
                cached_input_tokens: usage.prompt_cache_hit_tokens as u64,
                cache_creation_input_tokens: 0,
            })
            .unwrap_or_default();

        Ok(completion::CompletionResponse {
            choice,
            usage,
            raw_response: response,
            message_id: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub index: usize,
    pub message: Message,
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: String,
}

/// Token usage statistics, extended with DeepSeek-specific prompt cache fields.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    /// Tokens served from the prompt cache (DeepSeek-specific).
    #[serde(default)]
    pub prompt_cache_hit_tokens: usize,
    /// Tokens not found in the prompt cache (DeepSeek-specific).
    #[serde(default)]
    pub prompt_cache_miss_tokens: usize,
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Prompt tokens: {} Completion tokens: {} Total tokens: {}",
            self.prompt_tokens, self.completion_tokens, self.total_tokens
        )
    }
}

impl crate::usage::GetTokenUsage for Usage {
    fn token_usage(&self) -> Option<crate::usage::Usage> {
        let mut usage = crate::usage::Usage::new();
        usage.input_tokens = self.prompt_tokens as u64;
        usage.output_tokens = self.completion_tokens as u64;
        usage.total_tokens = self.total_tokens as u64;
        usage.cached_input_tokens = self.prompt_cache_hit_tokens as u64;

        Some(usage)
    }
}

#[derive(Clone)]
pub struct CompletionModel<T = reqwest::Client> {
    pub(crate) client: Client<T>,
    pub model: String,
    /// reasoning_effort for deepseek-reasoner (e.g. "high", "max").
    reasoning_effort: Option<String>,
    /// Whether to enable or disable thinking mode.
    thinking_enabled: Option<bool>,
}

impl<T> CompletionModel<T>
where
    T: Default + std::fmt::Debug + Clone + 'static,
{
    pub fn new(client: Client<T>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            reasoning_effort: None,
            thinking_enabled: None,
        }
    }

    /// Sets the `reasoning_effort` parameter for deepseek-reasoner (e.g. `"high"`, `"max"`).
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Enables or disables thinking mode via the `thinking` parameter.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.thinking_enabled = Some(enabled);
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(flatten)]
    additional_params: Option<serde_json::Value>,
}

/// Carries DeepSeek-specific parameters alongside the model name for request construction.
pub(crate) struct DeepSeekRequestParams {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: Option<bool>,
}

impl TryFrom<(DeepSeekRequestParams, CoreCompletionRequest)> for CompletionRequest {
    type Error = CompletionError;

    fn try_from(
        (params, req): (DeepSeekRequestParams, CoreCompletionRequest),
    ) -> Result<Self, Self::Error> {
        let mut partial_history = vec![];

        let CoreCompletionRequest {
            model: request_model,
            preamble,
            chat_history,
            tools,
            temperature,
            max_tokens,
            additional_params,
            tool_choice,
            output_schema: _,
            ..
        } = req;

        partial_history.extend(chat_history);

        let mut full_history: Vec<Message> =
            preamble.map_or_else(Vec::new, |preamble| vec![Message::system(&preamble)]);

        full_history.extend(
            partial_history
                .into_iter()
                .map(message::Message::try_into)
                .collect::<Result<Vec<Vec<Message>>, _>>()
                .with_whatever_context(|err| {
                    format!("convert completion request message failed: {err}")
                })?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        );

        if full_history.is_empty() {
            whatever!(
                "DeepSeek Chat Completions request has no provider-compatible messages after conversion"
            );
        }

        let tool_choice = tool_choice.map(ToolChoice::try_from).transpose()?;

        let tools: Vec<ToolDefinition> = tools.into_iter().map(ToolDefinition::from).collect();

        // Merge reasoning_effort and thinking into additional_params.
        let merged_params = {
            let mut extra = serde_json::Map::new();
            if let Some(effort) = params.reasoning_effort {
                extra.insert(
                    "reasoning_effort".to_string(),
                    serde_json::Value::String(effort),
                );
            }
            if let Some(enabled) = params.thinking_enabled {
                let thinking_type = if enabled { "enabled" } else { "disabled" };
                extra.insert(
                    "thinking".to_string(),
                    serde_json::json!({"type": thinking_type}),
                );
            }

            if extra.is_empty() {
                additional_params
            } else {
                let extra_val = serde_json::Value::Object(extra);
                Some(match additional_params {
                    Some(serde_json::Value::Object(mut map)) => {
                        if let serde_json::Value::Object(new_map) = extra_val {
                            map.extend(new_map);
                        }
                        serde_json::Value::Object(map)
                    }
                    Some(other) => other,
                    None => extra_val,
                })
            }
        };

        let res = Self {
            model: request_model.unwrap_or(params.model),
            messages: full_history,
            tools,
            tool_choice,
            temperature,
            max_tokens,
            additional_params: merged_params,
        };

        Ok(res)
    }
}

impl<T> completion::CompletionModel for CompletionModel<T>
where
    T: HttpClientExt + Default + std::fmt::Debug + Clone + Send + Sync + 'static,
{
    type Response = CompletionResponse;
    type StreamingResponse = StreamingCompletionResponse;

    type Client = super::client::Client<T>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::new(client.clone(), model)
    }

    async fn completion(
        &self,
        completion_request: CoreCompletionRequest,
    ) -> Result<completion::CompletionResponse<CompletionResponse>, CompletionError> {
        let span = if tracing::Span::current().is_disabled() {
            info_span!(
                target: "rig::completions",
                "chat",
                gen_ai.operation.name = "chat",
                gen_ai.provider.name = "deepseek",
                gen_ai.request.model = self.model,
                gen_ai.system_instructions = &completion_request.preamble,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cached_tokens = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        let request = CompletionRequest::try_from((
            DeepSeekRequestParams {
                model: self.model.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                thinking_enabled: self.thinking_enabled,
            },
            completion_request,
        ))?;

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "DeepSeek Chat Completions completion request: {}",
                serde_json::to_string_pretty(&request).context(SerializeSnafu{stage:"deepseek-trace-request"})?
            );
        }

        let body = serde_json::to_vec(&request).context(SerializeSnafu {
            stage: "deepseek-trace-request",
        })?;

        let req = self
            .client
            .post("/chat/completions")
            .context(ClientSnafu {
                stage: "deepseek-request-building",
            })?
            .body(body)
            .context(HttpSnafu {
                stage: "deepseek-request-body",
            })
            .context(ClientSnafu {
                stage: "deepseek-request-body",
            })?;

        async move {
            let response = self.client.send(req).await.context(ClientSnafu {
                stage: "deepseek-send",
            })?;

            if response.status().is_success() {
                let text = http_client::text(response).await.context(ClientSnafu {
                    stage: "deepseek-text",
                })?;

                match serde_json::from_str::<ApiResponse<CompletionResponse>>(&text).context(
                    SerializeSnafu {
                        stage: "deserialize-deepseek",
                    },
                )? {
                    ApiResponse::Ok(response) => {
                        let span = tracing::Span::current();
                        span.record_response_metadata(&response);
                        span.record_token_usage(&response.usage);

                        if enabled!(Level::TRACE) {
                            tracing::trace!(
                                target: "rig::completions",
                                "DeepSeek Chat Completions completion response: {}",
                                serde_json::to_string_pretty(&response).context(SerializeSnafu{stage:"deepseek-trace-response"})?
                            );
                        }

                        response.try_into()
                    }
                    ApiResponse::Err(err) => Err(ProviderSnafu { msg: err.message }.build()),
                }
            } else {
                let text = http_client::text(response).await.context(ClientSnafu{stage:"deepseek-text"})?;
                Err(ProviderSnafu { msg: text }.build())
            }
        }
        .instrument(span)
        .await
    }

    async fn stream(
        &self,
        request: CoreCompletionRequest,
    ) -> Result<
        crate::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        CompletionError,
    > {
        Self::stream(self, request).await
    }

    fn completion_request(
        &self,
        prompt: impl Into<message::Message>,
    ) -> completion::CompletionRequestBuilder<Self> {
        completion::CompletionRequestBuilder::new(self.clone(), prompt)
    }
}

impl ProviderResponseExt for CompletionResponse {
    type OutputMessage = Choice;
    type Usage = Usage;

    fn get_response_id(&self) -> Option<String> {
        Some(self.id.to_owned())
    }

    fn get_response_model_name(&self) -> Option<String> {
        Some(self.model.to_owned())
    }

    fn get_output_messages(&self) -> Vec<Self::OutputMessage> {
        self.choices.clone()
    }

    fn get_text_response(&self) -> Option<String> {
        let Message::Assistant { ref content, .. } = self.choices.last()?.message.clone() else {
            return None;
        };

        Some(content.clone())
    }

    fn get_usage(&self) -> Option<Self::Usage> {
        self.usage.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{CompletionRequest, DeepSeekRequestParams};
    use crate::completion::{
        self,
        message::{AssistantContent, Message as CoreMessage},
    };
    use crate::one_or_many::OneOrMany;

    /// Verifies DeepSeek request conversion sanitizes tool names for provider validation.
    #[test]
    fn deepseek_request_sanitizes_function_tool_names() {
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(CoreMessage::from("test".to_string())),
            temperature: None,
            max_tokens: None,
            tools: vec![completion::ToolDefinition {
                name: "fs/read_text_file".to_string(),
                description: "namespaced read alias".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = CompletionRequest::try_from((
            DeepSeekRequestParams {
                model: "deepseek-v4-pro".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
            },
            request,
        ))
        .expect("request conversion should succeed");

        let function_names = completion_request
            .tools
            .iter()
            .map(|tool| tool.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(function_names, vec!["fs_read_text_file"]);
    }

    /// Verifies assistant reasoning preserved in chat history becomes DeepSeek reasoning_content.
    #[test]
    fn deepseek_request_keeps_reasoning_from_assistant_history() {
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(CoreMessage::Assistant {
                id: None,
                content: OneOrMany::many(vec![
                    AssistantContent::reasoning("hidden chain of thought"),
                    AssistantContent::tool_call(
                        "call_1",
                        "fs/read_text_file",
                        serde_json::json!({"path": "cli.log"}),
                    ),
                ])
                .expect("assistant content"),
            }),
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
            additional_params: Some(serde_json::json!({
                "thinking": { "type": "enabled" }
            })),
            output_schema: None,
        };

        let completion_request = CompletionRequest::try_from((
            DeepSeekRequestParams {
                model: "deepseek-v4-pro".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
            },
            request,
        ))
        .expect("request conversion should succeed");

        match &completion_request.messages[0] {
            super::Message::Assistant {
                reasoning_content,
                tool_calls,
                ..
            } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(
                    reasoning_content.as_deref(),
                    Some("hidden chain of thought")
                );
            }
            other => panic!("expected assistant message, got {other:?}"),
        }
    }
}

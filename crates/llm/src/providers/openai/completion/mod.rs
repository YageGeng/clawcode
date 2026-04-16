// ================================================================
// OpenAI Completion API
// ================================================================

use crate::completion::message::MimeType;
use crate::completion::{
    ClientSnafu, CompletionRequest as CoreCompletionRequest, ProviderSnafu, ResponseSnafu,
    SerializeSnafu,
};
use crate::http_client::HttpSnafu;
use crate::telemetry::{ProviderResponseExt, SpanCombinator};
use crate::{
    completion::{
        self, CompletionError,
        message::{self, AudioMediaType, DocumentSourceKind, ImageDetail},
    },
    http_client::{self, HttpClientExt},
    json_utils,
    one_or_many::{OneOrMany, string_or_one_or_many},
    providers::openai::client::CompletionsClient as Client,
};
use serde::{Deserialize, Serialize, Serializer};
use snafu::{OptionExt, ResultExt, whatever};
use std::convert::Infallible;
use std::fmt;
use streaming::StreamingCompletionResponse;
use tracing::{Instrument, Level, enabled, info_span};

use std::str::FromStr;

use super::client::ApiResponse;

pub mod streaming;

/// Serializes user content as a plain string when there's a single text item,
/// otherwise as an array of content parts.
fn serialize_user_content<S>(
    content: &OneOrMany<UserContent>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if content.len() == 1
        && let UserContent::Text { text } = content.first_ref()
    {
        return serializer.serialize_str(text);
    }
    content.serialize(serializer)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    #[serde(alias = "developer")]
    System {
        #[serde(deserialize_with = "string_or_one_or_many")]
        content: OneOrMany<SystemContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        #[serde(
            deserialize_with = "string_or_one_or_many",
            serialize_with = "serialize_user_content"
        )]
        content: OneOrMany<UserContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        #[serde(
            default,
            deserialize_with = "json_utils::string_or_vec",
            skip_serializing_if = "Vec::is_empty",
            serialize_with = "serialize_assistant_content_vec"
        )]
        content: Vec<AssistantContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: Option<AudioAssistant>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(
            default,
            deserialize_with = "json_utils::null_or_vec",
            skip_serializing_if = "Vec::is_empty"
        )]
        tool_calls: Vec<ToolCall>,
    },
    #[serde(rename = "tool")]
    ToolResult {
        tool_call_id: String,
        content: ToolResultContentValue,
    },
}

impl Message {
    pub fn system(content: &str) -> Self {
        Message::System {
            content: OneOrMany::one(content.to_owned().into()),
            name: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct AudioAssistant {
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct SystemContent {
    #[serde(default)]
    pub r#type: SystemContentType,
    pub text: String,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum SystemContentType {
    #[default]
    Text,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AssistantContent {
    Text { text: String },
    Refusal { refusal: String },
}

impl From<AssistantContent> for completion::AssistantContent {
    fn from(value: AssistantContent) -> Self {
        match value {
            AssistantContent::Text { text } => completion::AssistantContent::text(text),
            AssistantContent::Refusal { refusal } => completion::AssistantContent::text(refusal),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum UserContent {
    Text {
        text: String,
    },
    #[serde(rename = "image_url")]
    Image {
        image_url: ImageUrl,
    },
    Audio {
        input_audio: InputAudio,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default)]
    pub detail: ImageDetail,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct InputAudio {
    pub data: String,
    pub format: AudioMediaType,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ToolResultContent {
    #[serde(default)]
    r#type: ToolResultContentType,
    pub text: String,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ToolResultContentType {
    #[default]
    Text,
}

impl FromStr for ToolResultContent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.to_owned().into())
    }
}

impl From<String> for ToolResultContent {
    fn from(s: String) -> Self {
        ToolResultContent {
            r#type: ToolResultContentType::default(),
            text: s,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum ToolResultContentValue {
    Array(Vec<ToolResultContent>),
    String(String),
}

impl ToolResultContentValue {
    pub fn from_string(s: String, use_array_format: bool) -> Self {
        if use_array_format {
            ToolResultContentValue::Array(vec![ToolResultContent::from(s)])
        } else {
            ToolResultContentValue::String(s)
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            ToolResultContentValue::Array(arr) => arr
                .iter()
                .map(|c| c.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            ToolResultContentValue::String(s) => s.clone(),
        }
    }

    pub fn to_array(&self) -> Self {
        match self {
            ToolResultContentValue::Array(_) => self.clone(),
            ToolResultContentValue::String(s) => {
                ToolResultContentValue::Array(vec![ToolResultContent::from(s.clone())])
            }
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

/// Function definition for a tool, with optional strict mode
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

impl From<completion::ToolDefinition> for ToolDefinition {
    fn from(tool: completion::ToolDefinition) -> Self {
        Self {
            r#type: "function".into(),
            function: FunctionDefinition {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
                strict: None,
            },
        }
    }
}

impl ToolDefinition {
    /// Apply strict mode to this tool definition.
    /// This sets `strict: true` and sanitizes the schema to meet OpenAI requirements.
    pub fn with_strict(mut self) -> Self {
        self.function.strict = Some(true);
        super::sanitize_schema(&mut self.function.parameters);
        self
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
                        msg: "Tool result content could not be serialized for OpenAI conversion",
                    }
                    .build()
                }),
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");

        Ok(Message::ToolResult {
            tool_call_id: value.id,
            content: ToolResultContentValue::String(text),
        })
    }
}

impl TryFrom<message::UserContent> for UserContent {
    type Error = message::MessageError;

    fn try_from(value: message::UserContent) -> Result<Self, Self::Error> {
        match value {
            message::UserContent::Text(message::Text { text }) => Ok(UserContent::Text { text }),
            message::UserContent::Image(message::Image {
                data,
                detail,
                media_type,
                ..
            }) => match data {
                DocumentSourceKind::Url(url) => Ok(UserContent::Image {
                    image_url: ImageUrl {
                        url,
                        detail: detail.unwrap_or_default(),
                    },
                }),
                DocumentSourceKind::Base64(data) => {
                    let url = format!(
                        "data:{};base64,{}",
                        media_type.map(|i| i.to_mime_type()).ok_or(
                            message::ConversionSnafu {
                                msg: "OpenAI Image URI must have media type"
                            }
                            .build()
                        )?,
                        data
                    );

                    let detail = detail.ok_or(
                        message::ConversionSnafu {
                            msg: "OpenAI image URI must have image detail",
                        }
                        .build(),
                    )?;

                    Ok(UserContent::Image {
                        image_url: ImageUrl { url, detail },
                    })
                }
                DocumentSourceKind::Raw(_) => Err(message::ConversionSnafu {
                    msg: "Raw files not supported, encode as base64 first",
                }
                .build()),
                DocumentSourceKind::Unknown => Err(message::ConversionSnafu {
                    msg: "Document has no body",
                }
                .build()),
                doc => Err(message::ConversionSnafu {
                    msg: format!("Unsupported document type: {doc:?}"),
                }
                .build()),
            },
            message::UserContent::Document(message::Document { data, .. }) => {
                if let DocumentSourceKind::Base64(text) | DocumentSourceKind::String(text) = data {
                    Ok(UserContent::Text { text })
                } else {
                    Err(message::ConversionSnafu {
                        msg: "Documents must be base64 or a string",
                    }
                    .build())
                }
            }
            message::UserContent::Audio(message::Audio {
                data, media_type, ..
            }) => match data {
                DocumentSourceKind::Base64(data) => Ok(UserContent::Audio {
                    input_audio: InputAudio {
                        data,
                        format: match media_type {
                            Some(media_type) => media_type,
                            None => AudioMediaType::MP3,
                        },
                    },
                }),
                DocumentSourceKind::Url(_) => Err(message::ConversionSnafu {
                    msg: "URLs are not supported for audio",
                }
                .build()),
                DocumentSourceKind::Raw(_) => Err(message::ConversionSnafu {
                    msg: "Raw files are not supported for audio",
                }
                .build()),
                DocumentSourceKind::Unknown => Err(message::ConversionSnafu {
                    msg: "Audio has no body",
                }
                .build()),
                audio => Err(message::ConversionSnafu {
                    msg: format!("Unsupported audio type: {audio:?}"),
                }
                .build()),
            },
            message::UserContent::ToolResult(_) => Err(message::ConversionSnafu {
                msg: "Tool result is in unsupported format",
            }
            .build()),
            message::UserContent::Video(_) => Err(message::ConversionSnafu {
                msg: "Video is in unsupported format",
            }
            .build()),
        }
    }
}

impl TryFrom<OneOrMany<message::UserContent>> for Vec<Message> {
    type Error = message::MessageError;

    fn try_from(value: OneOrMany<message::UserContent>) -> Result<Self, Self::Error> {
        let (tool_results, other_content): (Vec<_>, Vec<_>) = value
            .into_iter()
            .partition(|content| matches!(content, message::UserContent::ToolResult(_)));

        // If there are messages with both tool results and user content, openai will only
        //  handle tool results. It's unlikely that there will be both.
        if !tool_results.is_empty() {
            tool_results
                .into_iter()
                .map(|content| match content {
                    message::UserContent::ToolResult(tool_result) => tool_result.try_into(),
                    _ => unreachable!(),
                })
                .collect::<Result<Vec<_>, _>>()
        } else {
            let other_content: Vec<UserContent> = other_content
                .into_iter()
                .map(|content| content.try_into())
                .collect::<Result<Vec<_>, _>>()?;

            let other_content = OneOrMany::many(other_content)
                .expect("There must be other content here if there were no tool result content");

            Ok(vec![Message::User {
                content: other_content,
                name: None,
            }])
        }
    }
}

impl TryFrom<OneOrMany<message::AssistantContent>> for Vec<Message> {
    type Error = message::MessageError;

    fn try_from(value: OneOrMany<message::AssistantContent>) -> Result<Self, Self::Error> {
        let mut text_content = Vec::new();
        let mut tool_calls = Vec::new();

        for content in value {
            match content {
                message::AssistantContent::Text(text) => text_content.push(text),
                message::AssistantContent::ToolCall(tool_call) => tool_calls.push(tool_call),
                message::AssistantContent::Reasoning(_) => {
                    // OpenAI Chat Completions does not support assistant-history reasoning items.
                    // Silently skip unsupported reasoning content.
                }
                message::AssistantContent::Image(_) => {
                    panic!(
                        "The OpenAI Completions API doesn't support image content in assistant messages!"
                    );
                }
            }
        }

        if text_content.is_empty() && tool_calls.is_empty() {
            return Ok(vec![]);
        }

        Ok(vec![Message::Assistant {
            content: text_content
                .into_iter()
                .map(|content| content.text.into())
                .collect::<Vec<_>>(),
            refusal: None,
            audio: None,
            name: None,
            tool_calls: tool_calls
                .into_iter()
                .map(|tool_call| tool_call.into())
                .collect::<Vec<_>>(),
        }])
    }
}

impl TryFrom<message::Message> for Vec<Message> {
    type Error = message::MessageError;

    fn try_from(message: message::Message) -> Result<Self, Self::Error> {
        match message {
            message::Message::System { content } => Ok(vec![Message::system(&content)]),
            message::Message::User { content } => content.try_into(),
            message::Message::Assistant { content, .. } => content.try_into(),
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

impl TryFrom<Message> for message::Message {
    type Error = message::MessageError;

    fn try_from(message: Message) -> Result<Self, Self::Error> {
        Ok(match message {
            Message::User { content, .. } => message::Message::User {
                content: content.map(|content| content.into()),
            },
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut content = content
                    .into_iter()
                    .map(|content| match content {
                        AssistantContent::Text { text } => message::AssistantContent::text(text),

                        // TODO: Currently, refusals are converted into text, but should be
                        //  investigated for generalization.
                        AssistantContent::Refusal { refusal } => {
                            message::AssistantContent::text(refusal)
                        }
                    })
                    .collect::<Vec<_>>();

                content.extend(
                    tool_calls
                        .into_iter()
                        .map(|tool_call| Ok(message::AssistantContent::ToolCall(tool_call.into())))
                        .collect::<Result<Vec<_>, _>>()?,
                );

                message::Message::Assistant {
                    id: None,
                    content: OneOrMany::many(content).map_err(|_| {
                        message::ConversionSnafu {
                            msg: "Neither `content` nor `tool_calls` was provided to the Message",
                        }
                        .build()
                    })?,
                }
            }

            Message::ToolResult {
                tool_call_id,
                content,
            } => message::Message::User {
                content: OneOrMany::one(message::UserContent::tool_result(
                    tool_call_id,
                    OneOrMany::one(message::ToolResultContent::text(content.as_text())),
                )),
            },

            // System messages should get stripped out when converting messages, this is just a
            // stop gap to avoid obnoxious error handling or panic occurring.
            Message::System { content, .. } => message::Message::User {
                content: content.map(|content| message::UserContent::text(content.text)),
            },
        })
    }
}

impl From<UserContent> for message::UserContent {
    fn from(content: UserContent) -> Self {
        match content {
            UserContent::Text { text } => message::UserContent::text(text),
            UserContent::Image { image_url } => {
                message::UserContent::image_url(image_url.url, None, Some(image_url.detail))
            }
            UserContent::Audio { input_audio } => {
                message::UserContent::audio(input_audio.data, Some(input_audio.format))
            }
        }
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        UserContent::Text { text: s }
    }
}

impl FromStr for UserContent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(UserContent::Text {
            text: s.to_string(),
        })
    }
}

impl From<String> for AssistantContent {
    fn from(s: String) -> Self {
        AssistantContent::Text { text: s }
    }
}

impl FromStr for AssistantContent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(AssistantContent::Text {
            text: s.to_string(),
        })
    }
}
impl From<String> for SystemContent {
    fn from(s: String) -> Self {
        SystemContent {
            r#type: SystemContentType::default(),
            text: s,
        }
    }
}

impl FromStr for SystemContent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SystemContent {
            r#type: SystemContentType::default(),
            text: s.to_string(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub system_fingerprint: Option<String>,
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
                ..
            } => {
                let mut content = content
                    .iter()
                    .filter_map(|c| {
                        let s = match c {
                            AssistantContent::Text { text } => text,
                            AssistantContent::Refusal { refusal } => refusal,
                        };
                        if s.is_empty() {
                            None
                        } else {
                            Some(completion::AssistantContent::text(s))
                        }
                    })
                    .collect::<Vec<_>>();

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
                Ok(content)
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
                output_tokens: (usage.total_tokens - usage.prompt_tokens) as u64,
                total_tokens: usage.total_tokens as u64,
                cached_input_tokens: usage
                    .prompt_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens as u64)
                    .unwrap_or(0),
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

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PromptTokensDetails {
    /// Cached tokens from prompt caching
    #[serde(default)]
    pub cached_tokens: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub total_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

impl Usage {
    pub fn new() -> Self {
        Self {
            prompt_tokens: 0,
            total_tokens: 0,
            prompt_tokens_details: None,
        }
    }
}

impl Default for Usage {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Usage {
            prompt_tokens,
            total_tokens,
            ..
        } = self;
        write!(
            f,
            "Prompt tokens: {prompt_tokens} Total tokens: {total_tokens}"
        )
    }
}

impl crate::usage::GetTokenUsage for Usage {
    fn token_usage(&self) -> Option<crate::usage::Usage> {
        let mut usage = crate::usage::Usage::new();
        usage.input_tokens = self.prompt_tokens as u64;
        usage.output_tokens = (self.total_tokens - self.prompt_tokens) as u64;
        usage.total_tokens = self.total_tokens as u64;
        usage.cached_input_tokens = self
            .prompt_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens as u64)
            .unwrap_or(0);

        Some(usage)
    }
}

#[derive(Clone)]
pub struct CompletionModel<T = reqwest::Client> {
    pub(crate) client: Client<T>,
    pub model: String,
    pub strict_tools: bool,
    pub tool_result_array_content: bool,
}

impl<T> CompletionModel<T>
where
    T: Default + std::fmt::Debug + Clone + 'static,
{
    pub fn new(client: Client<T>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            strict_tools: false,
            tool_result_array_content: false,
        }
    }

    pub fn with_model(client: Client<T>, model: &str) -> Self {
        Self {
            client,
            model: model.into(),
            strict_tools: false,
            tool_result_array_content: false,
        }
    }

    /// Enable strict mode for tool schemas.
    ///
    /// When enabled, tool schemas are automatically sanitized to meet OpenAI's strict mode requirements:
    /// - `additionalProperties: false` is added to all objects
    /// - All properties are marked as required
    /// - `strict: true` is set on each function definition
    ///
    /// This allows OpenAI to guarantee that the model's tool calls will match the schema exactly.
    pub fn with_strict_tools(mut self) -> Self {
        self.strict_tools = true;
        self
    }

    pub fn with_tool_result_array_content(mut self) -> Self {
        self.tool_result_array_content = true;
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

pub struct OpenAIRequestParams {
    pub model: String,
    pub request: CoreCompletionRequest,
    pub strict_tools: bool,
    pub tool_result_array_content: bool,
}

impl TryFrom<OpenAIRequestParams> for CompletionRequest {
    type Error = CompletionError;

    fn try_from(params: OpenAIRequestParams) -> Result<Self, Self::Error> {
        let OpenAIRequestParams {
            model,
            request: req,
            strict_tools,
            tool_result_array_content,
        } = params;

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
            output_schema,
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
                "OpenAI Chat Completions request has no provider-compatible messages after conversion"
            );
        }

        if tool_result_array_content {
            for msg in &mut full_history {
                if let Message::ToolResult { content, .. } = msg {
                    *content = content.to_array();
                }
            }
        }

        let tool_choice = tool_choice.map(ToolChoice::try_from).transpose()?;

        let tools: Vec<ToolDefinition> = tools
            .into_iter()
            .map(|tool| {
                let def = ToolDefinition::from(tool);
                if strict_tools { def.with_strict() } else { def }
            })
            .collect();

        // Map output_schema to OpenAI's response_format and merge into additional_params
        let additional_params = if let Some(schema) = output_schema {
            let name = schema
                .as_object()
                .and_then(|o| o.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("response_schema")
                .to_string();
            let mut schema_value = schema.to_value();
            super::sanitize_schema(&mut schema_value);
            let response_format = serde_json::json!({
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": name,
                        "strict": true,
                        "schema": schema_value
                    }
                }
            });
            Some(match additional_params {
                Some(existing) => json_utils::merge(existing, response_format),
                None => response_format,
            })
        } else {
            additional_params
        };

        let res = Self {
            model: request_model.unwrap_or(model),
            messages: full_history,
            tools,
            tool_choice,
            temperature,
            max_tokens,
            additional_params,
        };

        Ok(res)
    }
}

impl TryFrom<(String, CoreCompletionRequest)> for CompletionRequest {
    type Error = CompletionError;

    fn try_from((model, req): (String, CoreCompletionRequest)) -> Result<Self, Self::Error> {
        CompletionRequest::try_from(OpenAIRequestParams {
            model,
            request: req,
            strict_tools: false,
            tool_result_array_content: false,
        })
    }
}

impl<T> completion::CompletionModel for CompletionModel<T>
where
    T: HttpClientExt + Default + std::fmt::Debug + Clone + Send + Sync + 'static,
{
    type Response = CompletionResponse;
    type StreamingResponse = StreamingCompletionResponse;

    type Client = super::client::CompletionsClient<T>;

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
                gen_ai.provider.name = "openai",
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

        let request = CompletionRequest::try_from(OpenAIRequestParams {
            model: self.model.to_owned(),
            request: completion_request,
            strict_tools: self.strict_tools,
            tool_result_array_content: self.tool_result_array_content,
        })?;

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "OpenAI Chat Completions completion request: {}",
                serde_json::to_string_pretty(&request).context(SerializeSnafu{stage:"openai-trace-request"})?
            );
        }

        let body = serde_json::to_vec(&request).context(SerializeSnafu {
            stage: "openai-trace-request",
        })?;

        let req = self
            .client
            .post("/chat/completions")
            .context(ClientSnafu {
                stage: "openai-request-building",
            })?
            .body(body)
            .context(HttpSnafu {
                stage: "openai-request-body",
            })
            .context(ClientSnafu {
                stage: "openai-request-body",
            })?;

        async move {
            let response = self.client.send(req).await.context(ClientSnafu {
                stage: "openai-send",
            })?;

            if response.status().is_success() {
                let text = http_client::text(response).await.context(ClientSnafu {
                    stage: "openai-text",
                })?;

                match serde_json::from_str::<ApiResponse<CompletionResponse>>(&text).context(
                    SerializeSnafu {
                        stage: "deserialize-openai",
                    },
                )? {
                    ApiResponse::Ok(response) => {
                        let span = tracing::Span::current();
                        span.record_response_metadata(&response);
                        span.record_token_usage(&response.usage);

                        if enabled!(Level::TRACE) {
                            tracing::trace!(
                                target: "rig::completions",
                                "OpenAI Chat Completions completion response: {}",
                                serde_json::to_string_pretty(&response).context(SerializeSnafu{stage:"openai-trace-response"})?
                            );
                        }

                        response.try_into()
                    }
                    ApiResponse::Err(err) => Err(ProviderSnafu { msg: err.message }.build()),
                }
            } else {
                let text = http_client::text(response).await.context(ClientSnafu{stage:"openai-text"})?;
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

fn serialize_assistant_content_vec<S>(
    value: &Vec<AssistantContent>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_empty() {
        serializer.serialize_str("")
    } else {
        value.serialize(serializer)
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
        let Message::User { ref content, .. } = self.choices.last()?.message.clone() else {
            return None;
        };

        let UserContent::Text { text } = content.first() else {
            return None;
        };

        Some(text)
    }

    fn get_usage(&self) -> Option<Self::Usage> {
        self.usage.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_request_uses_request_model_override() {
        let request = crate::completion::CompletionRequest {
            model: Some("gpt-4.1".to_string()),
            preamble: None,
            chat_history: OneOrMany::one("Hello".into()),
            tools: vec![],
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let openai_request = CompletionRequest::try_from(OpenAIRequestParams {
            model: "gpt-4o-mini".to_string(),
            request,
            strict_tools: false,
            tool_result_array_content: false,
        })
        .expect("request conversion should succeed");
        let serialized =
            serde_json::to_value(openai_request).expect("serialization should succeed");

        assert_eq!(serialized["model"], "gpt-4.1");
    }

    #[test]
    fn test_openai_request_uses_default_model_when_override_unset() {
        let request = crate::completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one("Hello".into()),
            tools: vec![],
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let openai_request = CompletionRequest::try_from(OpenAIRequestParams {
            model: "gpt-4o-mini".to_string(),
            request,
            strict_tools: false,
            tool_result_array_content: false,
        })
        .expect("request conversion should succeed");
        let serialized =
            serde_json::to_value(openai_request).expect("serialization should succeed");

        assert_eq!(serialized["model"], "gpt-4o-mini");
    }

    #[test]
    fn tool_result_with_mixed_content_is_stringified_for_completion_api() {
        let tool_result = crate::completion::message::ToolResult {
            id: "tool_call_123".to_string(),
            call_id: None,
            content: crate::one_or_many::OneOrMany::many(vec![
                crate::completion::message::ToolResultContent::text("plain text"),
                crate::completion::message::ToolResultContent::image_url(
                    "https://example.com/logo.png",
                    Some(crate::completion::message::ImageMediaType::PNG),
                    Some(crate::completion::message::ImageDetail::Low),
                ),
            ])
            .expect("tool result content should be non-empty"),
        };

        let openai_message: Message = tool_result.try_into().expect("tool result conversion");
        let Message::ToolResult {
            tool_call_id,
            content,
        } = openai_message
        else {
            panic!("expected OpenAI tool result message")
        };

        assert_eq!(tool_call_id, "tool_call_123");

        let ToolResultContentValue::String(output) = content else {
            panic!("expected string-valued tool result content")
        };

        let expected_output = format!(
            "{}\n{}",
            "plain text",
            serde_json::to_string(&crate::completion::message::ToolResultContent::image_url(
                "https://example.com/logo.png",
                Some(crate::completion::message::ImageMediaType::PNG),
                Some(crate::completion::message::ImageDetail::Low),
            ))
            .expect("image tool result content should serialize")
        );

        assert_eq!(output, expected_output);
    }

    #[test]
    fn assistant_reasoning_is_silently_skipped() {
        let assistant_content = OneOrMany::one(message::AssistantContent::reasoning("hidden"));

        let converted: Vec<Message> = assistant_content
            .try_into()
            .expect("conversion should work");

        assert!(converted.is_empty());
    }

    #[test]
    fn assistant_text_and_tool_call_are_preserved_when_reasoning_is_present() {
        let assistant_content = OneOrMany::many(vec![
            message::AssistantContent::reasoning("hidden"),
            message::AssistantContent::text("visible"),
            message::AssistantContent::tool_call(
                "call_1",
                "subtract",
                serde_json::json!({"x": 2, "y": 1}),
            ),
        ])
        .expect("non-empty assistant content");

        let converted: Vec<Message> = assistant_content
            .try_into()
            .expect("conversion should work");
        assert_eq!(converted.len(), 1);

        match &converted[0] {
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                assert_eq!(
                    content,
                    &vec![AssistantContent::Text {
                        text: "visible".to_string()
                    }]
                );
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].function.name, "subtract");
                assert_eq!(
                    tool_calls[0].function.arguments,
                    serde_json::json!({"x": 2, "y": 1})
                );
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn test_max_tokens_is_forwarded_to_request() {
        let request = crate::completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one("Hello".into()),
            tools: vec![],
            temperature: None,
            max_tokens: Some(4096),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let openai_request = CompletionRequest::try_from(OpenAIRequestParams {
            model: "gpt-4o-mini".to_string(),
            request,
            strict_tools: false,
            tool_result_array_content: false,
        })
        .expect("request conversion should succeed");
        let serialized =
            serde_json::to_value(openai_request).expect("serialization should succeed");

        assert_eq!(serialized["max_tokens"], 4096);
    }

    #[test]
    fn test_max_tokens_omitted_when_none() {
        let request = crate::completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one("Hello".into()),
            tools: vec![],
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let openai_request = CompletionRequest::try_from(OpenAIRequestParams {
            model: "gpt-4o-mini".to_string(),
            request,
            strict_tools: false,
            tool_result_array_content: false,
        })
        .expect("request conversion should succeed");
        let serialized =
            serde_json::to_value(openai_request).expect("serialization should succeed");

        assert!(serialized.get("max_tokens").is_none());
    }

    #[test]
    fn request_conversion_errors_when_all_messages_are_filtered() {
        let request = CoreCompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(message::Message::Assistant {
                id: None,
                content: OneOrMany::one(message::AssistantContent::reasoning("hidden")),
            }),
            tools: vec![],
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let result = CompletionRequest::try_from(OpenAIRequestParams {
            model: "gpt-4o-mini".to_string(),
            request,
            strict_tools: false,
            tool_result_array_content: false,
        });

        println!("{result:?}");
    }
}

//! The OpenAI Responses API.
//!
//! By default when creating a completion client, this is the API that gets used.
use super::client::Client;
use super::codex::OPENAI_CODEX_API_BASE_URL;
use super::completion::{InputAudio, ToolChoice};
use super::responses_api::streaming::StreamingCompletionResponse;
use crate::completion::message::{
    self, AudioMediaType, ConversionSnafu, Document, DocumentMediaType, DocumentSourceKind,
    ImageDetail, MessageError, MimeType, Text,
};
use crate::completion::{
    ClientSnafu, CompletionError, ProviderSnafu, ResponseSnafu, SerializeSnafu,
};
use crate::http_client::HttpClientExt;
use crate::http_client::{self, HttpSnafu};
use crate::json_utils;
use crate::one_or_many::{OneOrMany, string_or_one_or_many};

use crate::completion;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use snafu::{OptionExt, ResultExt, whatever};
use tracing::{Instrument, Level, enabled, info_span};

use std::convert::Infallible;
use std::ops::Add;
use std::str::FromStr;

pub mod streaming;
pub mod websocket;

/// The completion request type for OpenAI's Response API: <https://platform.openai.com/docs/api-reference/responses/create>
/// Intended to be derived from [`crate::completion::request::CompletionRequest`].
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CompletionRequest {
    /// Message inputs
    pub input: OneOrMany<InputItem>,
    /// The model name
    pub model: String,
    /// Instructions (also referred to as preamble, although in other APIs this would be the "system prompt")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// The maximum number of output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Toggle to true for streaming responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// The temperature. Set higher (up to a max of 1.0) for more creative responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Whether the LLM should be forced to use a tool before returning a response.
    /// If none provided, the default option is "auto".
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    /// The tools you want to use. This supports both function tools and hosted tools
    /// such as `web_search`, `file_search`, and `computer_use`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ResponsesToolDefinition>,
    /// Additional parameters
    #[serde(flatten)]
    pub additional_parameters: AdditionalParameters,
}

impl CompletionRequest {
    pub fn with_structured_outputs<S>(mut self, schema_name: S, schema: serde_json::Value) -> Self
    where
        S: Into<String>,
    {
        self.additional_parameters.text = Some(TextConfig::structured_output(schema_name, schema));

        self
    }

    pub fn with_reasoning(mut self, reasoning: Reasoning) -> Self {
        self.additional_parameters.reasoning = Some(reasoning);

        self
    }

    /// Adds a provider-native hosted tool (e.g. `web_search`, `file_search`, `computer_use`)
    /// to the request. These tools are executed by OpenAI's infrastructure, not by Rig's
    /// agent loop.
    pub fn with_tool(mut self, tool: impl Into<ResponsesToolDefinition>) -> Self {
        self.tools.push(tool.into());
        self
    }

    /// Adds multiple provider-native hosted tools to the request. These tools are executed
    /// by OpenAI's infrastructure, not by Rig's agent loop.
    pub fn with_tools<I, Tool>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = Tool>,
        Tool: Into<ResponsesToolDefinition>,
    {
        self.tools.extend(tools.into_iter().map(Into::into));
        self
    }
}

/// An input item for [`CompletionRequest`].
#[derive(Debug, Deserialize, Clone)]
pub struct InputItem {
    /// The role of an input item/message.
    /// Input messages should be Some(Role::User), and output messages should be Some(Role::Assistant).
    /// Everything else should be None.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<Role>,
    /// The input content itself.
    #[serde(flatten)]
    input: InputContent,
}

impl Serialize for InputItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut value = serde_json::to_value(&self.input).map_err(serde::ser::Error::custom)?;
        let map = value.as_object_mut().ok_or_else(|| {
            serde::ser::Error::custom("Input content must serialize to an object")
        })?;

        if let Some(role) = &self.role
            && !map.contains_key("role")
        {
            map.insert(
                "role".to_string(),
                serde_json::to_value(role).map_err(serde::ser::Error::custom)?,
            );
        }

        value.serialize(serializer)
    }
}

impl InputItem {
    pub fn system_message(content: impl Into<String>) -> Self {
        Self {
            role: Some(Role::System),
            input: InputContent::Message(Message::System {
                content: OneOrMany::one(SystemContent::InputText {
                    text: content.into(),
                }),
                name: None,
            }),
        }
    }
}

/// Message roles. Used by OpenAI Responses API to determine who created a given message.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// The type of content used in an [`InputItem`]. Additionally holds data for each type of input content.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContent {
    Message(Message),
    Reasoning(OpenAIReasoning),
    FunctionCall(OutputFunctionCall),
    FunctionCallOutput(ToolResult),
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct OpenAIReasoning {
    id: String,
    pub summary: Vec<ReasoningSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolStatus>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummary {
    SummaryText { text: String },
}

impl ReasoningSummary {
    fn new(input: &str) -> Self {
        Self::SummaryText {
            text: input.to_string(),
        }
    }

    pub fn text(&self) -> String {
        let ReasoningSummary::SummaryText { text } = self;
        text.clone()
    }
}

/// A tool result.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolResult {
    /// The call ID of a tool (this should be linked to the call ID for a tool call, otherwise an error will be received)
    call_id: String,
    /// The result of a tool call.
    output: String,
    /// The status of a tool call (if used in a completion request, this should always be Completed)
    status: ToolStatus,
}

impl From<Message> for InputItem {
    fn from(value: Message) -> Self {
        match value {
            Message::User { .. } => Self {
                role: Some(Role::User),
                input: InputContent::Message(value),
            },
            Message::Assistant { ref content, .. } => {
                let role = if content
                    .iter()
                    .any(|x| matches!(x, AssistantContentType::Reasoning(_)))
                {
                    None
                } else {
                    Some(Role::Assistant)
                };
                Self {
                    role,
                    input: InputContent::Message(value),
                }
            }
            Message::System { .. } => Self {
                role: Some(Role::System),
                input: InputContent::Message(value),
            },
            Message::ToolResult {
                tool_call_id,
                output,
            } => Self {
                role: None,
                input: InputContent::FunctionCallOutput(ToolResult {
                    call_id: tool_call_id,
                    output,
                    status: ToolStatus::Completed,
                }),
            },
        }
    }
}

impl TryFrom<crate::completion::Message> for Vec<InputItem> {
    type Error = CompletionError;

    fn try_from(value: crate::completion::Message) -> Result<Self, Self::Error> {
        match value {
            crate::completion::Message::System { content } => Ok(vec![InputItem {
                role: Some(Role::System),
                input: InputContent::Message(Message::System {
                    content: OneOrMany::one(content.into()),
                    name: None,
                }),
            }]),
            crate::completion::Message::User { content } => {
                let mut items = Vec::new();

                for user_content in content {
                    match user_content {
                        crate::completion::message::UserContent::Text(Text { text }) => {
                            items.push(InputItem {
                                role: Some(Role::User),
                                input: InputContent::Message(Message::User {
                                    content: OneOrMany::one(UserContent::InputText { text }),
                                    name: None,
                                }),
                            });
                        }
                        crate::completion::message::UserContent::ToolResult(
                            crate::completion::message::ToolResult {
                                call_id,
                                content: tool_content,
                                ..
                            },
                        ) => {
                            let call_id = call_id.ok_or_else(|| {
                                ProviderSnafu {
                                    msg:
                                        "Tool result `call_id` is required for OpenAI Responses API"
                                            .to_string(),
                                }
                                .build()
                            })?;
                            for tool_result_content in tool_content {
                                let text = match tool_result_content {
                                    crate::completion::message::ToolResultContent::Text(Text {
                                        text,
                                    }) => text,
                                    other => serde_json::to_string(&other).map_err(|_| {
                                        ProviderSnafu {
                                            msg: "Tool result content could not be serialized"
                                                .to_string(),
                                        }
                                        .build()
                                    })?,
                                };
                                items.push(InputItem {
                                    role: None,
                                    input: InputContent::FunctionCallOutput(ToolResult {
                                        call_id: call_id.clone(),
                                        output: text,
                                        status: ToolStatus::Completed,
                                    }),
                                });
                            }
                        }
                        crate::completion::message::UserContent::Document(Document {
                            data,
                            media_type: Some(DocumentMediaType::PDF),
                            ..
                        }) => {
                            let (file_data, file_url) = match data {
                                DocumentSourceKind::Base64(data) => {
                                    (Some(format!("data:application/pdf;base64,{data}")), None)
                                }
                                DocumentSourceKind::Url(url) => (None, Some(url)),
                                DocumentSourceKind::Raw(_) => {
                                    whatever!(
                                        "Raw file data not supported, encode as base64 first"
                                    );
                                }
                                doc => {
                                    whatever!("Unsupported document type: {doc}")
                                }
                            };

                            items.push(InputItem {
                                role: Some(Role::User),
                                input: InputContent::Message(Message::User {
                                    content: OneOrMany::one(UserContent::InputFile {
                                        file_data,
                                        file_url,
                                        filename: Some("document.pdf".to_string()),
                                    }),
                                    name: None,
                                }),
                            })
                        }
                        crate::completion::message::UserContent::Document(Document {
                            data:
                                DocumentSourceKind::Base64(text) | DocumentSourceKind::String(text),
                            ..
                        }) => items.push(InputItem {
                            role: Some(Role::User),
                            input: InputContent::Message(Message::User {
                                content: OneOrMany::one(UserContent::InputText { text }),
                                name: None,
                            }),
                        }),
                        crate::completion::message::UserContent::Image(
                            crate::completion::message::Image {
                                data,
                                media_type,
                                detail,
                                ..
                            },
                        ) => {
                            let url = match data {
                                DocumentSourceKind::Base64(data) => {
                                    let media_type = if let Some(media_type) = media_type {
                                        media_type.to_mime_type().to_string()
                                    } else {
                                        String::new()
                                    };
                                    format!("data:{media_type};base64,{data}")
                                }
                                DocumentSourceKind::Url(url) => url,
                                DocumentSourceKind::Raw(_) => {
                                    whatever!("Raw file data not supported, encode as base64 first")
                                }
                                doc => {
                                    whatever!("Unsupported document type: {doc}")
                                }
                            };
                            items.push(InputItem {
                                role: Some(Role::User),
                                input: InputContent::Message(Message::User {
                                    content: OneOrMany::one(UserContent::InputImage {
                                        image_url: url,
                                        detail: detail.unwrap_or_default(),
                                    }),
                                    name: None,
                                }),
                            });
                        }
                        message => {
                            return Err(ProviderSnafu {
                                msg: format!("Unsupported message: {message:?}"),
                            }
                            .build());
                        }
                    }
                }

                Ok(items)
            }
            crate::completion::Message::Assistant { id, content } => {
                let mut reasoning_items = Vec::new();
                let mut other_items = Vec::new();

                for assistant_content in content {
                    match assistant_content {
                        crate::completion::message::AssistantContent::Text(Text { text }) => {
                            let id = id.as_ref().unwrap_or(&String::default()).clone();
                            other_items.push(InputItem {
                                role: Some(Role::Assistant),
                                input: InputContent::Message(Message::Assistant {
                                    content: OneOrMany::one(AssistantContentType::Text(
                                        AssistantContent::OutputText(Text { text }),
                                    )),
                                    id,
                                    name: None,
                                    status: ToolStatus::Completed,
                                }),
                            });
                        }
                        crate::completion::message::AssistantContent::ToolCall(
                            crate::completion::message::ToolCall {
                                id: tool_id,
                                call_id,
                                function,
                                ..
                            },
                        ) => {
                            other_items.push(InputItem {
                                role: None,
                                input: InputContent::FunctionCall(OutputFunctionCall {
                                    arguments: function.arguments,
                                    call_id: require_call_id(call_id, "Assistant tool call")?,
                                    id: tool_id,
                                    name: function.name,
                                    status: ToolStatus::Completed,
                                }),
                            });
                        }
                        crate::completion::message::AssistantContent::Reasoning(reasoning) => {
                            let openai_reasoning =
                                openai_reasoning_from_core(&reasoning).map_err(|err| {
                                    {
                                        ProviderSnafu {
                                            msg: err.to_string(),
                                        }
                                    }
                                    .build()
                                })?;
                            reasoning_items.push(InputItem {
                                role: None,
                                input: InputContent::Reasoning(openai_reasoning),
                            });
                        }
                        crate::completion::message::AssistantContent::Image(_) => {
                            return Err(ProviderSnafu{
                                msg: "Assistant image content is not supported in OpenAI Responses API",
                             }.build());
                        }
                    }
                }

                let mut items = reasoning_items;
                items.extend(other_items);
                Ok(items)
            }
        }
    }
}

impl From<OneOrMany<String>> for Vec<ReasoningSummary> {
    fn from(value: OneOrMany<String>) -> Self {
        value.iter().map(|x| ReasoningSummary::new(x)).collect()
    }
}

fn require_call_id(call_id: Option<String>, context: &str) -> Result<String, CompletionError> {
    call_id.ok_or_else(|| CompletionError::Request {
        message: format!("{context} `call_id` is required for OpenAI Responses API"),
        source: None,
    })
}

fn openai_reasoning_from_core(
    reasoning: &crate::completion::message::Reasoning,
) -> Result<OpenAIReasoning, MessageError> {
    let id = reasoning.id.clone().context(ConversionSnafu {
        msg: "An OpenAI-generated ID is required when using OpenAI reasoning items",
    })?;
    let mut summary = Vec::new();
    let mut encrypted_content = None;
    for content in &reasoning.content {
        match content {
            crate::completion::message::ReasoningContent::Text { text, .. }
            | crate::completion::message::ReasoningContent::Summary(text) => {
                summary.push(ReasoningSummary::new(text));
            }
            // OpenAI reasoning input has one opaque payload field; preserve either
            // encrypted or redacted blocks there, preferring the first one seen.
            crate::completion::message::ReasoningContent::Encrypted(data)
            | crate::completion::message::ReasoningContent::Redacted { data } => {
                encrypted_content.get_or_insert_with(|| data.clone());
            }
        }
    }

    Ok(OpenAIReasoning {
        id,
        summary,
        encrypted_content,
        status: None,
    })
}

/// The definition of a tool response, repurposed for OpenAI's Responses API.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ResponsesToolDefinition {
    /// The type of tool.
    #[serde(rename = "type")]
    pub kind: String,
    /// Tool name
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Parameters - this should be a JSON schema. Tools should additionally ensure an "additionalParameters" field has been added with the value set to false, as this is required if using OpenAI's strict mode (enabled by default).
    #[serde(default, skip_serializing_if = "is_json_null")]
    pub parameters: serde_json::Value,
    /// Whether to use strict mode. Enabled by default as it allows for improved efficiency.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
    /// Tool description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Additional provider-specific configuration for hosted tools.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
}

fn is_json_null(value: &Value) -> bool {
    value.is_null()
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Normalizes a tool name for providers that only allow `[A-Za-z0-9_-]`.
///
/// OpenAI/Codex rejects `/` and other characters for function tool names. We preserve
/// valid identifier characters and replace all others with `_`.
fn sanitize_openai_tool_name(name: &str) -> String {
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

fn is_alias_only_any_of_variant(value: &Value) -> bool {
    match value {
        Value::Object(variant) => {
            variant.contains_key("required")
                && !variant.contains_key("type")
                && variant.len() <= 2
                && variant
                    .keys()
                    .all(|key| key == "required" || key == "additionalProperties")
        }
        _ => false,
    }
}

/// Removes fields that Codex does not accept in function schemas while keeping the
/// remaining schema as strict as OpenAI's requirements.
fn sanitize_codex_tool_schema(schema: &mut Value) {
    if let Value::Object(root) = schema {
        // Codex rejects top-level schema combinators and enums.
        if let Some(Value::Array(any_of)) = root.get("anyOf") {
            // The tool schemas in this project intentionally model `session_id`/`sessionId`
            // as alias variants here; we keep both properties in the object schema and
            // drop those variants to satisfy strict Codex payload constraints.
            if any_of.iter().all(is_alias_only_any_of_variant) {
                root.remove("anyOf");

                // ACP/MCP requires `sessionId` for compatibility, but Codex
                // cannot validate alias-only variants in strict schema mode. Keep the
                // canonical `session_id` key for callability and drop the alias form.
                if let Some(Value::Object(properties)) = root.get_mut("properties") {
                    properties.remove("sessionId");
                }
            }
        }

        // `_meta` is an ACP/MCP transport envelope field and should not be sent
        // in tool arguments. Remove it from the public schema so strict Codex
        // validation does not expect caller-provided metadata.
        if let Some(Value::Object(properties)) = root.get_mut("properties") {
            properties.remove("_meta");
        }

        root.remove("oneOf");
        root.remove("allOf");
        root.remove("enum");
        root.remove("not");

        // Ensure required stays consistent with the sanitized properties list.
        let property_keys = match root.get("properties") {
            Some(Value::Object(properties)) => properties
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };

        if let Some(Value::Array(required)) = root.get_mut("required") {
            required.retain(|required_key| {
                required_key
                    .as_str()
                    .is_some_and(|k| property_keys.iter().any(|property_key| property_key == k))
            });
        }

        // Recurse through nested schema objects to keep codex sanitization
        // consistent for aliases in nested structures.
        if let Some(Value::Object(nested)) = root.get_mut("properties") {
            for (_, property_schema) in nested.iter_mut() {
                sanitize_codex_tool_schema(property_schema);
            }
        }

        if let Some(Value::Object(defs)) = root.get_mut("$defs") {
            for (_, def_schema) in defs.iter_mut() {
                sanitize_codex_tool_schema(def_schema);
            }
        }

        if let Some(Value::Array(any_of)) = root.get_mut("anyOf") {
            for schema in any_of.iter_mut() {
                sanitize_codex_tool_schema(schema);
            }
        }

        if let Some(Value::Array(one_of)) = root.get_mut("oneOf") {
            for schema in one_of.iter_mut() {
                sanitize_codex_tool_schema(schema);
            }
        }

        if let Some(Value::Array(all_of)) = root.get_mut("allOf") {
            for schema in all_of.iter_mut() {
                sanitize_codex_tool_schema(schema);
            }
        }
    }
}

/// Canonicalizes provider tool-call arguments so kernel/runtime code only sees the
/// snake_case internal representation for fields that may arrive in multiple forms.
fn normalize_provider_tool_arguments(mut arguments: Value) -> Value {
    if let Value::Object(ref mut map) = arguments {
        // Codex may emit both `session_id` and `sessionId` in the same tool call even
        // when the public schema only exposes one canonical field. Keep the internal
        // snake_case key so downstream serde alias handling does not trip on duplicates.
        if map.contains_key("session_id") && map.contains_key("sessionId") {
            map.remove("sessionId");
        }
    }

    arguments
}

/// Deserializes stringified provider tool arguments and canonicalizes known aliases.
fn deserialize_tool_call_arguments<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    let arguments = json_utils::stringified_json::deserialize(deserializer)?;
    Ok(normalize_provider_tool_arguments(arguments))
}

impl ResponsesToolDefinition {
    /// Creates a function tool definition.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        mut parameters: serde_json::Value,
    ) -> Self {
        super::sanitize_schema(&mut parameters);

        Self {
            kind: "function".to_string(),
            name: name.into(),
            parameters,
            strict: true,
            description: description.into(),
            config: Map::new(),
        }
    }

    /// Creates a hosted tool definition for an arbitrary hosted tool type.
    pub fn hosted(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: String::new(),
            parameters: Value::Null,
            strict: false,
            description: String::new(),
            config: Map::new(),
        }
    }

    /// Creates a hosted `web_search` tool definition.
    pub fn web_search() -> Self {
        Self::hosted("web_search")
    }

    /// Creates a hosted `file_search` tool definition.
    pub fn file_search() -> Self {
        Self::hosted("file_search")
    }

    /// Creates a hosted `computer_use` tool definition.
    pub fn computer_use() -> Self {
        Self::hosted("computer_use")
    }

    /// Adds hosted-tool configuration fields.
    pub fn with_config(mut self, key: impl Into<String>, value: Value) -> Self {
        self.config.insert(key.into(), value);
        self
    }

    fn normalize(mut self) -> Self {
        if self.kind == "function" {
            super::sanitize_schema(&mut self.parameters);
            self.strict = true;
        }
        self
    }
}

impl From<completion::ToolDefinition> for ResponsesToolDefinition {
    fn from(value: completion::ToolDefinition) -> Self {
        let completion::ToolDefinition {
            name,
            parameters,
            description,
        } = value;

        Self::function(name, description, parameters)
    }
}

/// Token usage.
/// Token usage from the OpenAI Responses API generally shows the input tokens and output tokens (both with more in-depth details) as well as a total tokens field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponsesUsage {
    /// Input tokens
    pub input_tokens: u64,
    /// In-depth detail on input tokens (cached tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<InputTokensDetails>,
    /// Output tokens
    pub output_tokens: u64,
    /// In-depth detail on output tokens (reasoning tokens)
    pub output_tokens_details: OutputTokensDetails,
    /// Total tokens used (for a given prompt)
    pub total_tokens: u64,
}

impl ResponsesUsage {
    /// Create a new ResponsesUsage instance
    pub(crate) fn new() -> Self {
        Self {
            input_tokens: 0,
            input_tokens_details: Some(InputTokensDetails::new()),
            output_tokens: 0,
            output_tokens_details: OutputTokensDetails::new(),
            total_tokens: 0,
        }
    }
}

impl Add for ResponsesUsage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        let input_tokens = self.input_tokens + rhs.input_tokens;
        let input_tokens_details = self.input_tokens_details.map(|lhs| {
            if let Some(tokens) = rhs.input_tokens_details {
                lhs + tokens
            } else {
                lhs
            }
        });
        let output_tokens = self.output_tokens + rhs.output_tokens;
        let output_tokens_details = self.output_tokens_details + rhs.output_tokens_details;
        let total_tokens = self.total_tokens + rhs.total_tokens;
        Self {
            input_tokens,
            input_tokens_details,
            output_tokens,
            output_tokens_details,
            total_tokens,
        }
    }
}

/// In-depth details on input tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputTokensDetails {
    /// Cached tokens from OpenAI
    pub cached_tokens: u64,
}

impl InputTokensDetails {
    pub(crate) fn new() -> Self {
        Self { cached_tokens: 0 }
    }
}

impl Add for InputTokensDetails {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            cached_tokens: self.cached_tokens + rhs.cached_tokens,
        }
    }
}

/// In-depth details on output tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    /// Reasoning tokens
    pub reasoning_tokens: u64,
}

impl OutputTokensDetails {
    pub(crate) fn new() -> Self {
        Self {
            reasoning_tokens: 0,
        }
    }
}

impl Add for OutputTokensDetails {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            reasoning_tokens: self.reasoning_tokens + rhs.reasoning_tokens,
        }
    }
}

/// Occasionally, when using OpenAI's Responses API you may get an incomplete response. This struct holds the reason as to why it happened.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IncompleteDetailsReason {
    /// The reason for an incomplete [`CompletionResponse`].
    pub reason: String,
}

/// A response error from OpenAI's Response API.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResponseError {
    /// Error code
    pub code: String,
    /// Error message
    pub message: String,
}

/// A response object as an enum (ensures type validation)
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseObject {
    Response,
}

/// The response status as an enum (ensures type validation)
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
    Queued,
    Incomplete,
}

/// Attempt to try and create a `NewCompletionRequest` from a model name and [`crate::completion::CompletionRequest`]
impl TryFrom<(String, crate::completion::CompletionRequest)> for CompletionRequest {
    type Error = CompletionError;
    fn try_from(
        (model, mut req): (String, crate::completion::CompletionRequest),
    ) -> Result<Self, Self::Error> {
        let model = req.model.clone().unwrap_or(model);
        let input = {
            let mut partial_history = vec![];
            partial_history.extend(req.chat_history);

            // Initialize full history with preamble (or empty if non-existent)
            // Some "Responses API compatible" providers don't support `instructions` field
            // so we need to add a system message until further notice
            let mut full_history: Vec<InputItem> = if let Some(content) = req.preamble {
                vec![InputItem::system_message(content)]
            } else {
                Vec::new()
            };

            for history_item in partial_history {
                full_history.extend(<Vec<InputItem>>::try_from(history_item)?);
            }

            full_history
        };

        let input = OneOrMany::many(input).map_err(|_| CompletionError::Request {
            message: "OpenAI Responses request input must contain at least one item".into(),
            source: None,
        })?;

        let mut additional_params_payload = req.additional_params.take().unwrap_or(Value::Null);
        let stream = match &additional_params_payload {
            Value::Bool(stream) => Some(*stream),
            Value::Object(map) => map.get("stream").and_then(Value::as_bool),
            _ => None,
        };

        let mut additional_tools = Vec::new();
        if let Some(additional_params_map) = additional_params_payload.as_object_mut() {
            if let Some(raw_tools) = additional_params_map.remove("tools") {
                additional_tools = serde_json::from_value::<Vec<ResponsesToolDefinition>>(
                    raw_tools,
                )
                .map_err(|err| CompletionError::Request {
                    message: format!(
                        "Invalid OpenAI Responses tools payload in additional_params: {err}"
                    ),
                    source: Some(Box::new(err)),
                })?;
            }
            additional_params_map.remove("stream");
        }

        if additional_params_payload.is_boolean() {
            additional_params_payload = Value::Null;
        }

        additional_tools = additional_tools
            .into_iter()
            .map(ResponsesToolDefinition::normalize)
            .collect();

        let mut additional_parameters = if additional_params_payload.is_null() {
            // If there's no additional parameters, initialise an empty object
            AdditionalParameters::default()
        } else {
            serde_json::from_value::<AdditionalParameters>(additional_params_payload).map_err(
                |err| CompletionError::Request {
                    message: format!("Invalid OpenAI Responses additional_params payload: {err}"),
                    source: Some(Box::new(err)),
                },
            )?
        };
        if additional_parameters.reasoning.is_some() {
            let include = additional_parameters.include.get_or_insert_with(Vec::new);
            if !include
                .iter()
                .any(|item| matches!(item, Include::ReasoningEncryptedContent))
            {
                include.push(Include::ReasoningEncryptedContent);
            }
        }

        // Apply output_schema as structured output if not already configured via additional_params
        if additional_parameters.text.is_none()
            && let Some(schema) = req.output_schema
        {
            let name = schema
                .as_object()
                .and_then(|o| o.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("response_schema")
                .to_string();
            let mut schema_value = schema.to_value();
            super::sanitize_schema(&mut schema_value);
            additional_parameters.text = Some(TextConfig::structured_output(name, schema_value));
        }

        let tool_choice = req.tool_choice.map(ToolChoice::try_from).transpose()?;
        let mut tools: Vec<ResponsesToolDefinition> = req
            .tools
            .into_iter()
            .map(ResponsesToolDefinition::from)
            .collect();
        tools.append(&mut additional_tools);

        Ok(Self {
            input,
            model,
            instructions: None, // is currently None due to lack of support in compliant providers
            max_output_tokens: req.max_tokens,
            stream,
            tool_choice,
            tools,
            temperature: req.temperature,
            additional_parameters,
        })
    }
}

/// The completion model struct for OpenAI's response API.
#[derive(Clone)]
pub struct ResponsesCompletionModel<T = reqwest::Client> {
    /// The OpenAI client
    pub(crate) client: Client<T>,
    /// Name of the model (e.g.: gpt-3.5-turbo-1106)
    pub model: String,
    /// Model-level default tools that are always added to outgoing requests.
    pub tools: Vec<ResponsesToolDefinition>,
}

impl<T> ResponsesCompletionModel<T>
where
    T: HttpClientExt + Clone + Default + std::fmt::Debug + 'static,
{
    /// Creates a new [`ResponsesCompletionModel`].
    pub fn new(client: Client<T>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            tools: Vec::new(),
        }
    }

    pub fn with_model(client: Client<T>, model: &str) -> Self {
        Self {
            client,
            model: model.to_string(),
            tools: Vec::new(),
        }
    }

    /// Adds a default tool to all requests from this model.
    pub fn with_tool(mut self, tool: impl Into<ResponsesToolDefinition>) -> Self {
        self.tools.push(tool.into());
        self
    }

    /// Adds default tools to all requests from this model.
    pub fn with_tools<I, Tool>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = Tool>,
        Tool: Into<ResponsesToolDefinition>,
    {
        self.tools.extend(tools.into_iter().map(Into::into));
        self
    }

    /// Use the Completions API instead of Responses.
    pub fn completions_api(self) -> crate::providers::openai::completion::CompletionModel<T> {
        super::completion::CompletionModel::with_model(self.client.completions_api(), &self.model)
    }

    /// Attempt to create a completion request from [`crate::completion::CompletionRequest`].
    pub(crate) fn create_completion_request(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<CompletionRequest, CompletionError> {
        let mut req = CompletionRequest::try_from((self.model.clone(), completion_request))?;

        let is_codex = self.client.base_url().trim_end_matches('/')
            == OPENAI_CODEX_API_BASE_URL.trim_end_matches('/');

        // Codex requests require slightly different response handling and stricter
        // payload constraints, so we apply Codex-specific normalization here.
        if is_codex {
            // Codex API requires responses to not be stored, otherwise it rejects the request.
            req.additional_parameters.store = Some(false);

            let mut instructions = Vec::new();
            let mut non_system_messages = Vec::new();

            for item in req.input.iter() {
                match item {
                    InputItem {
                        role: Some(Role::System),
                        input: InputContent::Message(Message::System { content, .. }),
                        ..
                    } => {
                        instructions.extend(content.iter().map(
                            |system_content| match system_content {
                                SystemContent::InputText { text } => text.clone(),
                            },
                        ));
                    }
                    item => {
                        non_system_messages.push(item.clone());
                    }
                }
            }

            if !instructions.is_empty() {
                req.instructions = Some(instructions.join("\n\n"));
                req.input =
                    OneOrMany::many(non_system_messages).map_err(|_| CompletionError::Request {
                        message: "OpenAI Responses request input must contain at least one item"
                            .into(),
                        source: None,
                    })?;
            }
        }

        req.tools.extend(self.tools.clone());

        // OpenAI/Codex tool name validation does not allow `/`, so normalize function
        // tool names to match the required character class while leaving hosted tool names
        // unchanged.
        if is_codex {
            for tool in req.tools.iter_mut() {
                if tool.kind == "function" {
                    tool.name = sanitize_openai_tool_name(&tool.name);
                    sanitize_codex_tool_schema(&mut tool.parameters);
                }
            }
        }

        Ok(req)
    }
}

/// The standard response format from OpenAI's Responses API.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The ID of a completion response.
    pub id: String,
    /// The type of the object.
    pub object: ResponseObject,
    /// The time at which a given response has been created, in seconds from the UNIX epoch (01/01/1970 00:00:00).
    pub created_at: u64,
    /// The status of the response.
    pub status: ResponseStatus,
    /// Response error (optional)
    pub error: Option<ResponseError>,
    /// Incomplete response details (optional)
    pub incomplete_details: Option<IncompleteDetailsReason>,
    /// System prompt/preamble
    pub instructions: Option<String>,
    /// The maximum number of tokens the model should output
    pub max_output_tokens: Option<u64>,
    /// The model name
    pub model: String,
    /// Token usage
    pub usage: Option<ResponsesUsage>,
    /// The model output (messages, etc will go here)
    pub output: Vec<Output>,
    /// Tools
    #[serde(default)]
    pub tools: Vec<ResponsesToolDefinition>,
    /// Additional parameters
    #[serde(flatten)]
    pub additional_parameters: AdditionalParameters,
}

/// Additional parameters for the completion request type for OpenAI's Response API: <https://platform.openai.com/docs/api-reference/responses/create>
/// Intended to be derived from [`crate::completion::request::CompletionRequest`].
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct AdditionalParameters {
    /// Whether or not a given model task should run in the background (ie a detached process).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// The text response format. This is where you would add structured outputs (if you want them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextConfig>,
    /// What types of extra data you would like to include. This is mostly useless at the moment since the types of extra data to add is currently unsupported, but this will be coming soon!
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<Include>>,
    /// `top_p`. Mutually exclusive with the `temperature` argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Whether or not the response should be truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationStrategy>,
    /// The username of the user (that you want to use).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Any additional metadata you'd like to add. This will additionally be returned by the response.
    #[serde(skip_serializing_if = "Map::is_empty", default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Whether or not you want tool calls to run in parallel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Previous response ID. If you are not sending a full conversation, this can help to track the message flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Add thinking/reasoning to your response. The response will be emitted as a list member of the `output` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// The service tier you're using.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<OpenAIServiceTier>,
    /// Whether or not to store the response for later retrieval by API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
}

impl AdditionalParameters {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::Value::Object(Map::new()))
    }
}

/// The truncation strategy.
/// When using auto, if the context of this response and previous ones exceeds the model's context window size, the model will truncate the response to fit the context window by dropping input items in the middle of the conversation.
/// Otherwise, does nothing (and is disabled by default).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationStrategy {
    Auto,
    #[default]
    Disabled,
}

/// The model output format configuration.
/// You can either have plain text by default, or attach a JSON schema for the purposes of structured outputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextConfig {
    pub format: TextFormat,
}

impl TextConfig {
    pub(crate) fn structured_output<S>(name: S, schema: serde_json::Value) -> Self
    where
        S: Into<String>,
    {
        Self {
            format: TextFormat::JsonSchema(StructuredOutputsInput {
                name: name.into(),
                schema,
                strict: true,
            }),
        }
    }
}

/// The text format (contained by [`TextConfig`]).
/// You can either have plain text by default, or attach a JSON schema for the purposes of structured outputs.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    JsonSchema(StructuredOutputsInput),
    #[default]
    Text,
}

/// The inputs required for adding structured outputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StructuredOutputsInput {
    /// The name of your schema.
    pub name: String,
    /// Your required output schema. It is recommended that you use the JsonSchema macro, which you can check out at <https://docs.rs/schemars/latest/schemars/trait.JsonSchema.html>.
    pub schema: serde_json::Value,
    /// Enable strict output. If you are using your AI agent in a data pipeline or another scenario that requires the data to be absolutely fixed to a given schema, it is recommended to set this to true.
    #[serde(default)]
    pub strict: bool,
}

/// Add reasoning to a [`CompletionRequest`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reasoning {
    /// How much effort you want the model to put into thinking/reasoning.
    pub effort: Option<ReasoningEffort>,
    /// How much effort you want the model to put into writing the reasoning summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryLevel>,
}

impl Reasoning {
    /// Creates a new Reasoning instantiation (with empty values).
    pub fn new() -> Self {
        Self {
            effort: None,
            summary: None,
        }
    }

    /// Adds reasoning effort.
    pub fn with_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.effort = Some(reasoning_effort);

        self
    }

    /// Adds summary level (how detailed the reasoning summary will be).
    pub fn with_summary_level(mut self, reasoning_summary_level: ReasoningSummaryLevel) -> Self {
        self.summary = Some(reasoning_summary_level);

        self
    }
}

/// The billing service tier that will be used. On auto by default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAIServiceTier {
    #[default]
    Auto,
    Default,
    Flex,
}

/// The amount of reasoning effort that will be used by a given model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
}

/// The amount of effort that will go into a reasoning summary by a given model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSummaryLevel {
    #[default]
    Auto,
    Concise,
    Detailed,
}

/// Results to additionally include in the OpenAI Responses API.
/// Note that most of these are currently unsupported, but have been added for completeness.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Include {
    #[serde(rename = "file_search_call.results")]
    FileSearchCallResults,
    #[serde(rename = "message.input_image.image_url")]
    MessageInputImageImageUrl,
    #[serde(rename = "computer_call.output.image_url")]
    ComputerCallOutputOutputImageUrl,
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
    #[serde(rename = "code_interpreter_call.outputs")]
    CodeInterpreterCallOutputs,
}

/// A currently non-exhaustive list of output types.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Message(OutputMessage),
    #[serde(alias = "function_call")]
    FunctionCall(OutputFunctionCall),
    Reasoning {
        id: String,
        summary: Vec<ReasoningSummary>,
        #[serde(default)]
        encrypted_content: Option<String>,
        #[serde(default)]
        status: Option<ToolStatus>,
    },
}

impl From<Output> for Vec<completion::AssistantContent> {
    fn from(value: Output) -> Self {
        let res: Vec<completion::AssistantContent> = match value {
            Output::Message(OutputMessage { content, .. }) => content
                .into_iter()
                .map(completion::AssistantContent::from)
                .collect(),
            Output::FunctionCall(OutputFunctionCall {
                id,
                arguments,
                call_id,
                name,
                ..
            }) => vec![completion::AssistantContent::tool_call_with_call_id(
                id, call_id, name, arguments,
            )],
            Output::Reasoning {
                id,
                summary,
                encrypted_content,
                ..
            } => {
                let mut content = summary
                    .into_iter()
                    .map(|summary| match summary {
                        ReasoningSummary::SummaryText { text } => {
                            message::ReasoningContent::Summary(text)
                        }
                    })
                    .collect::<Vec<_>>();
                if let Some(encrypted_content) = encrypted_content {
                    content.push(message::ReasoningContent::Encrypted(encrypted_content));
                }
                vec![completion::AssistantContent::Reasoning(
                    message::Reasoning {
                        id: Some(id),
                        content,
                    },
                )]
            }
        };

        res
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OutputReasoning {
    id: String,
    summary: Vec<ReasoningSummary>,
    status: ToolStatus,
}

/// An OpenAI Responses API tool call. A call ID will be returned that must be used when creating a tool result to send back to OpenAI as a message input, otherwise an error will be received.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OutputFunctionCall {
    pub id: String,
    #[serde(
        serialize_with = "json_utils::stringified_json::serialize",
        deserialize_with = "deserialize_tool_call_arguments"
    )]
    pub arguments: serde_json::Value,
    pub call_id: String,
    pub name: String,
    pub status: ToolStatus,
}

/// The status of a given tool.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    InProgress,
    Completed,
    Incomplete,
}

/// An output message from OpenAI's Responses API.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OutputMessage {
    /// The message ID. Must be included when sending the message back to OpenAI
    pub id: String,
    /// The role (currently only Assistant is available as this struct is only created when receiving an LLM message as a response)
    pub role: OutputRole,
    /// The status of the response
    pub status: ResponseStatus,
    /// The actual message content
    pub content: Vec<AssistantContent>,
}

/// The role of an output message.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutputRole {
    Assistant,
}

impl<T> completion::CompletionModel for ResponsesCompletionModel<T>
where
    T: HttpClientExt + Clone + std::fmt::Debug + Default + Send + Sync + 'static,
{
    type Response = CompletionResponse;
    type StreamingResponse = StreamingCompletionResponse;

    type Client = super::Client<T>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::new(client.clone(), model)
    }

    async fn completion(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<completion::CompletionResponse<Self::Response>, CompletionError> {
        let span = if tracing::Span::current().is_disabled() {
            info_span!(
                target: "rig::completions",
                "chat",
                gen_ai.operation.name = "chat",
                gen_ai.provider.name = tracing::field::Empty,
                gen_ai.request.model = tracing::field::Empty,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cached_tokens = tracing::field::Empty,
                gen_ai.input.messages = tracing::field::Empty,
                gen_ai.output.messages = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        span.record("gen_ai.provider.name", "openai");
        span.record("gen_ai.request.model", &self.model);
        let request = self.create_completion_request(completion_request)?;
        let body = serde_json::to_vec(&request).context(SerializeSnafu {
            stage: "serde-request",
        })?;

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "OpenAI Responses completion request: {request}",
                request = serde_json::to_string_pretty(&request).context(SerializeSnafu{stage:"openai-request-tracing"})?
            );
        }

        let req = self
            .client
            .post("/responses")
            .context(ClientSnafu {
                stage: "openai-post",
            })?
            .body(body)
            .context(HttpSnafu {
                stage: "openai-body",
            })
            .context(ClientSnafu {
                stage: "openai-body",
            })?;

        async move {
            let response = self.client.send(req).await.context(ClientSnafu {
                stage: "openai-response-send",
            })?;

            if response.status().is_success() {
                let t = http_client::text(response).await.context(ClientSnafu {
                    stage: "openai-post",
                })?;
                let response =
                    serde_json::from_str::<Self::Response>(&t).context(SerializeSnafu {
                        stage: "openai-deserialize-response",
                    })?;
                let span = tracing::Span::current();
                span.record("gen_ai.response.id", &response.id);
                span.record("gen_ai.response.model", &response.model);
                if let Some(ref usage) = response.usage {
                    span.record("gen_ai.usage.output_tokens", usage.output_tokens);
                    span.record("gen_ai.usage.input_tokens", usage.input_tokens);
                    span.record(
                        "gen_ai.usage.cached_tokens",
                        usage
                            .input_tokens_details
                            .as_ref()
                            .map(|d| d.cached_tokens)
                            .unwrap_or(0),
                    );
                }
                if enabled!(Level::TRACE) {
                    tracing::trace!(
                        target: "rig::completions",
                        "OpenAI Responses completion response: {response}",
                        response = serde_json::to_string_pretty(&response).context(SerializeSnafu{stage:"openai-response-tracing"})?
                    );
                }
                response.try_into()
            } else {
                let text = http_client::text(response).await.context(ClientSnafu{stage:"openai-response-text"})?;
                Err(ProviderSnafu { msg: text }.build())
            }
        }
        .instrument(span)
        .await
    }

    async fn stream(
        &self,
        request: crate::completion::CompletionRequest,
    ) -> Result<
        crate::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        CompletionError,
    > {
        ResponsesCompletionModel::stream(self, request).await
    }
}

impl TryFrom<CompletionResponse> for completion::CompletionResponse<CompletionResponse> {
    type Error = CompletionError;

    fn try_from(response: CompletionResponse) -> Result<Self, Self::Error> {
        if response.output.is_empty() {
            return Err(ResponseSnafu {
                msg: "Response contained no parts",
            }
            .build());
        }

        // Extract the msg_ ID from the first Output::Message item
        let message_id = response.output.iter().find_map(|item| match item {
            Output::Message(msg) => Some(msg.id.clone()),
            _ => None,
        });

        let content: Vec<completion::AssistantContent> = response
            .output
            .iter()
            .cloned()
            .flat_map(<Vec<completion::AssistantContent>>::from)
            .collect();

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
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_input_tokens: usage
                    .input_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0),
                cache_creation_input_tokens: 0,
            })
            .unwrap_or_default();

        Ok(completion::CompletionResponse {
            choice,
            usage,
            raw_response: response,
            message_id,
        })
    }
}

/// An OpenAI Responses API message.
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
        #[serde(deserialize_with = "string_or_one_or_many")]
        content: OneOrMany<UserContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        content: OneOrMany<AssistantContentType>,
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        status: ToolStatus,
    },
    #[serde(rename = "tool")]
    ToolResult {
        tool_call_id: String,
        output: String,
    },
}

/// The type of a tool result content item.
#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ToolResultContentType {
    #[default]
    Text,
}

impl Message {
    pub fn system(content: &str) -> Self {
        Message::System {
            content: OneOrMany::one(content.to_owned().into()),
            name: None,
        }
    }
}

/// Text assistant content.
/// Note that the text type in comparison to the Completions API is actually `output_text` rather than `text`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    OutputText(Text),
    Refusal { refusal: String },
}

impl From<AssistantContent> for completion::AssistantContent {
    fn from(value: AssistantContent) -> Self {
        match value {
            AssistantContent::Refusal { refusal } => {
                completion::AssistantContent::Text(Text { text: refusal })
            }
            AssistantContent::OutputText(Text { text }) => {
                completion::AssistantContent::Text(Text { text })
            }
        }
    }
}

/// The type of assistant content.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(untagged)]
pub enum AssistantContentType {
    Text(AssistantContent),
    ToolCall(OutputFunctionCall),
    Reasoning(OpenAIReasoning),
}

/// System content for the OpenAI Responses API.
/// Uses `input_text` type to match the Responses API format.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemContent {
    InputText { text: String },
}

impl From<String> for SystemContent {
    fn from(s: String) -> Self {
        SystemContent::InputText { text: s }
    }
}

impl std::str::FromStr for SystemContent {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SystemContent::InputText {
            text: s.to_string(),
        })
    }
}

/// Different types of user content.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(default)]
        detail: ImageDetail,
    },
    InputFile {
        #[serde(skip_serializing_if = "Option::is_none")]
        file_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    Audio {
        input_audio: InputAudio,
    },
    #[serde(rename = "tool")]
    ToolResult {
        tool_call_id: String,
        output: String,
    },
}

impl TryFrom<message::Message> for Vec<Message> {
    type Error = message::MessageError;

    fn try_from(message: message::Message) -> Result<Self, Self::Error> {
        match message {
            message::Message::System { content } => Ok(vec![Message::System {
                content: OneOrMany::one(content.into()),
                name: None,
            }]),
            message::Message::User { content } => {
                let (tool_results, other_content): (Vec<_>, Vec<_>) = content
                    .into_iter()
                    .partition(|content| matches!(content, message::UserContent::ToolResult(_)));

                // If there are messages with both tool results and user content, openai will only
                //  handle tool results. It's unlikely that there will be both.
                if !tool_results.is_empty() {
                    let mut outputs = Vec::new();

                    for content in tool_results {
                        let message::UserContent::ToolResult(tool_result) = content else {
                            unreachable!();
                        };

                        let call_id = tool_result.call_id.ok_or_else(|| {
                            ConversionSnafu {
                                msg: "Tool result `call_id` is required for OpenAI Responses API",
                            }
                            .build()
                        })?;

                        for tool_result_content in tool_result.content {
                            let output = match tool_result_content {
                                completion::message::ToolResultContent::Text(Text { text }) => text,
                                    other => serde_json::to_string(&other).map_err(|_| {
                                        ConversionSnafu {
                                            msg: "This API only currently supports serializable tool result parts",
                                        }
                                        .build()
                                    })?,
                                };

                            outputs.push(Message::ToolResult {
                                tool_call_id: call_id.clone(),
                                output,
                            });
                        }
                    }

                    Ok(outputs)
                } else {
                    let other_content = other_content
                        .into_iter()
                        .map(|content| match content {
                            message::UserContent::Text(message::Text { text }) => {
                                Ok(UserContent::InputText { text })
                            }
                            message::UserContent::Image(message::Image {
                                data,
                                detail,
                                media_type,
                                ..
                            }) => {
                                let url = match data {
                                    DocumentSourceKind::Base64(data) => {
                                        let media_type = if let Some(media_type) = media_type {
                                            media_type.to_mime_type().to_string()
                                        } else {
                                            String::new()
                                        };
                                        format!("data:{media_type};base64,{data}")
                                    }
                                    DocumentSourceKind::Url(url) => url,
                                    DocumentSourceKind::Raw(_) => {
                                        return Err(ConversionSnafu {
                                            msg: "Raw files not supported, encode as base64 first",
                                        }
                                        .build());
                                    }
                                    doc => {
                                        return Err(ConversionSnafu {
                                            msg: format!("Unsupported document type: {doc}"),
                                        }
                                        .build());
                                    }
                                };

                                Ok(UserContent::InputImage {
                                    image_url: url,
                                    detail: detail.unwrap_or_default(),
                                })
                            }
                            message::UserContent::Document(message::Document {
                                media_type: Some(DocumentMediaType::PDF),
                                data,
                                ..
                            }) => {
                                let (file_data, file_url, filename) = match data {
                                    DocumentSourceKind::Base64(data) => (
                                        Some(format!("data:application/pdf;base64,{data}")),
                                        None,
                                        Some("document.pdf".to_string()),
                                    ),
                                    DocumentSourceKind::Url(url) => (None, Some(url), None),
                                    DocumentSourceKind::Raw(_) => {
                                        return Err(ConversionSnafu {
                                            msg: "Raw files not supported, encode as base64 first",
                                        }
                                        .build());
                                    }
                                    doc => {
                                        return Err(ConversionSnafu {
                                            msg: format!("Unsupported document type: {doc}"),
                                        }
                                        .build());
                                    }
                                };

                                Ok(UserContent::InputFile {
                                    file_url,
                                    file_data,
                                    filename,
                                })
                            }
                            message::UserContent::Document(message::Document {
                                data: DocumentSourceKind::Base64(text),
                                ..
                            }) => Ok(UserContent::InputText { text }),
                            message::UserContent::Audio(message::Audio {
                                data: DocumentSourceKind::Base64(data),
                                media_type,
                                ..
                            }) => Ok(UserContent::Audio {
                                input_audio: InputAudio {
                                    data,
                                    format: match media_type {
                                        Some(media_type) => media_type,
                                        None => AudioMediaType::MP3,
                                    },
                                },
                            }),
                            message::UserContent::Audio(_) => Err(ConversionSnafu {
                                msg: "Audio must be base64 encoded data",
                            }
                            .build()),
                            _ => unreachable!(),
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let other_content = OneOrMany::many(other_content).map_err(|_| {
                        ConversionSnafu {
                            msg: "User message did not contain OpenAI Responses-compatible content",
                        }
                        .build()
                    })?;

                    Ok(vec![Message::User {
                        content: other_content,
                        name: None,
                    }])
                }
            }
            message::Message::Assistant { content, id } => {
                let assistant_message_id = id.ok_or_else(|| {
                    ConversionSnafu {
                        msg: "Assistant message ID is required for OpenAI Responses API",
                    }
                    .build()
                })?;

                let mut reasoning_items = Vec::new();
                let mut visible_contents = Vec::new();

                for assistant_content in content {
                    match assistant_content {
                        crate::completion::message::AssistantContent::Text(Text { text }) => {
                            visible_contents.push(AssistantContentType::Text(
                                AssistantContent::OutputText(Text { text }),
                            ));
                        }
                        crate::completion::message::AssistantContent::ToolCall(
                            crate::completion::message::ToolCall {
                                id,
                                call_id,
                                function,
                                ..
                            },
                        ) => visible_contents.push(AssistantContentType::ToolCall(
                            OutputFunctionCall {
                                call_id: call_id.ok_or_else(|| {
                                    ConversionSnafu {
                                    msg: "Tool call `call_id` is required for OpenAI Responses API",
                                }
                                .build()
                                })?,
                                arguments: function.arguments,
                                id,
                                name: function.name,
                                status: ToolStatus::Completed,
                            },
                        )),
                        crate::completion::message::AssistantContent::Reasoning(reasoning) => {
                            reasoning_items.push(Message::Assistant {
                                content: OneOrMany::one(AssistantContentType::Reasoning(
                                    openai_reasoning_from_core(&reasoning)?,
                                )),
                                id: assistant_message_id.clone(),
                                name: None,
                                status: ToolStatus::Completed,
                            });
                        }
                        crate::completion::message::AssistantContent::Image(_) => {
                            return Err(ConversionSnafu {
                                msg: "Assistant image content is not supported in OpenAI Responses API",
                            }
                            .build());
                        }
                    }
                }

                let mut assistant_items = reasoning_items;

                if !visible_contents.is_empty() {
                    assistant_items.push(Message::Assistant {
                        content: OneOrMany::many(visible_contents).map_err(|_| {
                            ConversionSnafu {
                                msg: "Assistant message did not contain OpenAI Responses-compatible content",
                            }
                            .build()
                        })?,
                        id: assistant_message_id,
                        name: None,
                        status: ToolStatus::Completed,
                    });
                }

                Ok(assistant_items)
            }
        }
    }
}

impl FromStr for UserContent {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(UserContent::InputText {
            text: s.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion;
    use crate::completion::Message as CoreMessage;
    use crate::one_or_many::OneOrMany;

    /// Builds a client bound to the Codex base URL for test coverage.
    fn codex_client() -> super::super::client::Client {
        super::super::client::Client::builder()
            .base_url(OPENAI_CODEX_API_BASE_URL)
            .api_key("test-token")
            .build()
            .expect("builder should create a codex responses client")
    }

    /// Verifies Codex mode moves system messages from input into top-level instructions.
    #[test]
    fn codex_request_moves_system_messages_to_instructions() {
        let model = super::ResponsesCompletionModel::with_model(codex_client(), "codex-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: Some("Prompt preamble".to_string()),
            chat_history: OneOrMany::many(vec![
                CoreMessage::System {
                    content: "History system".to_string(),
                },
                CoreMessage::User {
                    content: OneOrMany::one(crate::completion::message::UserContent::from(
                        "ask".to_string(),
                    )),
                },
            ])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("codex request should be valid");

        assert_eq!(
            completion_request.instructions,
            Some("Prompt preamble\n\nHistory system".to_string())
        );
        assert_eq!(completion_request.additional_parameters.store, Some(false));
        assert!(
            completion_request
                .input
                .iter()
                .all(|item| !matches!(item.role, Some(Role::System)))
        );
        assert_eq!(completion_request.input.len(), 1);
    }

    /// Verifies Codex sanitizes function-tool names before sending them.
    #[test]
    fn codex_request_sanitizes_function_tool_names() {
        let model = super::ResponsesCompletionModel::with_model(codex_client(), "codex-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![CoreMessage::User {
                content: OneOrMany::one(crate::completion::message::UserContent::from(
                    "test".to_string(),
                )),
            }])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![completion::ToolDefinition {
                name: "fs/read_text_file".to_string(),
                description: "namespaced read alias".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }],
            tool_choice: None,
            additional_params: Some(serde_json::json!({
                "tools": [
                    {
                        "type": "function",
                        "name": "write/text_file",
                        "description": "namespaced write alias",
                        "parameters": {"type": "object", "properties": {}},
                    }
                ]
            })),
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("codex request should be valid");

        let function_names = completion_request
            .tools
            .iter()
            .filter(|tool| tool.kind == "function")
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(function_names.len(), 2);
        assert!(function_names.contains(&"fs_read_text_file"));
        assert!(function_names.contains(&"write_text_file"));
        assert!(!function_names.contains(&"fs/read_text_file"));
        assert!(!function_names.contains(&"write/text_file"));
    }

    #[test]
    fn codex_request_sanitizes_function_schema_top_level_constraints() {
        let model = super::ResponsesCompletionModel::with_model(codex_client(), "codex-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![CoreMessage::User {
                content: OneOrMany::one(crate::completion::message::UserContent::from(
                    "test".to_string(),
                )),
            }])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![completion::ToolDefinition {
                name: "fs/read_text_file".to_string(),
                description: "namespaced read alias".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "session_id": { "type": "string" },
                        "sessionId": { "type": "string" },
                    },
                    "required": ["path"],
                    "anyOf": [
                        { "required": ["session_id"] },
                        { "required": ["sessionId"] }
                    ]
                }),
            }],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("codex request should be valid");

        let function_tool = completion_request
            .tools
            .into_iter()
            .find(|tool| tool.kind == "function")
            .expect("function tool should be present");

        assert!(function_tool.parameters.get("anyOf").is_none());
        assert!(function_tool.parameters.get("oneOf").is_none());
        assert!(function_tool.parameters.get("allOf").is_none());
        assert!(function_tool.parameters.get("enum").is_none());
        assert!(function_tool.parameters.get("not").is_none());

        let required = function_tool.parameters["required"]
            .as_array()
            .expect("parameters should include required array");
        assert!(required.contains(&serde_json::json!("path")));
        assert!(!required.contains(&serde_json::json!("_meta")));
        assert!(!required.contains(&serde_json::json!("sessionId")));
        assert!(!required.is_empty());

        let properties = function_tool.parameters["properties"]
            .as_object()
            .expect("parameters should include properties object");
        assert!(properties.contains_key("path"));
        assert!(!properties.contains_key("_meta"));
        assert!(!properties.contains_key("sessionId"));
        assert!(properties.contains_key("session_id"));
    }

    /// Validates recursive `_meta` stripping and required-key pruning inside nested
    /// tool schemas while preserving canonical `session_id` usage.
    #[test]
    fn codex_request_sanitizes_nested_schema_aliases() {
        let model = super::ResponsesCompletionModel::with_model(codex_client(), "codex-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![CoreMessage::User {
                content: OneOrMany::one(crate::completion::message::UserContent::from(
                    "test nested schema".to_string(),
                )),
            }])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![
                completion::ToolDefinition {
                    name: "write/text_file".to_string(),
                    description: "nested alias and _meta case".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "_meta": { "type": "object", "properties": { "mcp": {"type":"string"}}},
                            "session_id": { "type": "string" },
                            "sessionId": { "type": "string" },
                            "options": {
                                "type": "object",
                                "properties": {
                                    "_meta": { "type": "object", "properties": { "inner": {"type":"string"}}},
                                    "recursive": { "type": "boolean" },
                                },
                                "required": ["_meta", "recursive"],
                            },
                        },
                        "required": ["path", "_meta", "session_id", "sessionId", "options"],
                        "anyOf": [
                            { "required": ["session_id"] },
                            { "required": ["sessionId"] }
                        ]
                    }),
                },
                completion::ToolDefinition {
                    name: "fs/read_text_file".to_string(),
                    description: "second namespaced tool alias case".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "sessionId": { "type": "string" },
                            "session_id": { "type": "string" },
                            "path": { "type": "string" },
                            "_meta": { "type": "object", "properties": { "mcp": {"type":"string"}}},
                        },
                        "required": ["_meta", "sessionId", "session_id", "path"],
                        "anyOf": [
                            { "required": ["session_id"] },
                            { "required": ["sessionId"] }
                        ]
                    }),
                },
            ],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("codex request should be valid");

        let function_tools = completion_request
            .tools
            .into_iter()
            .filter(|tool| tool.kind == "function")
            .collect::<Vec<_>>();

        assert_eq!(function_tools.len(), 2);

        let mut function_names = function_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        function_names.sort_unstable();
        assert_eq!(function_names, vec!["fs_read_text_file", "write_text_file"]);

        for tool in function_tools {
            let properties = tool.parameters["properties"]
                .as_object()
                .expect("parameters should include properties object");

            assert!(!properties.contains_key("_meta"));
            assert!(!properties.contains_key("sessionId"));
            assert!(properties.contains_key("session_id"));

            let required = tool.parameters["required"]
                .as_array()
                .expect("parameters should include required array");
            assert!(!required.contains(&serde_json::json!("_meta")));
            assert!(!required.contains(&serde_json::json!("sessionId")));
            assert!(required.contains(&serde_json::json!("session_id")));

            let options = properties.get("options").and_then(Value::as_object);
            if let Some(options) = options {
                let nested_properties = options
                    .get("properties")
                    .and_then(Value::as_object)
                    .expect("options should include object properties");
                assert!(!nested_properties.contains_key("_meta"));

                let nested_required = options
                    .get("required")
                    .and_then(Value::as_array)
                    .expect("options should include required array when path uses nested schema");
                assert!(!nested_required.contains(&serde_json::json!("_meta")));
            }
        }
    }

    /// Verifies provider tool-call decoding canonicalizes duplicate session aliases.
    #[test]
    fn output_function_call_deserialization_normalizes_duplicate_session_aliases() {
        let output: Output = serde_json::from_value(serde_json::json!({
            "type": "function_call",
            "id": "fc_123",
            "arguments": "{\"path\":\"/tmp/poem.txt\",\"session_id\":\"sess-1\",\"sessionId\":\"sess-1\"}",
            "call_id": "call_123",
            "name": "fs_write_text_file",
            "status": "completed"
        }))
        .expect("function call output should deserialize");

        let Output::FunctionCall(function_call) = output else {
            panic!("expected function call output");
        };

        assert_eq!(
            function_call.arguments["path"],
            serde_json::json!("/tmp/poem.txt")
        );
        assert_eq!(
            function_call.arguments["session_id"],
            serde_json::json!("sess-1")
        );
        assert!(function_call.arguments.get("sessionId").is_none());
    }

    #[test]
    fn user_tool_result_with_mixed_content_flattens_to_multiple_function_call_outputs() {
        let tool_result_content = crate::one_or_many::OneOrMany::many(vec![
            crate::completion::message::ToolResultContent::text("status: ok"),
            crate::completion::message::ToolResultContent::image_url(
                "https://example.com/diagram.svg",
                Some(crate::completion::message::ImageMediaType::SVG),
                Some(crate::completion::message::ImageDetail::High),
            ),
        ])
        .expect("tool result content should be non-empty");

        let user_message = crate::completion::message::Message::User {
            content: OneOrMany::one(
                crate::completion::message::UserContent::tool_result_with_call_id(
                    "tool-msg-1",
                    "tool-call-1".to_string(),
                    tool_result_content,
                ),
            ),
        };

        let openai_items: Vec<super::Message> = user_message
            .try_into()
            .expect("tool-result user message conversion should succeed");

        assert_eq!(openai_items.len(), 2);

        assert!(matches!(
            &openai_items[0],
            Message::ToolResult {
                tool_call_id,
                output: _,
            } if tool_call_id == "tool-call-1"
        ));
        assert!(matches!(
            &openai_items[1],
            Message::ToolResult {
                tool_call_id,
                output: _,
            } if tool_call_id == "tool-call-1"
        ));

        if let Message::ToolResult { output, .. } = &openai_items[0] {
            assert_eq!(output, "status: ok");
        } else {
            panic!("expected first tool result output")
        }

        let expected_image_output =
            serde_json::to_string(&crate::completion::message::ToolResultContent::image_url(
                "https://example.com/diagram.svg",
                Some(crate::completion::message::ImageMediaType::SVG),
                Some(crate::completion::message::ImageDetail::High),
            ))
            .expect("image tool result content should serialize");

        if let Message::ToolResult { output, .. } = &openai_items[1] {
            assert_eq!(output, &expected_image_output);
        } else {
            panic!("expected second tool result output")
        }
    }

    #[test]
    fn assistant_message_with_multiple_content_items_preserves_all_content() {
        let assistant_message = crate::completion::message::Message::Assistant {
            id: Some("assistant-message-1".to_string()),
            content: OneOrMany::many(vec![
                crate::completion::message::AssistantContent::text("hello"),
                crate::completion::message::AssistantContent::tool_call_with_call_id(
                    "tool-1",
                    "assistant-call-1".to_string(),
                    "calculator",
                    serde_json::json!({"expression": "2+2"}),
                ),
            ])
            .expect("assistant content should be non-empty"),
        };

        let openai_items: Vec<super::Message> = assistant_message
            .try_into()
            .expect("assistant message conversion should succeed");

        assert_eq!(openai_items.len(), 1);

        match &openai_items[0] {
            Message::Assistant {
                content,
                id,
                status: _,
                ..
            } => {
                assert_eq!(id, "assistant-message-1");
                assert_eq!(content.len(), 2);
                match content.first_ref() {
                    AssistantContentType::Text(AssistantContent::OutputText(Text { text })) => {
                        assert_eq!(text, "hello");
                    }
                    _ => panic!("expected assistant text content as first item"),
                }

                let rest_content = content.rest();
                let tool_call = rest_content
                    .first()
                    .expect("assistant content should include tool call as second item");

                match tool_call {
                    AssistantContentType::ToolCall(OutputFunctionCall {
                        id: tool_id,
                        arguments,
                        name: tool_name,
                        ..
                    }) => {
                        assert_eq!(tool_id, "tool-1");
                        assert_eq!(tool_name, "calculator");
                        assert_eq!(arguments, &serde_json::json!({"expression": "2+2"}));
                    }
                    _ => panic!("expected assistant tool call content as second item"),
                }
            }
            _ => panic!("expected assistant message"),
        }
    }

    /// Verifies non-Codex clients preserve legacy system messages in the input payload.
    #[test]
    fn codex_request_keeps_system_items_for_non_codex_client() {
        let normal_client = super::super::client::Client::builder()
            .base_url("https://api.openai.com/v1")
            .api_key("test-token")
            .build()
            .expect("builder should create openai responses client");
        let model = super::ResponsesCompletionModel::with_model(normal_client, "openai-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: Some("Prompt preamble".to_string()),
            chat_history: OneOrMany::many(vec![
                CoreMessage::System {
                    content: "History system".to_string(),
                },
                CoreMessage::User {
                    content: OneOrMany::one(crate::completion::message::UserContent::from(
                        "ask".to_string(),
                    )),
                },
            ])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("openai request should be valid");

        assert!(completion_request.instructions.is_none());
        assert_eq!(completion_request.additional_parameters.store, None);
        assert_eq!(completion_request.input.len(), 3);
        assert!(matches!(
            completion_request.input.first_ref().role,
            Some(Role::System)
        ));
    }

    /// Verifies non-Codex responses clients keep function-tool names as-is.
    #[test]
    fn non_codex_request_keeps_unsanitized_function_tool_names() {
        let normal_client = super::super::client::Client::builder()
            .base_url("https://api.openai.com/v1")
            .api_key("test-token")
            .build()
            .expect("builder should create openai responses client");
        let model = super::ResponsesCompletionModel::with_model(normal_client, "openai-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![CoreMessage::User {
                content: OneOrMany::one(crate::completion::message::UserContent::from(
                    "test".to_string(),
                )),
            }])
            .expect("chat history should contain at least one message"),
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

        let completion_request = model
            .create_completion_request(request)
            .expect("openai request should be valid");

        let function_names = completion_request
            .tools
            .iter()
            .filter(|tool| tool.kind == "function")
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(function_names.len(), 1);
        assert!(function_names.contains(&"fs/read_text_file"));
        assert!(!function_names.contains(&"fs_read_text_file"));
    }

    /// Verifies non-Codex responses requests keep canonical namespaced session schema
    /// without alias-only combinators that would become unsatisfiable under sanitization.
    #[test]
    fn non_codex_request_keeps_canonical_namespaced_session_schema() {
        let normal_client = super::super::client::Client::builder()
            .base_url("https://api.openai.com/v1")
            .api_key("test-token")
            .build()
            .expect("builder should create openai responses client");
        let model = super::ResponsesCompletionModel::with_model(normal_client, "openai-test");
        let request = completion::CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::many(vec![CoreMessage::User {
                content: OneOrMany::one(crate::completion::message::UserContent::from(
                    "test schema".to_string(),
                )),
            }])
            .expect("chat history should contain at least one message"),
            temperature: None,
            max_tokens: None,
            tools: vec![completion::ToolDefinition {
                name: "fs/read_text_file".to_string(),
                description: "namespaced read alias".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer", "minimum": 1 },
                        "limit": { "type": "integer", "minimum": 1 },
                        "session_id": { "type": "string" }
                    },
                    "required": ["path", "session_id"]
                }),
            }],
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let completion_request = model
            .create_completion_request(request)
            .expect("openai request should be valid");

        let function_tool = completion_request
            .tools
            .into_iter()
            .find(|tool| tool.kind == "function")
            .expect("function tool should be present");

        let properties = function_tool.parameters["properties"]
            .as_object()
            .expect("parameters should include properties object");
        assert!(properties.contains_key("path"));
        assert!(properties.contains_key("session_id"));
        assert!(!properties.contains_key("sessionId"));
        assert!(!properties.contains_key("_meta"));
        assert!(function_tool.parameters.get("anyOf").is_none());

        let required = function_tool.parameters["required"]
            .as_array()
            .expect("parameters should include required array");
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("session_id")));
        assert!(!required.contains(&serde_json::json!("sessionId")));
    }
}

use serde::{Deserialize, Serialize};

use crate::{
    ExtensionId, MessageId, StopReason, TimestampMs, ToolCallId,
    ToolResultDetails, TurnId, UserBashResult,
};

/// Serde adapter for precision-safe unsigned decimal token counts.
mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    /// Serializes an unsigned token count as a decimal string.
    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    /// Deserializes an unsigned token count from an exact decimal string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(D::Error::custom(
                "token count must be an unsigned decimal string",
            ));
        }

        value.parse::<u64>().map_err(D::Error::custom)
    }
}

/// Serde adapter for optional precision-safe unsigned decimal token counts.
mod optional_decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    /// Serializes an optional token count as a decimal string when present.
    pub fn serialize<S>(
        value: &Option<u64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    /// Deserializes an optional token count from an exact decimal string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|value| {
                if value.is_empty()
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(D::Error::custom(
                        "token count must be an unsigned decimal string",
                    ));
                }

                value.parse::<u64>().map_err(D::Error::custom)
            })
            .transpose()
    }
}

/// Identifies a message and the turn that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageIdentity {
    /// Stable message identifier.
    pub message_id: MessageId,

    /// Turn that produced or consumed the message.
    pub turn_id: TurnId,
}

/// Complete timing data persisted for one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageTiming {
    /// Message creation timestamp in Unix milliseconds.
    pub timestamp_ms: TimestampMs,

    /// First-token or immediate-message timestamp in Unix milliseconds.
    pub started_at_ms: TimestampMs,

    /// Last-token or immediate-message timestamp in Unix milliseconds.
    pub ended_at_ms: TimestampMs,
}

/// Reports an invalid persisted message interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MessageTimingError {
    /// The message ended before its first token or immediate start.
    #[error("message ended at {ended} before it started at {started}")]
    EndBeforeStart {
        /// First-token or immediate-message timestamp.
        started: TimestampMs,

        /// Last-token or immediate-message timestamp.
        ended: TimestampMs,
    },
}

impl TryFrom<(TimestampMs, TimestampMs, TimestampMs)> for MessageTiming {
    type Error = MessageTimingError;

    /// Builds complete timing data while enforcing chronological ordering.
    fn try_from(
        (timestamp_ms, started_at_ms, ended_at_ms): (
            TimestampMs,
            TimestampMs,
            TimestampMs,
        ),
    ) -> Result<Self, Self::Error> {
        if ended_at_ms < started_at_ms {
            return Err(MessageTimingError::EndBeforeStart {
                started: started_at_ms,
                ended: ended_at_ms,
            });
        }

        Ok(Self {
            timestamp_ms,
            started_at_ms,
            ended_at_ms,
        })
    }
}

/// Role of a standard model-visible message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System instruction message.
    System,
    /// User-authored message.
    User,
    /// Assistant-authored message.
    Assistant,
}

/// Complete provider token accounting for one assistant attempt.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ModelUsage {
    /// Tokens supplied directly to the model.
    #[serde(with = "decimal_u64")]
    pub input_tokens: u64,
    /// Tokens emitted by the model.
    #[serde(with = "decimal_u64")]
    pub output_tokens: u64,
    /// Provider cache tokens read for this attempt.
    #[serde(with = "decimal_u64")]
    pub cache_read_tokens: u64,
    /// Provider cache tokens written for this attempt.
    #[serde(with = "decimal_u64")]
    pub cache_write_tokens: u64,
    /// Reasoning tokens reported separately by providers that expose them.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_decimal_u64"
    )]
    #[builder(default)]
    pub reasoning_tokens: Option<u64>,
    /// Provider-reported total token consumption.
    #[serde(with = "decimal_u64")]
    pub total_tokens: u64,
}

/// Complete model outcome attached only to assistant messages.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct AssistantMetadata {
    /// Configuration identifier of the provider that handled the attempt.
    pub provider_id: String,
    /// Provider-facing model identifier used for the attempt.
    pub model_id: String,
    /// Kernel-normalized reason that model generation stopped.
    pub stop_reason: StopReason,
    /// Original provider stop reason retained for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub raw_stop_reason: Option<String>,
    /// Complete token usage reported for the attempt.
    pub usage: ModelUsage,
    /// Human-readable terminal error when the assistant attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub error: Option<String>,
}

/// Text or binary payload embedded by an MCP Resource content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EmbeddedResourceContent {
    /// UTF-8 resource text.
    Text {
        /// Complete text payload.
        text: String,
    },
    /// Base64-encoded resource bytes.
    Blob {
        /// Base64 payload without a data-URL prefix.
        data: String,
    },
}

/// One ordered content block carried by a standard message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain UTF-8 text.
    Text {
        /// Text content.
        text: String,
    },

    /// Model reasoning that adapters may hide or render separately.
    Reasoning {
        /// Reasoning text or summary.
        text: String,
    },

    /// Base64-encoded image returned by a user or tool operation.
    Image {
        /// Base64 media payload without a data-URL prefix.
        data: String,

        /// MIME type describing the encoded image payload.
        mime_type: String,
    },

    /// Base64-encoded audio returned by a user, model, or Tool operation.
    Audio {
        /// Base64 media payload without a data-URL prefix.
        data: String,

        /// MIME type describing the encoded audio payload.
        mime_type: String,
    },

    /// Complete text or binary Resource embedded in the message.
    EmbeddedResource {
        /// Original Resource URI used for routing and provenance.
        uri: String,

        /// Optional MIME type declared by the MCP Server.
        mime_type: Option<String>,

        /// Typed text or binary Resource payload.
        content: EmbeddedResourceContent,
    },

    /// Link to a Resource that remains available from its owning MCP Server.
    ResourceLink {
        /// Original Resource URI.
        uri: String,

        /// Server-provided Resource name.
        name: String,

        /// Optional user-facing title.
        title: Option<String>,

        /// Optional Resource description.
        description: Option<String>,

        /// Optional declared MIME type.
        mime_type: Option<String>,

        /// Optional Resource size in bytes.
        size: Option<u64>,
    },

    /// Structured JSON returned alongside displayable MCP content.
    Structured {
        /// Complete JSON value preserved without a text conversion.
        value: serde_json::Value,
    },

    /// A tool invocation requested by the assistant.
    ToolCall {
        /// Stable tool call identifier.
        tool_call_id: ToolCallId,

        /// Registered tool name.
        name: String,

        /// JSON arguments supplied by the model.
        arguments: serde_json::Value,
    },
}

impl ContentBlock {
    /// Returns plain text content when this block is a text block.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            Self::Reasoning { .. }
            | Self::Image { .. }
            | Self::Audio { .. }
            | Self::EmbeddedResource { .. }
            | Self::ResourceLink { .. }
            | Self::Structured { .. }
            | Self::ToolCall { .. } => None,
        }
    }
}

/// Typed custom message emitted by a pi-compatible extension.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ExtensionMessage {
    /// Extension that owns the custom message schema.
    pub extension_id: ExtensionId,
    /// Extension-defined message subtype.
    pub custom_type: String,
    /// Ordered message content.
    pub blocks: Vec<ContentBlock>,
    /// Whether clients should render this message.
    pub display: bool,
    /// Whether this message is projected into future model context.
    pub include_in_context: bool,
    /// Extension-owned structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub details: Option<serde_json::Value>,
}

/// Pi-compatible transcript message produced by a user-authored `!` or `!!` command.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    /// Shell command after removing the user-facing prefix.
    pub command: String,
    /// Complete server-side execution outcome.
    pub result: UserBashResult,
    /// Whether `!!` excluded this message from future model context.
    pub exclude_from_context: bool,
}

impl BashExecutionMessage {
    /// Converts one included bash execution into Pi's model-facing user message text.
    #[must_use]
    pub fn model_text(&self) -> String {
        let mut text = format!("Ran `{}`\n", self.command);
        if self.result.output.is_empty() {
            text.push_str("(no output)");
        } else {
            text.push_str("```\n");
            text.push_str(&self.result.output);
            text.push_str("\n```");
        }
        if self.result.cancelled {
            text.push_str("\n\n(command cancelled)");
        } else if let Some(code) = self.result.exit_code
            && code != 0
        {
            text.push_str(&format!("\n\nCommand exited with code {code}"));
        }
        if self.result.truncated
            && let Some(path) = &self.result.full_output_path
        {
            text.push_str(&format!(
                "\n\n[Output truncated. Full output: {path}]"
            ));
        }
        text
    }
}

/// Content variants supported by the agent-domain transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// System instructions assembled for the active run.
    System {
        /// Ordered multimodal or tool-call content blocks.
        blocks: Vec<ContentBlock>,
    },

    /// User-authored model input.
    User {
        /// Ordered multimodal content blocks.
        blocks: Vec<ContentBlock>,
    },

    /// Complete assistant output with its provider result metadata.
    Assistant {
        /// Ordered text, reasoning, and tool-call content blocks.
        blocks: Vec<ContentBlock>,
        /// Provider, stop, usage, and error details for this attempt.
        metadata: AssistantMetadata,
    },

    /// Result returned for a tool call.
    ToolResult {
        /// Tool call being answered.
        tool_call_id: ToolCallId,

        /// Tool output represented as ordered content blocks.
        blocks: Vec<ContentBlock>,

        /// Whether the tool failed.
        is_error: bool,

        /// Optional typed diagnostics retained for replay and ACP rendering.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<ToolResultDetails>,
    },

    /// User-authored server-side shell execution retained with Pi `!`/`!!` semantics.
    BashExecution {
        /// Command, result, and context-exclusion state.
        bash: BashExecutionMessage,
    },

    /// A pi-compatible custom message with no native ACP counterpart.
    Extension {
        /// Opaque extension message.
        extension: ExtensionMessage,
    },
}

impl MessageContent {
    /// Returns the model-facing role for standard transcript messages.
    #[must_use]
    pub const fn role(&self) -> Option<Role> {
        match self {
            Self::System { .. } => Some(Role::System),
            Self::User { .. } => Some(Role::User),
            Self::Assistant { .. } => Some(Role::Assistant),
            Self::ToolResult { .. }
            | Self::BashExecution { .. }
            | Self::Extension { .. } => None,
        }
    }

    /// Joins plain text blocks for consumers that need a textual projection.
    #[must_use]
    pub fn text_content(&self) -> Option<String> {
        let blocks = match self {
            Self::System { blocks }
            | Self::User { blocks }
            | Self::Assistant { blocks, .. }
            | Self::ToolResult { blocks, .. } => blocks,
            Self::BashExecution { bash } => return Some(bash.model_text()),
            Self::Extension { .. } => return None,
        };
        let text = blocks
            .iter()
            .filter_map(ContentBlock::text)
            .collect::<Vec<_>>()
            .join("");

        (!text.is_empty()).then_some(text)
    }
}

/// Complete persisted message with mandatory turn and millisecond timing data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Stable message and owning-turn identity flattened onto the wire envelope.
    #[serde(flatten)]
    pub identity: MessageIdentity,

    /// Exact complete-message timing flattened onto the wire envelope.
    #[serde(flatten)]
    pub timing: MessageTiming,

    /// Typed or opaque message content.
    pub content: MessageContent,
}

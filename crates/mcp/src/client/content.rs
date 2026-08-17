#![expect(
    deprecated,
    reason = "Legacy Sampling content remains required by the supported protocol"
)]

use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, EmbeddedResourceContent,
    McpRequestContext, McpServerId, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelUsage, Role, StopReason, TimestampMs, ToolCallId,
};
use rmcp::model::{self as wire, ResourceContents};

use crate::McpError;

/// Converts rmcp wire content into the shared lossless protocol representation.
pub(super) trait IntoProtocolContent {
    /// Performs one lossless content conversion for the owning Server.
    fn into_protocol(
        self,
        server_id: &McpServerId,
    ) -> Result<ContentBlock, McpError>;
}

impl IntoProtocolContent for wire::ContentBlock {
    /// Maps every content form shared by the selected MCP protocol revisions.
    fn into_protocol(
        self,
        server_id: &McpServerId,
    ) -> Result<ContentBlock, McpError> {
        match self {
            Self::Text(content) => {
                Ok(ContentBlock::Text { text: content.text })
            }
            Self::Image(content) => Ok(ContentBlock::Image {
                data: content.data,
                mime_type: content.mime_type,
            }),
            Self::Audio(content) => Ok(ContentBlock::Audio {
                data: content.data,
                mime_type: content.mime_type,
            }),
            Self::Resource(content) => {
                content.resource.into_protocol(server_id)
            }
            Self::ResourceLink(resource) => Ok(ContentBlock::ResourceLink {
                uri: resource.uri,
                name: resource.name,
                title: resource.title,
                description: resource.description,
                mime_type: resource.mime_type,
                size: resource.size,
            }),
            _ => Err(McpError::UnsupportedContent {
                server_id: server_id.clone(),
            }),
        }
    }
}

impl IntoProtocolContent for ResourceContents {
    /// Preserves Resource URI, MIME type, and text or base64 payload.
    fn into_protocol(
        self,
        _server_id: &McpServerId,
    ) -> Result<ContentBlock, McpError> {
        match self {
            Self::TextResourceContents {
                uri,
                mime_type,
                text,
                ..
            } => Ok(ContentBlock::EmbeddedResource {
                uri,
                mime_type,
                content: EmbeddedResourceContent::Text { text },
            }),
            Self::BlobResourceContents {
                uri,
                mime_type,
                blob,
                ..
            } => Ok(ContentBlock::EmbeddedResource {
                uri,
                mime_type,
                content: EmbeddedResourceContent::Blob { data: blob },
            }),
            _ => Err(McpError::UnsupportedContent {
                server_id: _server_id.clone(),
            }),
        }
    }
}

/// Converts one rmcp Prompt role into the shared model-facing role.
pub(super) trait IntoProtocolRole {
    /// Maps the two MCP Prompt roles without inventing a system role.
    fn into_protocol(self) -> Role;
}

/// Converts one wire Sampling message into a fully correlated agent message.
pub(super) trait IntoProtocolSamplingMessage {
    /// Preserves role and supported content while assigning Turn identity and timing.
    fn into_protocol_sampling(
        self,
        context: &McpRequestContext,
        message_id: MessageId,
    ) -> Result<AgentMessage, McpError>;
}

impl IntoProtocolSamplingMessage for wire::SamplingMessage {
    /// Converts Sampling content without adding it to the normal Session transcript.
    fn into_protocol_sampling(
        self,
        context: &McpRequestContext,
        message_id: MessageId,
    ) -> Result<AgentMessage, McpError> {
        let blocks = self
            .content
            .into_vec()
            .into_iter()
            .map(|block| match block {
                wire::SamplingMessageContentBlock::Text(content) => {
                    Ok(ContentBlock::Text { text: content.text })
                }
                wire::SamplingMessageContentBlock::Image(content) => {
                    Ok(ContentBlock::Image {
                        data: content.data,
                        mime_type: content.mime_type,
                    })
                }
                wire::SamplingMessageContentBlock::Audio(content) => {
                    Ok(ContentBlock::Audio {
                        data: content.data,
                        mime_type: content.mime_type,
                    })
                }
                wire::SamplingMessageContentBlock::ToolUse(content) => {
                    Ok(ContentBlock::ToolCall {
                        tool_call_id: ToolCallId::try_from(content.id)
                            .map_err(|error| {
                                McpError::Host(error.to_string())
                            })?,
                        name: content.name,
                        arguments: serde_json::Value::Object(content.input),
                    })
                }
                wire::SamplingMessageContentBlock::ToolResult(content) => {
                    serde_json::to_value(content)
                        .map(|value| ContentBlock::Structured { value })
                        .map_err(|error| McpError::Host(error.to_string()))
                }
                _ => Err(McpError::UnsupportedContent {
                    server_id: context.server_id.clone(),
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let timestamp = TimestampMs::now();
        let content = match self.role.into_protocol() {
            Role::User => MessageContent::User { blocks },
            Role::Assistant => MessageContent::Assistant {
                blocks,
                metadata: AssistantMetadata::builder()
                    .provider_id("mcp-host-request".to_string())
                    .model_id("server-supplied".to_string())
                    .stop_reason(StopReason::EndTurn)
                    .usage(
                        ModelUsage::builder()
                            .input_tokens(0)
                            .output_tokens(0)
                            .cache_read_tokens(0)
                            .cache_write_tokens(0)
                            .total_tokens(0)
                            .build(),
                    )
                    .build(),
            },
            Role::System => unreachable!("wire Sampling has no System role"),
        };
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id,
                turn_id: context.turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| McpError::Host(error.to_string()))?,
            content,
        })
    }
}

/// Converts protocol Sampling output blocks back into the rmcp wire subset.
pub(super) trait IntoWireSamplingContent {
    /// Preserves every content kind representable in Sampling responses.
    fn into_wire_sampling(
        self,
        server_id: &McpServerId,
    ) -> Result<wire::SamplingMessageContentBlock, McpError>;
}

impl IntoWireSamplingContent for ContentBlock {
    /// Converts one public block or rejects content absent from Sampling's wire union.
    fn into_wire_sampling(
        self,
        server_id: &McpServerId,
    ) -> Result<wire::SamplingMessageContentBlock, McpError> {
        match self {
            Self::Text { text } => {
                Ok(wire::SamplingMessageContentBlock::text(text))
            }
            Self::Image { data, mime_type } => {
                Ok(wire::SamplingMessageContentBlock::Image(
                    wire::ImageContent::new(data, mime_type),
                ))
            }
            Self::Audio { data, mime_type } => {
                Ok(wire::SamplingMessageContentBlock::Audio(
                    wire::AudioContent::new(data, mime_type),
                ))
            }
            Self::ToolCall {
                tool_call_id,
                name,
                arguments,
            } => {
                let input =
                    arguments.as_object().cloned().ok_or_else(|| {
                        McpError::Host(
                            "Sampling Tool call arguments must be an object"
                                .to_string(),
                        )
                    })?;
                Ok(wire::SamplingMessageContentBlock::tool_use(
                    tool_call_id.to_string(),
                    name,
                    input,
                ))
            }
            Self::Reasoning { .. }
            | Self::EmbeddedResource { .. }
            | Self::ResourceLink { .. }
            | Self::Structured { .. } => Err(McpError::UnsupportedContent {
                server_id: server_id.clone(),
            }),
        }
    }
}

impl IntoProtocolRole for wire::Role {
    /// Converts one wire Prompt role.
    fn into_protocol(self) -> Role {
        match self {
            Self::User => Role::User,
            Self::Assistant => Role::Assistant,
        }
    }
}

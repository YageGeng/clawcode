use std::collections::BTreeMap;

use agent_client_protocol::schema::v2 as wire;
use protocol::{ContentBlock, EmbeddedResourceContent, ProductIdentity};

/// Converts shared content blocks into native or extended ACP v2 blocks.
pub(super) struct AcpContentMapper;

impl AcpContentMapper {
    /// Maps one displayable block while leaving reasoning and Tool Calls to dedicated updates.
    pub(super) fn message(
        block: &ContentBlock,
        metadata: &wire::Meta,
    ) -> Result<Option<wire::ContentBlock>, serde_json::Error> {
        let mapped = match block {
            ContentBlock::Text { text } => Some(wire::ContentBlock::Text(
                wire::TextContent::new(text).meta(metadata.clone()),
            )),
            ContentBlock::Image { data, mime_type } => {
                Some(wire::ContentBlock::Image(
                    wire::ImageContent::new(data, mime_type)
                        .meta(metadata.clone()),
                ))
            }
            ContentBlock::Audio { data, mime_type } => {
                Some(wire::ContentBlock::Audio(
                    wire::AudioContent::new(data, mime_type)
                        .meta(metadata.clone()),
                ))
            }
            ContentBlock::EmbeddedResource {
                uri,
                mime_type,
                content,
            } => {
                let resource = match content {
                    EmbeddedResourceContent::Text { text } => {
                        wire::EmbeddedResourceResource::TextResourceContents(
                            wire::TextResourceContents::new(text, uri)
                                .mime_type(
                                    mime_type
                                        .as_deref()
                                        .map(wire::MediaType::new),
                                )
                                .meta(metadata.clone()),
                        )
                    }
                    EmbeddedResourceContent::Blob { data } => {
                        wire::EmbeddedResourceResource::BlobResourceContents(
                            wire::BlobResourceContents::new(data, uri)
                                .mime_type(
                                    mime_type
                                        .as_deref()
                                        .map(wire::MediaType::new),
                                )
                                .meta(metadata.clone()),
                        )
                    }
                };
                Some(wire::ContentBlock::Resource(
                    wire::EmbeddedResource::new(resource)
                        .meta(metadata.clone()),
                ))
            }
            ContentBlock::ResourceLink {
                uri,
                name,
                title,
                description,
                mime_type,
                size,
            } => Some(wire::ContentBlock::ResourceLink(
                wire::ResourceLink::new(name, uri)
                    .title(title.clone())
                    .description(description.clone())
                    .mime_type(mime_type.as_deref().map(wire::MediaType::new))
                    .size(size.and_then(|value| i64::try_from(value).ok()))
                    .meta(metadata.clone()),
            )),
            ContentBlock::Structured { value } => {
                let fields = BTreeMap::from([
                    ("value".to_string(), value.clone()),
                    ("_meta".to_string(), serde_json::to_value(metadata)?),
                ]);
                Some(wire::ContentBlock::Other(wire::OtherContentBlock::new(
                    ProductIdentity::ACP_STRUCTURED_CONTENT_BLOCK,
                    fields,
                )))
            }
            ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {
                None
            }
        };
        Ok(mapped)
    }

    /// Wraps one displayable message block for ACP Tool progress and results.
    pub(super) fn tool_result(
        block: &ContentBlock,
        metadata: &wire::Meta,
    ) -> Result<Option<wire::ToolCallContent>, serde_json::Error> {
        Ok(Self::message(block, metadata)?.map(|block| {
            wire::ToolCallContent::Content(Box::new(
                wire::Content::new(block).meta(metadata.clone()),
            ))
        }))
    }
}

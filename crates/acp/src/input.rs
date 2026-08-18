//! ACP prompt validation and conversion into Kernel input.

use agent_client_protocol::schema::v2 as wire;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

const MAX_IMAGE_COUNT: usize = 5;
pub(crate) const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_ENCODED_IMAGE_BYTES: usize =
    MAX_IMAGE_BYTES.div_ceil(3) * 4;
const MAX_TOTAL_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Validated ACP prompt content ready for one Kernel run or queued follow-up.
pub(crate) struct PromptInput(protocol::RunInput);

impl PromptInput {
    /// Consumes the validated prompt into the shared Kernel input type.
    pub(crate) fn into_inner(self) -> protocol::RunInput {
        self.0
    }
}

impl TryFrom<Vec<wire::ContentBlock>> for PromptInput {
    type Error = PromptInputError;

    /// Preserves ordered text and image content while enforcing image limits.
    fn try_from(blocks: Vec<wire::ContentBlock>) -> Result<Self, Self::Error> {
        let mut model_blocks = Vec::with_capacity(blocks.len());
        let mut text_only = true;
        let mut image_count = 0_usize;
        let mut total_image_bytes = 0_usize;
        for block in blocks {
            match block {
                wire::ContentBlock::Text(text) => {
                    model_blocks
                        .push(protocol::ContentBlock::Text { text: text.text });
                }
                wire::ContentBlock::Image(image) => {
                    text_only = false;
                    image_count = image_count.saturating_add(1);
                    if image_count > MAX_IMAGE_COUNT {
                        tracing::warn!(
                            "rejecting ACP prompt with {} images because the maximum is {}",
                            image_count,
                            MAX_IMAGE_COUNT
                        );
                        return Err(PromptInputError::TooManyImages);
                    }
                    let mime_type = image.mime_type.as_ref();
                    if !matches!(
                        mime_type,
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ) {
                        tracing::warn!(
                            "rejecting ACP prompt image {} with unsupported MIME type {}",
                            image_count,
                            mime_type
                        );
                        return Err(
                            PromptInputError::UnsupportedImageMediaType(
                                mime_type.to_string(),
                            ),
                        );
                    }
                    if image.data.len() > MAX_ENCODED_IMAGE_BYTES {
                        tracing::warn!(
                            "rejecting ACP prompt image {} because its encoded payload cannot fit within {} decoded bytes",
                            image_count,
                            MAX_IMAGE_BYTES
                        );
                        return Err(PromptInputError::ImageTooLarge);
                    }
                    let decoded = BASE64_STANDARD.decode(image.data.as_bytes()).map_err(
                        |error| {
                            tracing::warn!(
                                "rejecting ACP prompt image {} because its base64 payload is invalid: {}",
                                image_count,
                                error
                            );
                            PromptInputError::InvalidImageData(error)
                        },
                    )?;
                    if decoded.len() > MAX_IMAGE_BYTES {
                        tracing::warn!(
                            "rejecting ACP prompt image {} because it contains {} decoded bytes and the maximum is {}",
                            image_count,
                            decoded.len(),
                            MAX_IMAGE_BYTES
                        );
                        return Err(PromptInputError::ImageTooLarge);
                    }
                    total_image_bytes = total_image_bytes
                        .checked_add(decoded.len())
                        .ok_or_else(|| {
                            tracing::warn!(
                                "rejecting ACP prompt because its decoded image byte total overflowed"
                            );
                            PromptInputError::TotalImagesTooLarge
                        })?;
                    if total_image_bytes > MAX_TOTAL_IMAGE_BYTES {
                        tracing::warn!(
                            "rejecting ACP prompt because its images contain {} decoded bytes and the maximum is {}",
                            total_image_bytes,
                            MAX_TOTAL_IMAGE_BYTES
                        );
                        return Err(PromptInputError::TotalImagesTooLarge);
                    }
                    model_blocks.push(protocol::ContentBlock::Image {
                        data: image.data,
                        mime_type: mime_type.to_string(),
                    });
                }
                wire::ContentBlock::ResourceLink(resource) => {
                    text_only = false;
                    model_blocks.push(protocol::ContentBlock::Text {
                        text: format!("[{}]({})", resource.name, resource.uri),
                    });
                }
                wire::ContentBlock::Other(_) => {
                    tracing::warn!(
                        "rejecting custom or future ACP prompt content without an advertised capability"
                    );
                    return Err(PromptInputError::UnsupportedContent);
                }
                wire::ContentBlock::Audio(_)
                | wire::ContentBlock::Resource(_) => {
                    tracing::warn!(
                        "rejecting ACP prompt content that requires an unadvertised capability"
                    );
                    return Err(PromptInputError::UnsupportedContent);
                }
                _ => {
                    tracing::warn!(
                        "rejecting an unknown ACP prompt content variant"
                    );
                    return Err(PromptInputError::UnsupportedContent);
                }
            }
        }
        if model_blocks.is_empty() {
            tracing::warn!("rejecting an empty ACP prompt");
            return Err(PromptInputError::Empty);
        }
        if text_only {
            let text = model_blocks
                .into_iter()
                .filter_map(|block| match block {
                    protocol::ContentBlock::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            Ok(Self(protocol::RunInput::Text(text)))
        } else {
            Ok(Self(protocol::RunInput::Blocks(model_blocks)))
        }
    }
}

/// Validation failures for client-supplied ACP prompt blocks.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PromptInputError {
    /// No supported content was supplied.
    #[error("prompt must contain at least one supported content block")]
    Empty,
    /// A content type was not advertised by this agent.
    #[error(
        "prompt content requires a capability this agent did not advertise"
    )]
    UnsupportedContent,
    /// The image MIME type is outside the supported raster formats.
    #[error("unsupported image MIME type: {0}")]
    UnsupportedImageMediaType(String),
    /// The image payload is not standard base64.
    #[error("image data is not valid base64: {0}")]
    InvalidImageData(base64::DecodeError),
    /// More than five images were supplied.
    #[error("prompt may contain at most {MAX_IMAGE_COUNT} images")]
    TooManyImages,
    /// One decoded image exceeded ten MiB.
    #[error("each decoded image may contain at most {MAX_IMAGE_BYTES} bytes")]
    ImageTooLarge,
    /// The aggregate decoded images exceeded twenty MiB.
    #[error(
        "decoded images may contain at most {MAX_TOTAL_IMAGE_BYTES} bytes in total"
    )]
    TotalImagesTooLarge,
}

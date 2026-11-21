//! Image attachments of the composer: the server's limits, checked before
//! sending, and the prompt blocks they become (`crates/acp/src/input.rs`).

use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;
use base64::Engine as _;

/// Most images one message may carry.
const MAX_IMAGES: usize = 5;
/// Largest single image, in bytes.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Largest total of one message's images, in bytes.
const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;

/// One image waiting to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    /// File name shown on the chip.
    pub name: String,
    /// MIME type detected from the bytes.
    pub mime: &'static str,
    /// The decodable image, built once: rendering the chip reuses it.
    pub image: Arc<gpui_kit::Image>,
}

/// Why an image was not attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentError {
    /// Not a PNG, JPEG, GIF, or WebP image.
    UnsupportedFormat,
    /// Larger than 10 MiB.
    TooLarge,
    /// The message already has five images.
    TooMany,
    /// The images together would exceed 20 MiB.
    TotalTooLarge,
}

/// The images of one composer, in attach order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentSet {
    items: Vec<ImageAttachment>,
}

impl AttachmentSet {
    /// Adds an image after checking its format (by its bytes, not its name) and the limits.
    pub fn add(
        &mut self,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<(), AttachmentError> {
        let (mime, format) = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Png) => {
                ("image/png", gpui_kit::ImageFormat::Png)
            }
            Ok(image::ImageFormat::Jpeg) => {
                ("image/jpeg", gpui_kit::ImageFormat::Jpeg)
            }
            Ok(image::ImageFormat::Gif) => {
                ("image/gif", gpui_kit::ImageFormat::Gif)
            }
            Ok(image::ImageFormat::WebP) => {
                ("image/webp", gpui_kit::ImageFormat::Webp)
            }
            _ => return Err(AttachmentError::UnsupportedFormat),
        };
        self.admits(u64::try_from(bytes.len()).unwrap_or(u64::MAX))?;
        if self.items.len() >= MAX_IMAGES {
            return Err(AttachmentError::TooMany);
        }
        self.items.push(ImageAttachment {
            name,
            mime,
            image: Arc::new(gpui_kit::Image::from_bytes(format, bytes)),
        });
        Ok(())
    }

    /// Whether an image of `len` bytes would fit the size limits, checked before reading a file.
    pub fn admits(&self, len: u64) -> Result<(), AttachmentError> {
        let as_u64 = |bytes: usize| u64::try_from(bytes).unwrap_or(u64::MAX);
        if len > as_u64(MAX_IMAGE_BYTES) {
            return Err(AttachmentError::TooLarge);
        }
        let total: u64 = self
            .items
            .iter()
            .map(|item| as_u64(item.image.bytes().len()))
            .sum();
        if total.saturating_add(len) > as_u64(MAX_TOTAL_BYTES) {
            return Err(AttachmentError::TotalTooLarge);
        }
        Ok(())
    }

    /// The images, in attach order.
    pub fn items(&self) -> &[ImageAttachment] {
        &self.items
    }

    /// Number of images.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are no images.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drops the image at `index`; out of range does nothing.
    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }

    /// Prompt blocks of every image: standard base64 without a data-URL prefix.
    pub fn into_blocks(self) -> Vec<wire::ContentBlock> {
        self.items
            .into_iter()
            .map(|item| {
                let data = base64::engine::general_purpose::STANDARD
                    .encode(item.image.bytes());
                wire::ContentBlock::Image(wire::ImageContent::new(
                    data, item.mime,
                ))
            })
            .collect()
    }
}

/// A message leaving the composer: its text and images.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outgoing {
    /// The text exactly as typed; may be empty when images carry the message.
    pub text: String,
    /// Attached images.
    pub images: AttachmentSet,
}

impl Outgoing {
    /// Prompt blocks: the text (when not empty), then the images.
    pub fn into_blocks(self) -> Vec<wire::ContentBlock> {
        let mut blocks = Vec::new();
        if !self.text.is_empty() {
            blocks.push(wire::ContentBlock::Text(wire::TextContent::new(
                self.text,
            )));
        }
        blocks.extend(self.images.into_blocks());
        blocks
    }
}

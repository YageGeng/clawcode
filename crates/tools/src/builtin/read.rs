use async_trait::async_trait;
use base64::Engine as _;
use protocol::{
    ContentBlock, ToolCall, ToolDefinition, ToolPromptContribution, ToolResult,
    ToolResultDetails, TruncationLimit,
};
use serde::Deserialize;

use super::path::ResolvedPath;
use crate::{
    AgentTool, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, ToolError,
    ToolExecutionContext, format_size, truncate_head,
};

#[derive(Debug, Deserialize)]
struct ReadArguments {
    path: String,
    offset: Option<f64>,
    limit: Option<f64>,
}

impl TryFrom<&ToolCall> for ReadArguments {
    type Error = ToolError;

    /// Decodes one read call without consuming its execution correlation.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl ReadArguments {
    /// Converts pi's 1-based number offset into a safe zero-based line index.
    fn start_line(&self) -> Result<usize, ToolError> {
        let Some(offset) = self.offset else {
            return Ok(0);
        };
        if !offset.is_finite() {
            return Err(ToolError::InvalidArguments {
                tool: "read".to_string(),
                message: "offset must be a finite number".to_string(),
            });
        }
        if offset <= 1.0 {
            return Ok(0);
        }
        // Rust's float-to-integer cast saturates after the finite, positive
        // range check, matching JavaScript slice-index clamping in pi.
        #[allow(clippy::cast_sign_loss)]
        Ok((offset.floor() - 1.0) as usize)
    }

    /// Converts pi's optional number limit into a non-negative complete-line count.
    fn line_limit(&self) -> Result<Option<usize>, ToolError> {
        let Some(limit) = self.limit else {
            return Ok(None);
        };
        if !limit.is_finite() {
            return Err(ToolError::InvalidArguments {
                tool: "read".to_string(),
                message: "limit must be a finite number".to_string(),
            });
        }
        Ok(Some(if limit <= 0.0 {
            0
        } else {
            // The positive branch makes sign loss impossible; oversized
            // values saturate to usize::MAX and are later clamped by slicing.
            #[allow(clippy::cast_sign_loss)]
            {
                limit.floor() as usize
            }
        }))
    }
}

/// Pi-compatible local file and image reader.
pub(super) struct ReadTool;

#[async_trait]
impl AgentTool for ReadTool {
    /// Returns pi's read schema and truncation contract.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_string(),
            description: format!(
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
                DEFAULT_MAX_BYTES / 1_024
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)"
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (1-indexed)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    /// Contributes pi's read capability and file-inspection guideline.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some("Read file contents".to_string()),
            guidelines: vec![
                "Use read to examine files instead of cat or sed.".to_string(),
            ],
        }
    }

    /// Validates read JSON and numeric pagination without filesystem access.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        let arguments = ReadArguments::try_from(call)?;
        arguments.start_line()?;
        arguments.line_limit()?;
        Ok(())
    }

    /// Reads a text page or returns a base64 image attachment detected by magic bytes.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let arguments = ReadArguments::try_from(&call)?;
        let path =
            ResolvedPath::for_read(&arguments.path, &context.cwd).await?;
        let bytes = tokio::fs::read(path.as_path()).await.map_err(|error| {
            ToolError::Execution {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })?;
        context.ensure_active()?;

        if let Some(kind) = ImageKind::detect(&bytes) {
            let original_mime_type = kind.mime_type();
            let processed = kind.process(bytes).await;
            let blocks = match processed {
                Some(processed) => {
                    let mut note =
                        format!("Read image file [{}]", processed.mime_type);
                    if !processed.hints.is_empty() {
                        note.push('\n');
                        note.push_str(&processed.hints.join("\n"));
                    }
                    vec![
                        ContentBlock::Text { text: note },
                        ContentBlock::Image {
                            data: processed.data,
                            mime_type: processed.mime_type,
                        },
                    ]
                }
                None => vec![ContentBlock::Text {
                    text: format!(
                        "Read image file [{original_mime_type}]\n[Image omitted: could not be converted to a supported inline image format.]"
                    ),
                }],
            };
            return Ok(ToolResult::builder()
                .tool_call_id(call.tool_call_id)
                .blocks(blocks)
                .is_error(false)
                .build());
        }

        let text = String::from_utf8_lossy(&bytes);
        let lines = text.split('\n').collect::<Vec<_>>();
        let total_file_lines = lines.len();
        let start_line = arguments.start_line()?;
        if start_line >= total_file_lines {
            return Err(ToolError::Execution {
                tool: call.name,
                message: format!(
                    "Offset {} is beyond end of file ({total_file_lines} lines total)",
                    arguments.offset.unwrap_or_default()
                ),
            });
        }
        let limit = arguments.line_limit()?;
        let end_line = limit
            .map(|limit| start_line.saturating_add(limit).min(total_file_lines))
            .unwrap_or(total_file_lines);
        let selected = lines
            .get(start_line..end_line)
            .unwrap_or_default()
            .join("\n");
        let truncation =
            truncate_head(&selected, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
        let start_display = start_line + 1;
        let (output, details) = if truncation.first_line_exceeds_limit {
            let first_line_size = lines
                .get(start_line)
                .map(|line| format_size(line.len()))
                .unwrap_or_else(|| "0B".to_string());
            (
                format!(
                    "[Line {start_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
                    format_size(DEFAULT_MAX_BYTES),
                    arguments.path
                ),
                Some(ToolResultDetails::Read { truncation }),
            )
        } else if truncation.truncated {
            let end_display = start_display + truncation.output_lines - 1;
            let suffix = if truncation.truncated_by
                == Some(TruncationLimit::Lines)
            {
                format!(
                    "[Showing lines {start_display}-{end_display} of {total_file_lines}. Use offset={} to continue.]",
                    end_display + 1
                )
            } else {
                format!(
                    "[Showing lines {start_display}-{end_display} of {total_file_lines} ({} limit). Use offset={} to continue.]",
                    format_size(DEFAULT_MAX_BYTES),
                    end_display + 1
                )
            };
            (
                format!("{}\n\n{suffix}", truncation.content),
                Some(ToolResultDetails::Read { truncation }),
            )
        } else if limit.is_some() && end_line < total_file_lines {
            (
                format!(
                    "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                    truncation.content,
                    total_file_lines - end_line,
                    end_line + 1
                ),
                None,
            )
        } else {
            (truncation.content, None)
        };
        let builder = ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text: output }])
            .is_error(false);
        Ok(match details {
            Some(details) => builder.details(details).build(),
            None => builder.build(),
        })
    }
}

/// Inline image formats recognized by pi's file-magic detector.
#[derive(Debug, Clone, Copy)]
enum ImageKind {
    Jpeg,
    Png,
    Gif,
    Webp,
    Bmp,
}

/// Bounds-checked typed reader for image file header integers.
struct ImageHeader<'a>(&'a [u8]);

impl ImageHeader<'_> {
    /// Reads one big-endian 32-bit integer when the requested range exists.
    fn u32_be(&self, offset: usize) -> Option<u32> {
        self.0
            .get(offset..offset.saturating_add(4))?
            .try_into()
            .ok()
            .map(u32::from_be_bytes)
    }

    /// Reads one little-endian 32-bit integer when the requested range exists.
    fn u32_le(&self, offset: usize) -> Option<u32> {
        self.0
            .get(offset..offset.saturating_add(4))?
            .try_into()
            .ok()
            .map(u32::from_le_bytes)
    }

    /// Reads one little-endian 16-bit integer when the requested range exists.
    fn u16_le(&self, offset: usize) -> Option<u16> {
        self.0
            .get(offset..offset.saturating_add(2))?
            .try_into()
            .ok()
            .map(u16::from_le_bytes)
    }
}

impl ImageKind {
    /// Detects supported still-image signatures while excluding animated PNG.
    fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xff, 0xd8, 0xff])
            && bytes.get(3).copied() != Some(0xf7)
        {
            return Some(Self::Jpeg);
        }
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
            && Self::is_still_png(bytes)
        {
            return Some(Self::Png);
        }
        if bytes.starts_with(b"GIF") {
            return Some(Self::Gif);
        }
        if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            return Some(Self::Webp);
        }
        if bytes.starts_with(b"BM") && Self::is_bmp(bytes) {
            return Some(Self::Bmp);
        }
        None
    }

    /// Validates PNG chunk framing and rejects animation before the first image data.
    fn is_still_png(bytes: &[u8]) -> bool {
        let header = ImageHeader(bytes);
        if bytes.len() < 16
            || header.u32_be(8) != Some(13)
            || bytes.get(12..16) != Some(b"IHDR")
        {
            return false;
        }
        let mut offset = 8_usize;
        while offset.saturating_add(8) <= bytes.len() {
            let Some(length) = header.u32_be(offset) else {
                return false;
            };
            let length = usize::try_from(length).unwrap_or(usize::MAX);
            let chunk_type = bytes.get(offset + 4..offset + 8);
            if chunk_type == Some(b"acTL") {
                return false;
            }
            if chunk_type == Some(b"IDAT") {
                return true;
            }
            let next = offset.saturating_add(12).saturating_add(length);
            if next <= offset || next > bytes.len() {
                return true;
            }
            offset = next;
        }
        true
    }

    /// Validates BMP header sizes, pixel offsets, planes, and supported bit depths.
    fn is_bmp(bytes: &[u8]) -> bool {
        if bytes.len() < 26 {
            return false;
        }
        let header = ImageHeader(bytes);
        let Some(declared_size) = header.u32_le(2) else {
            return false;
        };
        let Some(pixel_offset) = header.u32_le(10) else {
            return false;
        };
        let Some(dib_size) = header.u32_le(14) else {
            return false;
        };
        if (declared_size != 0 && declared_size < 26)
            || pixel_offset < 14_u32.saturating_add(dib_size)
            || (declared_size != 0 && pixel_offset >= declared_size)
        {
            return false;
        }
        let (planes, bits_per_pixel) = if dib_size == 12 {
            let Some(planes) = header.u16_le(22) else {
                return false;
            };
            let Some(bits_per_pixel) = header.u16_le(24) else {
                return false;
            };
            (planes, bits_per_pixel)
        } else if (40..=124).contains(&dib_size) && bytes.len() >= 30 {
            let Some(planes) = header.u16_le(26) else {
                return false;
            };
            let Some(bits_per_pixel) = header.u16_le(28) else {
                return false;
            };
            (planes, bits_per_pixel)
        } else {
            return false;
        };
        planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
    }

    /// Returns pi's canonical MIME type for the detected image format.
    const fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
        }
    }

    /// Returns the decoder format associated with this detected image kind.
    const fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::Png => image::ImageFormat::Png,
            Self::Gif => image::ImageFormat::Gif,
            Self::Webp => image::ImageFormat::WebP,
            Self::Bmp => image::ImageFormat::Bmp,
        }
    }

    /// Normalizes and resizes one image off the async runtime worker thread.
    async fn process(self, bytes: Vec<u8>) -> Option<ProcessedImage> {
        tokio::task::spawn_blocking(move || {
            ImageProcessor::process(self, bytes)
        })
        .await
        .ok()
        .flatten()
    }
}

/// Provider-ready image plus model-visible conversion and coordinate hints.
#[derive(Debug)]
struct ProcessedImage {
    data: String,
    mime_type: String,
    hints: Vec<String>,
}

/// CPU-bound implementation of pi's image normalization and resize policy.
struct ImageProcessor;

impl ImageProcessor {
    /// Converts unsupported inline formats and enforces 2000px/4.5MB limits.
    fn process(kind: ImageKind, bytes: Vec<u8>) -> Option<ProcessedImage> {
        use image::GenericImageView as _;

        const MAX_DIMENSION: u32 = 2_000;
        const MAX_BASE64_BYTES: usize = 4_718_592;

        let decoded =
            image::load_from_memory_with_format(&bytes, kind.image_format())
                .ok()?;
        let (original_width, original_height) = decoded.dimensions();
        let (normalized_bytes, normalized_kind, converted_from) =
            if matches!(kind, ImageKind::Bmp) {
                (
                    Self::encode_png(&decoded)?,
                    ImageKind::Png,
                    Some(kind.mime_type()),
                )
            } else {
                (bytes, kind, None)
            };
        let encoded_size = normalized_bytes.len().div_ceil(3) * 4;
        if original_width <= MAX_DIMENSION
            && original_height <= MAX_DIMENSION
            && encoded_size < MAX_BASE64_BYTES
        {
            let hints = converted_from
                .map(|from| {
                    vec![format!(
                        "[Image converted from {from} to {}.]",
                        normalized_kind.mime_type()
                    )]
                })
                .unwrap_or_default();
            return Some(ProcessedImage {
                data: base64::engine::general_purpose::STANDARD
                    .encode(normalized_bytes),
                mime_type: normalized_kind.mime_type().to_string(),
                hints,
            });
        }

        let mut target_width = original_width;
        let mut target_height = original_height;
        if target_width > MAX_DIMENSION {
            target_height = ((u64::from(target_height)
                * u64::from(MAX_DIMENSION)
                + u64::from(target_width) / 2)
                / u64::from(target_width)) as u32;
            target_width = MAX_DIMENSION;
        }
        if target_height > MAX_DIMENSION {
            target_width = ((u64::from(target_width)
                * u64::from(MAX_DIMENSION)
                + u64::from(target_height) / 2)
                / u64::from(target_height)) as u32;
            target_height = MAX_DIMENSION;
        }
        target_width = target_width.max(1);
        target_height = target_height.max(1);

        loop {
            let resized = decoded.resize_exact(
                target_width,
                target_height,
                image::imageops::FilterType::Lanczos3,
            );
            let mut candidates =
                vec![(Self::encode_png(&resized)?, ImageKind::Png)];
            for quality in [80_u8, 85, 70, 55, 40] {
                if let Some(jpeg) = Self::encode_jpeg(&resized, quality) {
                    candidates.push((jpeg, ImageKind::Jpeg));
                }
            }
            if let Some((candidate, output_kind)) =
                candidates.into_iter().find(|(candidate, _kind)| {
                    candidate.len().div_ceil(3) * 4 < MAX_BASE64_BYTES
                })
            {
                let mut hints = Vec::new();
                if let Some(from) = converted_from {
                    hints.push(format!(
                        "[Image converted from {from} to {}.]",
                        output_kind.mime_type()
                    ));
                }
                let scale = f64::from(original_width) / f64::from(target_width);
                hints.push(format!(
                    "[Image: original {original_width}x{original_height}, displayed at {target_width}x{target_height}. Multiply coordinates by {scale:.2} to map to original image.]"
                ));
                return Some(ProcessedImage {
                    data: base64::engine::general_purpose::STANDARD
                        .encode(candidate),
                    mime_type: output_kind.mime_type().to_string(),
                    hints,
                });
            }
            if target_width == 1 && target_height == 1 {
                return None;
            }
            let next_width = if target_width == 1 {
                1
            } else {
                (target_width * 3 / 4).max(1)
            };
            let next_height = if target_height == 1 {
                1
            } else {
                (target_height * 3 / 4).max(1)
            };
            if next_width == target_width && next_height == target_height {
                return None;
            }
            target_width = next_width;
            target_height = next_height;
        }
    }

    /// Encodes one dynamic image as PNG bytes.
    fn encode_png(image: &image::DynamicImage) -> Option<Vec<u8>> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
        Some(cursor.into_inner())
    }

    /// Encodes one dynamic image as JPEG bytes at the requested quality.
    fn encode_jpeg(
        image: &image::DynamicImage,
        quality: u8,
    ) -> Option<Vec<u8>> {
        let mut output = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut output,
            quality,
        )
        .encode_image(image)
        .ok()?;
        Some(output)
    }
}

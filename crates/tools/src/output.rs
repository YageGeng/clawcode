//! Shared Provider text projection and post-hook result budgets.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;

use protocol::{
    ContentBlock, EmbeddedResourceContent, ToolOutputInfo, ToolOutputPolicy,
    ToolOutputSaveFailure, ToolResult,
};
use tokio_util::sync::CancellationToken;

use crate::ToolError;

pub const TOOL_TEXT_BYTES: usize = 20 * 1024;
const MEDIA_BLOCKS: usize = 4;
const MEDIA_BYTES: usize = 8 * 1024 * 1024;
const SPOOL_BYTES: usize = 64 * 1024 * 1024;

/// Borrows one block's exact model-visible text without allocating a full projection.
pub struct ToolContentText<'a>(&'a ContentBlock);

impl<'a> TryFrom<&'a ContentBlock> for ToolContentText<'a> {
    type Error = ToolError;

    /// Leaves native images and tool calls to the role-specific Provider mapper.
    fn try_from(block: &'a ContentBlock) -> Result<Self, Self::Error> {
        match block {
            ContentBlock::Image { .. } | ContentBlock::ToolCall { .. } => Err(ToolError::Execution {
                tool: "content projection".into(),
                message: "native image and Tool Call blocks require role-specific provider mapping".into(),
            }),
            _ => Ok(Self(block)),
        }
    }
}

impl ToolContentText<'_> {
    /// Writes the same text used for budget measurement and Provider conversion.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        match self.0 {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                writer.write_all(text.as_bytes())
            }
            ContentBlock::Audio { mime_type, .. } => {
                write!(writer, "[Audio content: {mime_type}]")
            }
            ContentBlock::EmbeddedResource {
                uri,
                mime_type,
                content,
            } => {
                let binary =
                    matches!(content, EmbeddedResourceContent::Blob { .. });
                write!(
                    writer,
                    "[Embedded {}resource: {uri}",
                    if binary { "binary " } else { "" }
                )?;
                if let Some(mime_type) = mime_type {
                    write!(writer, " ({mime_type})")?;
                }
                writer.write_all(b"]")?;
                if let EmbeddedResourceContent::Text { text } = content {
                    write!(writer, "\n{text}")?;
                }
                Ok(())
            }
            ContentBlock::ResourceLink {
                server_id,
                uri,
                name,
                description,
                mime_type,
                ..
            } => {
                write!(writer, "[Resource link: {name} — {uri}")?;
                if let Some(mime_type) = mime_type {
                    write!(writer, " ({mime_type})")?;
                }
                writer.write_all(b"]")?;
                if let Some(description) = description {
                    write!(writer, "\n{description}")?;
                }
                if let Some(server) = server_id {
                    // The adapter supplies this route only when server resource policy permits it.
                    write!(
                        writer,
                        "\nRead with read_mcp_resource using server={server} and uri={uri}."
                    )?;
                }
                Ok(())
            }
            ContentBlock::Structured { value } => {
                writer.write_all(b"[Structured content]\n")?;
                serde_json::to_writer_pretty(writer, value)
                    .map_err(io::Error::other)
            }
            ContentBlock::Image { .. } | ContentBlock::ToolCall { .. } => {
                Err(io::Error::other("native content has no text view"))
            }
        }
    }
}

/// Counts all text while retaining only bounded head and tail byte windows.
#[derive(typed_builder::TypedBuilder)]
struct TextPreview {
    #[builder(default)]
    bytes: usize,
    #[builder(default)]
    head: Vec<u8>,
    #[builder(default)]
    tail: VecDeque<u8>,
    #[builder(default)]
    segments: usize,
}

impl Write for TextPreview {
    /// Counts a streaming chunk without retaining its unbounded middle.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        let half = TOOL_TEXT_BYTES / 2;
        self.head
            .extend(bytes.iter().take(half.saturating_sub(self.head.len())));
        let tail = bytes
            .get(bytes.len().saturating_sub(half)..)
            .unwrap_or(bytes);
        let overflow = self
            .tail
            .len()
            .saturating_add(tail.len())
            .saturating_sub(half);
        self.tail.drain(..overflow.min(self.tail.len()));
        self.tail.extend(tail);
        Ok(bytes.len())
    }

    /// Memory-only writes need no flush operation.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TextPreview {
    /// Includes separators used by text-only providers, even between empty text blocks.
    fn push(&mut self, view: ToolContentText<'_>) -> io::Result<()> {
        if self.segments > 0 {
            self.write_all(b"\n")?;
        }
        self.segments += 1;
        view.write_to(self)
    }

    /// Produces valid UTF-8 head/tail text including the exact omission count.
    fn render(mut self, limit: usize) -> String {
        if limit < 80 {
            return "[text omitted]".chars().take(limit).collect();
        }
        if self.bytes <= limit {
            let tail = self.tail.make_contiguous();
            let overlap = self
                .head
                .len()
                .saturating_add(tail.len())
                .saturating_sub(self.bytes);
            self.head.extend(tail.iter().skip(overlap));
            return String::from_utf8(self.head)
                .expect("text projection writes UTF-8");
        }
        // Reserve enough room for the marker before selecting Unicode boundaries.
        let each = limit.saturating_sub(80) / 2;
        self.head.truncate(each);
        while std::str::from_utf8(&self.head).is_err() {
            self.head.pop();
        }
        let tail = self.tail.make_contiguous();
        let mut start = tail.len().saturating_sub(each);
        while start < tail.len()
            && std::str::from_utf8(tail.get(start..).unwrap_or_default())
                .is_err()
        {
            start += 1;
        }
        let tail = std::str::from_utf8(tail.get(start..).unwrap_or_default())
            .expect("UTF-8 boundary");
        let omitted = self.bytes.saturating_sub(self.head.len() + tail.len());
        format!(
            "{}\n[... omitted {omitted} text bytes ...]\n{tail}",
            String::from_utf8(self.head).expect("UTF-8 boundary")
        )
    }
}

/// Bounds serialized bytes and cancellation while buffering small fragments before filesystem IO.
struct SpoolWriter<'a, W: Write> {
    file: io::BufWriter<W>,
    cancellation: &'a CancellationToken,
    bytes: usize,
}

impl<'a, W: Write> SpoolWriter<'a, W> {
    /// Binds a writer to one output's cancellation and serialized-byte accounting.
    fn new(file: W, cancellation: &'a CancellationToken) -> Self {
        Self {
            file: io::BufWriter::new(file),
            cancellation,
            bytes: 0,
        }
    }

    /// Serializes and flushes a complete result before its temporary file can be retained.
    fn write_result(
        mut self,
        result: &ToolResult,
    ) -> Result<(), ToolOutputSaveFailure> {
        if serde_json::to_writer(&mut self, result).is_err()
            || self.flush().is_err()
        {
            return Err(if self.cancellation.is_cancelled() {
                ToolOutputSaveFailure::Cancelled
            } else if self.bytes > SPOOL_BYTES {
                ToolOutputSaveFailure::TooLarge
            } else {
                ToolOutputSaveFailure::Io
            });
        }
        if self.cancellation.is_cancelled() {
            return Err(ToolOutputSaveFailure::Cancelled);
        }
        Ok(())
    }
}

impl<W: Write> Write for SpoolWriter<'_, W> {
    /// Rejects an oversized or cancelled spool before publishing any path.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::other("cancelled output save"));
        }
        if bytes.len() > SPOOL_BYTES.saturating_sub(self.bytes) {
            self.bytes = SPOOL_BYTES + 1;
            return Err(io::Error::other("output exceeds spool limit"));
        }
        let written = self.file.write(bytes)?;
        self.bytes += written;
        Ok(written)
    }

    /// Flushes successful writes before the tempfile is retained.
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Borrows only the output and filesystem context needed for one final projection.
#[derive(typed_builder::TypedBuilder)]
pub struct ToolResultProjection<'a> {
    result: &'a mut ToolResult,
    policy: ToolOutputPolicy,
    cancellation: &'a CancellationToken,
    directory: &'a Path,
}

impl<'a> ToolResultProjection<'a> {
    /// Binds one normalized result to the selected tool's frozen host policy.
    pub fn new(
        result: &'a mut ToolResult,
        policy: ToolOutputPolicy,
        cancellation: &'a CancellationToken,
        directory: &'a Path,
    ) -> Self {
        Self::builder()
            .result(result)
            .policy(policy)
            .cancellation(cancellation)
            .directory(directory)
            .build()
    }

    /// Applies aggregate budgets without changing the remote operation's disposition.
    pub fn apply(self) -> Result<(), ToolError> {
        if self.policy == ToolOutputPolicy::Preserve {
            return Ok(());
        }
        if let ToolOutputPolicy::ResourcePage(kind) = self.policy
            && !self.result.is_error
        {
            let valid = match self.result.blocks.as_slice() {
                [ContentBlock::Text { text }] if text.len() <= TOOL_TEXT_BYTES => serde_json::from_str::<serde_json::Value>(text).ok().is_some_and(|value| {
                    let Some(object) = value.as_object() else { return false; };
                    let Some(items) = value.get(kind.array_key()).and_then(serde_json::Value::as_array) else { return false; };
                    object.keys().all(|key| key == kind.array_key() || key == "nextCursor")
                        && items.len() <= 200
                        && value.get("nextCursor").is_none_or(|cursor| !items.is_empty() && cursor.as_str().is_some_and(|cursor| !cursor.is_empty() && cursor.len() <= TOOL_TEXT_BYTES))
                        && items.iter().all(|item| match kind {
                            protocol::McpResourcePageKind::Resources => serde_json::from_value::<protocol::McpResourceInfo>(item.clone()).is_ok(),
                            protocol::McpResourcePageKind::Templates => serde_json::from_value::<protocol::McpResourceTemplateInfo>(item.clone()).is_ok(),
                        })
                }),
                _ => false,
            };
            if !valid {
                // A damaged page must not advertise a continuation or masquerade as truncated success.
                self.result.blocks = vec![ContentBlock::Text { text: "Invalid or oversized MCP resource page after result hooks; restart resource listing.".into() }];
                self.result.is_error = true;
                self.result.output = None;
            }
            return Ok(());
        }
        let mut preview = TextPreview::builder().build();
        let mut media_count = 0usize;
        let mut media_bytes = 0usize;
        let mut media_text_bytes = 0usize;
        let mut media_text_count = 0usize;
        let mut original_media_text_count = 0usize;
        let mut original_media_text_bytes = 0usize;
        let mut omitted_media = 0;
        let mut retained = Vec::with_capacity(self.result.blocks.len());
        for block in &self.result.blocks {
            let data = match block {
                ContentBlock::Image { data, .. }
                | ContentBlock::Audio { data, .. } => Some(data),
                ContentBlock::EmbeddedResource {
                    content: EmbeddedResourceContent::Blob { data },
                    ..
                } => Some(data),
                _ => None,
            };
            let mut media_text = TextPreview::builder().build();
            if data.is_some()
                && let Ok(view) = ToolContentText::try_from(block)
            {
                media_text.push(view).map_err(|error| {
                    ToolError::Execution {
                        tool: "output projection".into(),
                        message: error.to_string(),
                    }
                })?;
                original_media_text_bytes += media_text.bytes;
                original_media_text_count += media_text.segments;
            }
            let keep = data.is_none_or(|data| {
                media_count < MEDIA_BLOCKS
                    && data.len() <= MEDIA_BYTES.saturating_sub(media_bytes)
                    && media_text.bytes
                        <= (TOOL_TEXT_BYTES - 2048 - MEDIA_BLOCKS)
                            .saturating_sub(media_text_bytes)
            });
            if let Some(data) = data {
                if keep {
                    media_count += 1;
                    media_bytes += data.len();
                    media_text_bytes += media_text.bytes;
                    media_text_count += media_text.segments;
                } else {
                    omitted_media += 1;
                }
            }
            if data.is_none()
                && let Ok(view) = ToolContentText::try_from(block)
            {
                preview.push(view).map_err(|error| ToolError::Execution {
                    tool: "output projection".into(),
                    message: error.to_string(),
                })?;
            }
            retained.push((keep, data.is_some()));
        }
        // The preview already counts separators within non-media text; add the retained media boundaries.
        let media_separators =
            media_text_count.saturating_sub(usize::from(preview.segments == 0));
        if preview.bytes + media_text_bytes + media_separators
            <= TOOL_TEXT_BYTES
            && omitted_media == 0
        {
            return Ok(());
        }
        let original_text_bytes = preview.bytes
            + original_media_text_bytes
            + original_media_text_count
                .saturating_sub(usize::from(preview.segments == 0));
        let (full_output_path, save_failure) = match self.save() {
            Ok(path) => (Some(path), None),
            Err(reason) => (None, Some(reason)),
        };
        let note = match &full_output_path {
            Some(path) => format!(
                "\n[Output limited; omitted media: {omitted_media}. Complete normalized result: {path}]"
            ),
            None => format!(
                "\n[Output limited; omitted media: {omitted_media}. Complete output was not saved: {save_failure:?}]"
            ),
        };
        // A pathological temporary directory path must not consume the entire text budget.
        let note = if note.len() <= 2048 {
            note
        } else {
            "\n[Output limited; see output diagnostics for the complete file location.]".into()
        };
        // A collapsed preview shares one block with the note; otherwise the note adds another separator.
        let text_budget = TOOL_TEXT_BYTES
            .saturating_sub(note.len() + media_text_bytes + media_text_count);
        let collapse_text =
            preview.bytes + usize::from(preview.segments > 0) > text_budget;
        let mut text = if collapse_text {
            preview.render(text_budget)
        } else {
            String::new()
        };
        text.push_str(&note);
        let mut blocks = Vec::new();
        for (block, (keep, media)) in std::mem::take(&mut self.result.blocks)
            .into_iter()
            .zip(retained)
        {
            if keep && (!collapse_text || media) {
                blocks.push(block);
            }
        }
        // Oversized non-image content becomes an explicitly labelled text preview, never broken JSON/base64.
        blocks.push(ContentBlock::Text { text });
        self.result.blocks = blocks;
        let mut measured = TextPreview::builder().build();
        for block in &self.result.blocks {
            if let Ok(view) = ToolContentText::try_from(block) {
                measured.push(view).map_err(|error| ToolError::Execution {
                    tool: "output projection".into(),
                    message: error.to_string(),
                })?;
            }
        }
        self.result.output = Some(
            ToolOutputInfo::builder()
                .original_text_bytes(original_text_bytes)
                .visible_text_bytes(measured.bytes)
                .omitted_media(omitted_media)
                .full_output_path(full_output_path)
                .save_failure(save_failure)
                .build(),
        );
        Ok(())
    }

    /// Writes the original normalized result once and keeps only a complete, closed file.
    fn save(&self) -> Result<String, ToolOutputSaveFailure> {
        if self.cancellation.is_cancelled() {
            return Err(ToolOutputSaveFailure::Cancelled);
        }
        let mut file = tempfile::Builder::new()
            .prefix("clawcode-output-")
            .suffix(".json")
            .tempfile_in(self.directory)
            .map_err(|_error| ToolOutputSaveFailure::Io)?;
        SpoolWriter::new(file.as_file_mut(), self.cancellation)
            .write_result(self.result)?;
        let path = file
            .into_temp_path()
            .keep()
            .map_err(|_error| ToolOutputSaveFailure::Io)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use protocol::{
        ContentBlock, ToolCallId, ToolOutputSaveFailure, ToolResult,
    };
    use tokio_util::sync::CancellationToken;

    use super::SpoolWriter;

    /// Observes IO fragmentation or rejects writes without adding production test switches.
    #[derive(Default)]
    struct OutputSink {
        bytes: Vec<u8>,
        writes: usize,
        reject_writes: bool,
    }

    impl Write for OutputSink {
        /// Counts writes at the underlying IO boundary and preserves successfully written bytes.
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.reject_writes {
                return Err(io::Error::other("injected output failure"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        /// The sink has no additional buffer.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Large structured output must coalesce tiny serializer fragments without losing the final tail.
    #[test]
    fn spool_coalesces_writes_and_flushes_complete_json() {
        let result = ToolResult::builder()
            .tool_call_id(ToolCallId::try_from("spool").expect("id"))
            .blocks(vec![ContentBlock::Structured {
                value: serde_json::json!((0..10_000).collect::<Vec<_>>()),
            }])
            .is_error(false)
            .build();
        let token = CancellationToken::new();
        let mut sink = OutputSink::default();
        SpoolWriter::new(&mut sink, &token)
            .write_result(&result)
            .expect("complete spool");
        assert_eq!(
            serde_json::from_slice::<ToolResult>(&sink.bytes)
                .expect("complete JSON"),
            result
        );
        assert!(
            sink.writes < 128,
            "serializer fragments reached IO individually: {} writes",
            sink.writes
        );
    }

    /// A final buffered write failure must be reported before the caller publishes a path.
    #[test]
    fn spool_reports_failure_from_final_flush() {
        let result = ToolResult::builder()
            .tool_call_id(ToolCallId::try_from("spool").expect("id"))
            .blocks(vec![ContentBlock::Text {
                text: "small buffered tail".into(),
            }])
            .is_error(false)
            .build();
        let token = CancellationToken::new();
        let mut sink = OutputSink {
            reject_writes: true,
            ..OutputSink::default()
        };
        assert_eq!(
            SpoolWriter::new(&mut sink, &token).write_result(&result),
            Err(ToolOutputSaveFailure::Io)
        );
    }
}

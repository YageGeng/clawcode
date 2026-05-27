use std::path::PathBuf;
use std::time::{Duration, Instant};

use protocol::PatchPreviewChange;

use crate::ToolArgumentsConsumer;

use super::{Hunk, UpdateChunk};

const PATCH_TEXT_KEY: &str = "\"patchText\"";
const APPLY_PATCH_ARGUMENTS_PREVIEW_INTERVAL: Duration = Duration::from_millis(500);

/// Consumes streamed apply_patch JSON arguments and emits patch previews.
pub(super) struct ApplyPatchArgumentsConsumer {
    extractor: PatchTextDeltaExtractor,
    parser: StreamingPatchParser,
    state: ApplyPatchConsumerState,
}

impl ApplyPatchArgumentsConsumer {
    /// Create a new consumer for one apply_patch argument stream.
    pub(super) fn new() -> Self {
        Self {
            extractor: PatchTextDeltaExtractor::new(),
            parser: StreamingPatchParser::new(),
            state: ApplyPatchConsumerState::default(),
        }
    }

    /// Convert parsed hunks into a throttled preview stream item.
    fn preview_item(
        &mut self,
        call_id: &str,
        hunks: &[Hunk],
    ) -> Vec<protocol::ToolArgumentsStreamItem> {
        let changes = preview_changes_from_hunks(hunks);
        if changes.is_empty() {
            return Vec::new();
        }
        let item = protocol::ToolArgumentsStreamItem::PatchPreview {
            call_id: call_id.to_string(),
            changes,
        };
        let now = Instant::now();
        if self
            .state
            .last_sent_at
            .is_some_and(|last| now.duration_since(last) < APPLY_PATCH_ARGUMENTS_PREVIEW_INTERVAL)
        {
            self.state.pending = Some(item);
            Vec::new()
        } else {
            self.state.last_sent_at = Some(now);
            self.state.pending = None;
            vec![item]
        }
    }
}

impl ToolArgumentsConsumer for ApplyPatchArgumentsConsumer {
    fn consume_delta(
        &mut self,
        call_id: &str,
        delta: &str,
    ) -> Vec<protocol::ToolArgumentsStreamItem> {
        if self.state.disabled {
            return Vec::new();
        }
        let patch_delta = match self.extractor.push_delta(delta) {
            Ok(patch_delta) => patch_delta,
            Err(_) => {
                self.state.disabled = true;
                return Vec::new();
            }
        };
        if patch_delta.is_empty() {
            return Vec::new();
        }
        let hunks = match self.parser.push_delta(&patch_delta) {
            Ok(hunks) => hunks,
            Err(_) => {
                self.state.disabled = true;
                return Vec::new();
            }
        };
        self.preview_item(call_id, &hunks)
    }

    fn finish(&mut self, _call_id: &str) -> Result<Vec<protocol::ToolArgumentsStreamItem>, String> {
        if self.state.disabled {
            return Ok(Vec::new());
        }
        let _ = self.parser.finish();
        Ok(self.state.pending.take().into_iter().collect())
    }
}

#[derive(Default)]
struct ApplyPatchConsumerState {
    last_sent_at: Option<Instant>,
    pending: Option<protocol::ToolArgumentsStreamItem>,
    disabled: bool,
}

/// Extracts the raw `patchText` string from streamed JSON tool arguments.
pub(super) struct PatchTextDeltaExtractor {
    mode: ExtractMode,
    key_progress: usize,
    failed: bool,
}

impl PatchTextDeltaExtractor {
    /// Create a new extractor for one tool-call argument stream.
    pub(super) fn new() -> Self {
        Self {
            mode: ExtractMode::SearchingKey,
            key_progress: 0,
            failed: false,
        }
    }

    /// Push one JSON argument delta and return newly decoded patch text.
    pub(super) fn push_delta(&mut self, delta: &str) -> Result<String, String> {
        if self.failed {
            return Ok(String::new());
        }

        let mut output = String::new();
        for ch in delta.chars() {
            self.process_char(ch, &mut output)?;
        }
        Ok(output)
    }

    /// Process one character from the streamed JSON argument payload.
    fn process_char(&mut self, ch: char, output: &mut String) -> Result<(), String> {
        match &mut self.mode {
            ExtractMode::SearchingKey => self.process_key_search(ch),
            ExtractMode::WaitingColon => {
                if ch == ':' {
                    self.mode = ExtractMode::WaitingValue;
                } else if !ch.is_whitespace() {
                    self.failed = true;
                    return Err("patchText key was not followed by ':'".to_string());
                }
            }
            ExtractMode::WaitingValue => {
                if ch == '"' {
                    self.mode = ExtractMode::ReadingPatchText {
                        escape: false,
                        unicode: None,
                        pending_high_surrogate: None,
                    };
                } else if !ch.is_whitespace() {
                    self.failed = true;
                    return Err("patchText value was not a JSON string".to_string());
                }
            }
            ExtractMode::ReadingPatchText {
                escape,
                unicode,
                pending_high_surrogate,
            } => {
                if process_patch_text_char(ch, output, escape, unicode, pending_high_surrogate)? {
                    self.mode = ExtractMode::Done;
                }
            }
            ExtractMode::Done => {}
        }
        Ok(())
    }

    /// Advance the streaming key matcher for `"patchText"`.
    fn process_key_search(&mut self, ch: char) {
        let expected = PATCH_TEXT_KEY
            .chars()
            .nth(self.key_progress)
            .expect("key progress is bounded by pattern length");
        if ch == expected {
            self.key_progress += 1;
            if self.key_progress == PATCH_TEXT_KEY.chars().count() {
                self.mode = ExtractMode::WaitingColon;
                self.key_progress = 0;
            }
        } else {
            self.key_progress = usize::from(ch == '"');
        }
    }
}

enum ExtractMode {
    SearchingKey,
    WaitingColon,
    WaitingValue,
    ReadingPatchText {
        escape: bool,
        unicode: Option<UnicodeEscape>,
        pending_high_surrogate: Option<u16>,
    },
    Done,
}

struct UnicodeEscape {
    digits: String,
}

impl UnicodeEscape {
    /// Create an empty Unicode escape accumulator.
    fn new() -> Self {
        Self {
            digits: String::new(),
        }
    }

    /// Push one hex digit and return the decoded code unit once complete.
    fn push(&mut self, ch: char) -> Result<Option<u32>, String> {
        if !ch.is_ascii_hexdigit() {
            return Err("invalid unicode escape in patchText".to_string());
        }
        self.digits.push(ch);
        if self.digits.len() < 4 {
            return Ok(None);
        }
        let value = u32::from_str_radix(&self.digits, 16)
            .map_err(|error| format!("invalid unicode escape in patchText: {error}"))?;
        Ok(Some(value))
    }
}

/// Decode one character while reading the `patchText` JSON string value.
fn process_patch_text_char(
    ch: char,
    output: &mut String,
    escape: &mut bool,
    unicode: &mut Option<UnicodeEscape>,
    pending_high_surrogate: &mut Option<u16>,
) -> Result<bool, String> {
    if let Some(unicode_escape) = unicode {
        if let Some(code_unit) = unicode_escape.push(ch)? {
            push_json_unicode_code_unit(code_unit, output, pending_high_surrogate)?;
            *unicode = None;
            *escape = false;
        }
        return Ok(false);
    }

    if *escape {
        match ch {
            'u' => {
                *unicode = Some(UnicodeEscape::new());
                return Ok(false);
            }
            _ if pending_high_surrogate.is_some() => {
                return Err(
                    "high surrogate in patchText must be followed by unicode escape".into(),
                );
            }
            '"' => output.push('"'),
            '\\' => output.push('\\'),
            '/' => output.push('/'),
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            other => {
                return Err(format!("unsupported JSON escape in patchText: \\{other}"));
            }
        }
        *escape = false;
        return Ok(false);
    }

    match ch {
        '\\' => *escape = true,
        _ if pending_high_surrogate.is_some() => {
            return Err("high surrogate in patchText must be followed by unicode escape".into());
        }
        '"' => return Ok(true),
        _ => output.push(ch),
    }
    Ok(false)
}

/// Append one JSON unicode escape code unit, joining surrogate pairs when needed.
fn push_json_unicode_code_unit(
    code_unit: u32,
    output: &mut String,
    pending_high_surrogate: &mut Option<u16>,
) -> Result<(), String> {
    match code_unit {
        0xD800..=0xDBFF => {
            if pending_high_surrogate.replace(code_unit as u16).is_some() {
                return Err("nested high surrogate in patchText".to_string());
            }
        }
        0xDC00..=0xDFFF => {
            let Some(high) = pending_high_surrogate.take() else {
                return Err("low surrogate in patchText had no high surrogate".to_string());
            };
            let scalar = 0x10000 + (((u32::from(high) - 0xD800) << 10) | (code_unit - 0xDC00));
            let decoded = char::from_u32(scalar).ok_or("invalid unicode scalar in patchText")?;
            output.push(decoded);
        }
        _ => {
            if pending_high_surrogate.is_some() {
                return Err("high surrogate in patchText must be followed by low surrogate".into());
            }
            let decoded = char::from_u32(code_unit).ok_or("invalid unicode scalar in patchText")?;
            output.push(decoded);
        }
    }
    Ok(())
}

/// Incrementally parses apply_patch text into preview hunks.
pub(super) struct StreamingPatchParser {
    line_buffer: String,
    state: StreamingParserState,
    line_number: usize,
}

impl StreamingPatchParser {
    /// Create a parser for one streamed patch payload.
    pub(super) fn new() -> Self {
        Self {
            line_buffer: String::new(),
            state: StreamingParserState::default(),
            line_number: 0,
        }
    }

    /// Push raw patch text and return the currently parsed hunk snapshot.
    pub(super) fn push_delta(&mut self, delta: &str) -> Result<Vec<Hunk>, String> {
        for ch in delta.chars() {
            if ch == '\n' {
                let mut line = std::mem::take(&mut self.line_buffer);
                if line.ends_with('\r') {
                    line.pop();
                }
                self.line_number += 1;
                self.process_line(&line)?;
            } else {
                self.line_buffer.push(ch);
            }
        }
        Ok(self.state.hunks.clone())
    }

    /// Finish parsing and require the explicit end marker.
    pub(super) fn finish(&mut self) -> Result<Vec<Hunk>, String> {
        if !self.line_buffer.is_empty() {
            let line = std::mem::take(&mut self.line_buffer);
            self.line_number += 1;
            self.process_line(&line)?;
        }
        if !matches!(self.state.mode, StreamingParserMode::EndedPatch) {
            return Err("The last line of the patch must be '*** End Patch'".to_string());
        }
        Ok(self.state.hunks.clone())
    }

    /// Process one complete patch line according to the current parser mode.
    fn process_line(&mut self, line: &str) -> Result<(), String> {
        let trimmed = line.trim();
        match self.state.mode {
            StreamingParserMode::NotStarted => {
                if trimmed == "*** Begin Patch" {
                    self.state.mode = StreamingParserMode::StartedPatch;
                    Ok(())
                } else {
                    Err("The first line of the patch must be '*** Begin Patch'".to_string())
                }
            }
            StreamingParserMode::StartedPatch => self.process_hunk_header(trimmed),
            StreamingParserMode::AddFile => {
                if self.process_hunk_header(trimmed).is_ok() {
                    return Ok(());
                }
                // Keep the preview parser aligned with opencode's permissive
                // add-file body parser by ignoring lines without a `+` prefix.
                if let Some(line_to_add) = line.strip_prefix('+')
                    && let Some(Hunk::Add { contents, .. }) = self.state.hunks.last_mut()
                {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                }
                Ok(())
            }
            StreamingParserMode::DeleteFile => self.process_hunk_header(trimmed),
            StreamingParserMode::UpdateFile => self.process_update_line(line),
            StreamingParserMode::EndedPatch => Ok(()),
        }
    }

    /// Process patch hunk headers that can appear between file operations.
    fn process_hunk_header(&mut self, line: &str) -> Result<(), String> {
        if line == "*** End Patch" {
            self.state.mode = StreamingParserMode::EndedPatch;
            return Ok(());
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            self.state.hunks.push(Hunk::Add {
                path: path.to_string(),
                contents: String::new(),
            });
            self.state.mode = StreamingParserMode::AddFile;
            return Ok(());
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            self.state.hunks.push(Hunk::Delete {
                path: path.to_string(),
            });
            self.state.mode = StreamingParserMode::DeleteFile;
            return Ok(());
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            self.state.hunks.push(Hunk::Update {
                path: path.to_string(),
                move_path: None,
                chunks: Vec::new(),
            });
            self.state.mode = StreamingParserMode::UpdateFile;
            return Ok(());
        }
        Err(format!("unsupported patch line: {line}"))
    }

    /// Process one line inside an update hunk.
    fn process_update_line(&mut self, line: &str) -> Result<(), String> {
        if self.process_hunk_header(line.trim_end()).is_ok() {
            return Ok(());
        }
        let Some(Hunk::Update {
            move_path, chunks, ..
        }) = self.state.hunks.last_mut()
        else {
            return Err("update content must follow Update File".to_string());
        };

        if let Some(target) = line.trim_end().strip_prefix("*** Move to: ") {
            *move_path = Some(target.to_string());
            return Ok(());
        }
        if line.trim_end() == "@@" {
            chunks.push(new_preview_chunk(None));
            return Ok(());
        }
        if let Some(context) = line.trim_end().strip_prefix("@@ ") {
            chunks.push(new_preview_chunk(Some(context.to_string())));
            return Ok(());
        }
        if line.trim_end() == "*** End of File" {
            if let Some(chunk) = chunks.last_mut() {
                chunk.is_end_of_file = true;
            }
            return Ok(());
        }

        if chunks.is_empty() {
            // opencode only records update lines after an explicit `@@` chunk.
            return Ok(());
        }
        let chunk = chunks.last_mut().expect("chunk exists after insertion");
        if let Some(value) = line.strip_prefix(' ') {
            chunk.old_lines.push(value.to_string());
            chunk.new_lines.push(value.to_string());
        } else if let Some(value) = line.strip_prefix('+') {
            chunk.new_lines.push(value.to_string());
        } else if let Some(value) = line.strip_prefix('-') {
            chunk.old_lines.push(value.to_string());
        } else {
            // Unknown update lines are ignored to match the full parser.
        }
        Ok(())
    }
}

#[derive(Default)]
struct StreamingParserState {
    mode: StreamingParserMode,
    hunks: Vec<Hunk>,
}

#[derive(Default)]
enum StreamingParserMode {
    #[default]
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile,
    EndedPatch,
}

/// Convert parsed patch hunks into protocol preview changes.
pub(super) fn preview_changes_from_hunks(hunks: &[Hunk]) -> Vec<PatchPreviewChange> {
    hunks
        .iter()
        .map(|hunk| match hunk {
            Hunk::Add { path, contents } => PatchPreviewChange::Add {
                path: PathBuf::from(path),
                content: contents.clone(),
            },
            Hunk::Delete { path } => PatchPreviewChange::Delete {
                path: PathBuf::from(path),
            },
            Hunk::Update {
                path,
                move_path,
                chunks,
            } => {
                let (old_text, new_text) = preview_text_from_chunks(chunks);
                PatchPreviewChange::Update {
                    path: PathBuf::from(path),
                    move_path: move_path.as_ref().map(PathBuf::from),
                    old_text,
                    new_text,
                }
            }
        })
        .collect()
}

/// Build local old/new preview text from update chunks.
fn preview_text_from_chunks(chunks: &[UpdateChunk]) -> (String, String) {
    let mut old_lines = Vec::new();
    let mut new_lines = Vec::new();
    for chunk in chunks {
        old_lines.extend(chunk.old_lines.iter().cloned());
        new_lines.extend(chunk.new_lines.iter().cloned());
    }
    (
        join_preview_lines(&old_lines),
        join_preview_lines(&new_lines),
    )
}

/// Join preview lines using file-like newline formatting.
fn join_preview_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// Build an empty update chunk with optional context for streaming preview.
fn new_preview_chunk(change_context: Option<String>) -> UpdateChunk {
    if let Some(change_context) = change_context {
        UpdateChunk::builder()
            .old_lines(Vec::new())
            .new_lines(Vec::new())
            .change_context(change_context)
            .is_end_of_file(false)
            .build()
    } else {
        UpdateChunk::builder()
            .old_lines(Vec::new())
            .new_lines(Vec::new())
            .is_end_of_file(false)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that patchText can be extracted across JSON argument chunks.
    #[test]
    fn extractor_reads_patch_text_across_chunks() {
        let mut extractor = PatchTextDeltaExtractor::new();

        assert_eq!(extractor.push_delta("{\"patch").unwrap(), "");
        assert_eq!(
            extractor.push_delta("Text\":\"*** Begin").unwrap(),
            "*** Begin"
        );
        assert_eq!(
            extractor.push_delta(" Patch\\n*** End Patch\"}").unwrap(),
            " Patch\n*** End Patch"
        );
    }

    /// Verifies that JSON string escapes are decoded before patch parsing.
    #[test]
    fn extractor_decodes_common_json_escapes() {
        let mut extractor = PatchTextDeltaExtractor::new();

        let extracted = extractor
            .push_delta("{\"patchText\":\"line\\\\path\\n\\\"quoted\\\"\\u0021\"}")
            .unwrap();

        assert_eq!(extracted, "line\\path\n\"quoted\"!");
    }

    /// Verifies that JSON surrogate pairs are decoded as one Unicode scalar.
    #[test]
    fn extractor_decodes_json_surrogate_pairs() {
        let mut extractor = PatchTextDeltaExtractor::new();

        let extracted = extractor
            .push_delta("{\"patchText\":\"face: \\uD83D\\uDE00\"}")
            .unwrap();

        assert_eq!(extracted, "face: \u{1F600}");
    }

    /// Verifies that add/update/delete hunks stream as complete lines arrive.
    #[test]
    fn streaming_parser_returns_preview_hunks_for_complete_lines() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta(
                "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** Update File: old.txt\n*** Move to: new.txt\n@@ fn main\n-old\n+new\n*** Delete File: gone.txt\n",
            )
            .unwrap();

        assert_eq!(hunks.len(), 3);
        assert_eq!(preview_changes_from_hunks(&hunks).len(), 3);
    }

    /// Verifies that finish requires the explicit end marker.
    #[test]
    fn streaming_parser_finish_requires_end_patch() {
        let mut parser = StreamingPatchParser::new();

        parser
            .push_delta("*** Begin Patch\n*** Add File: added.txt\n+hello\n")
            .unwrap();

        parser.finish().unwrap_err();
    }

    /// Verifies that CRLF input preserves patch semantics while trimming line endings.
    #[test]
    fn streaming_parser_accepts_crlf_line_endings() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta("*** Begin Patch\r\n*** Add File: added.txt\r\n+hello\r\n*** End Patch\r\n")
            .unwrap();

        assert_eq!(hunks.len(), 1);
    }

    /// Verifies that add-file previews ignore non-prefixed lines like the executor.
    #[test]
    fn streaming_parser_ignores_unprefixed_add_lines() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta("*** Begin Patch\n*** Add File: added.txt\nignored\n+kept\n")
            .unwrap();

        assert!(matches!(
            preview_changes_from_hunks(&hunks).as_slice(),
            [PatchPreviewChange::Add { content, .. }] if content == "kept\n"
        ));
    }

    /// Verifies that update preview lines before a chunk marker are ignored.
    #[test]
    fn streaming_parser_ignores_update_lines_before_chunk_marker() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta("*** Begin Patch\n*** Update File: file.txt\n-old\n@@\n-old\n+new\n")
            .unwrap();

        assert!(matches!(
            preview_changes_from_hunks(&hunks).as_slice(),
            [PatchPreviewChange::Update { old_text, new_text, .. }]
                if old_text == "old\n" && new_text == "new\n"
        ));
    }

    /// Verifies that move-only updates still appear in preview output.
    #[test]
    fn streaming_parser_previews_move_only_update() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta("*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n")
            .unwrap();

        assert!(matches!(
            preview_changes_from_hunks(&hunks).as_slice(),
            [PatchPreviewChange::Update { path, move_path, old_text, new_text }]
                if path == &PathBuf::from("old.txt")
                    && move_path.as_ref() == Some(&PathBuf::from("new.txt"))
                    && old_text.is_empty()
                    && new_text.is_empty()
        ));
    }

    /// Verifies that EOF preview markers attach to the active update chunk.
    #[test]
    fn streaming_parser_marks_eof_chunk() {
        let mut parser = StreamingPatchParser::new();

        let hunks = parser
            .push_delta(
                "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End of File\n",
            )
            .unwrap();

        assert!(matches!(
            hunks.as_slice(),
            [Hunk::Update { chunks, .. }] if chunks[0].is_end_of_file
        ));
    }

    /// Verifies that malformed JSON escapes disable preview instead of panicking.
    #[test]
    fn arguments_consumer_disables_preview_on_invalid_json_escape() {
        let mut consumer = ApplyPatchArgumentsConsumer::new();

        let first = consumer.consume_delta("call-1", "{\"patchText\":\"bad\\x");
        let second =
            consumer.consume_delta("call-1", "*** Begin Patch\\n*** Add File: a.txt\\n+ok\\n");

        assert!(first.is_empty());
        assert!(second.is_empty());
    }

    /// Verifies that the arguments consumer emits patch preview stream items.
    #[test]
    fn arguments_consumer_emits_patch_preview_items() {
        let mut consumer = ApplyPatchArgumentsConsumer::new();

        let items = consumer.consume_delta(
            "call-1",
            "{\"patchText\":\"*** Begin Patch\\n*** Add File: added.txt\\n+hello\\n",
        );

        assert!(matches!(
            items.as_slice(),
            [protocol::ToolArgumentsStreamItem::PatchPreview { call_id, changes }]
                if call_id == "call-1" && changes.len() == 1
        ));
    }
}

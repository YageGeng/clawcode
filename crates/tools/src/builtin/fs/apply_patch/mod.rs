//! Structured multi-file patch tool for agent edits.

mod stream_parser;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use protocol::FileChange;
use tokio::fs;
use typed_builder::TypedBuilder;

use crate::Tool;

const APPLY_PATCH_DESCRIPTION: &str = r#"Use the `apply_patch` tool to edit files. Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

Example patch:

```
*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch
```

It is important to remember:

- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
"#;

/// Captures one update chunk from an `Update File` hunk.
#[derive(Debug, Clone, TypedBuilder)]
struct UpdateChunk {
    /// Lines removed from the original file with patch prefixes stripped.
    old_lines: Vec<String>,
    /// Lines inserted into the new file with patch prefixes stripped.
    new_lines: Vec<String>,
    /// Optional marker text after `@@` that narrows the search position.
    #[builder(default, setter(strip_option))]
    change_context: Option<String>,
    /// Whether this chunk should be matched from the end of the file.
    is_end_of_file: bool,
}

/// Captures one file operation from a parsed patch.
#[derive(Debug, Clone)]
enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

/// Describes a byte-range replacement to apply to file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    start_byte: usize,
    end_byte: usize,
    new_text: String,
}

/// Applies structured patch payloads to files under the current working directory.
pub struct ApplyPatch;

impl ApplyPatch {
    /// Create a new patch tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ApplyPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        APPLY_PATCH_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patchText": {
                    "type": "string",
                    "description": "The full patch text that describes all changes to be made"
                }
            },
            "required": ["patchText"]
        })
    }

    fn capability(&self) -> protocol::ToolCapability {
        protocol::ToolCapability {
            supports_streaming: true,
        }
    }

    fn arguments_consumer(&self) -> Option<Box<dyn crate::ToolArgumentsConsumer>> {
        Some(Box::new(stream_parser::ApplyPatchArgumentsConsumer::new()))
    }

    fn needs_approval(&self, _: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        self.do_apply(arguments, ctx).await.map(|(text, _)| text)
    }

    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<
        (
            String,
            std::pin::Pin<Box<dyn futures::stream::Stream<Item = protocol::ToolStreamItem> + Send>>,
        ),
        String,
    > {
        let (model_output, changes) = self.do_apply(arguments, ctx).await?;

        let begin = protocol::ToolStreamItem::Begin(protocol::TurnItem::FileChange(
            protocol::FileChangeItem::builder()
                .id(String::new())
                .title("Apply patch".into())
                .changes(vec![])
                .status(protocol::FileChangeStatus::InProgress)
                .build(),
        ));
        let end = protocol::ToolStreamItem::End(protocol::TurnItem::FileChange(
            protocol::FileChangeItem::builder()
                .id(String::new())
                .title("Apply patch".into())
                .changes(changes)
                .status(protocol::FileChangeStatus::Completed)
                .model_output(model_output.clone())
                .build(),
        ));

        let stream: std::pin::Pin<
            Box<dyn futures::stream::Stream<Item = protocol::ToolStreamItem> + Send>,
        > = Box::pin(futures::stream::iter([begin, end]));
        Ok((model_output, stream))
    }
}

impl ApplyPatch {
    /// Execute a patch and return the summary together with file changes.
    async fn do_apply(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<(String, Vec<FileChange>), String> {
        let patch_text = arguments
            .get("patchText")
            .and_then(|v| v.as_str())
            .ok_or("missing 'patchText' argument")?;
        if patch_text.trim().is_empty() {
            return Err("patchText is empty".to_string());
        }

        let hunks = parse_patch(patch_text)?;
        let prepared = prepare_hunks(&ctx.cwd, &hunks).await?;
        let summary = prepared
            .iter()
            .map(PreparedHunk::summary_line)
            .collect::<Vec<_>>()
            .join("\n");
        let changes = file_changes_from_prepared_hunks(&prepared);

        for hunk in prepared {
            hunk.write().await?;
        }

        Ok((summary, changes))
    }
}

#[derive(Debug)]
enum PreparedHunk {
    Add {
        path: PathBuf,
        contents: String,
    },
    Delete {
        path: PathBuf,
        contents: String,
    },
    Update {
        path: PathBuf,
        move_path: Option<PathBuf>,
        old_contents: String,
        contents: String,
    },
}

impl PreparedHunk {
    /// Return the opencode-style summary line for one validated hunk.
    fn summary_line(&self) -> String {
        match self {
            Self::Add { path, .. } => format!("A {}", path.display()),
            Self::Delete { path, .. } => format!("D {}", path.display()),
            Self::Update {
                path, move_path, ..
            } => format!("M {}", move_path.as_ref().unwrap_or(path).display()),
        }
    }

    /// Write one previously validated hunk to disk.
    async fn write(self) -> Result<(), String> {
        match self {
            Self::Add { path, contents } => {
                create_parent_dir(&path).await?;
                fs::write(&path, contents)
                    .await
                    .map_err(|e| format!("failed to add {}: {e}", path.display()))
            }
            Self::Delete { path, .. } => fs::remove_file(&path)
                .await
                .map_err(|e| format!("failed to delete {}: {e}", path.display())),
            Self::Update {
                path,
                move_path,
                old_contents: _,
                contents,
            } => {
                if let Some(move_path) = move_path {
                    create_parent_dir(&move_path).await?;
                    fs::write(&move_path, contents)
                        .await
                        .map_err(|e| format!("failed to update {}: {e}", move_path.display()))?;
                    fs::remove_file(&path)
                        .await
                        .map_err(|e| format!("failed to remove {}: {e}", path.display()))
                } else {
                    fs::write(&path, contents)
                        .await
                        .map_err(|e| format!("failed to update {}: {e}", path.display()))
                }
            }
        }
    }
}

/// Parse a complete patch payload into structured hunks.
#[allow(clippy::string_slice)]
/// SAFETY: the loop guard `index + 1 < lines.len()` ensures `index` is always
/// a valid index into `lines`.
#[allow(clippy::indexing_slicing)]
fn parse_patch(text: &str) -> Result<Vec<Hunk>, String> {
    let text = strip_heredoc_wrappers(text).replace("\r\n", "\n");
    let begin = text
        .find("*** Begin Patch")
        .ok_or("missing *** Begin Patch marker")?;
    let end = text
        .rfind("*** End Patch")
        .ok_or("missing *** End Patch marker")?;
    if begin > end {
        return Err("patch markers are out of order".to_string());
    }

    let patch = &text[begin..end + "*** End Patch".len()];
    if patch.trim() == "*** Begin Patch\n*** End Patch" {
        return Err("empty patch".to_string());
    }

    let lines = patch.lines().collect::<Vec<_>>();
    let mut hunks = Vec::new();
    let mut current_update: Option<UpdateAccumulator> = None;
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            flush_update(&mut hunks, &mut current_update)?;
            let (contents, next_index) = parse_add_file(&lines, index + 1)?;
            hunks.push(Hunk::Add {
                path: path.to_string(),
                contents,
            });
            index = next_index;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            flush_update(&mut hunks, &mut current_update)?;
            hunks.push(Hunk::Delete {
                path: path.to_string(),
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            flush_update(&mut hunks, &mut current_update)?;
            current_update = Some(UpdateAccumulator::new(path.to_string()));
            index += 1;
            continue;
        }
        if let Some(move_path) = line.strip_prefix("*** Move to: ") {
            if let Some(update) = current_update.as_mut() {
                update.move_path = Some(move_path.to_string());
            }
            index += 1;
            continue;
        }
        if line.starts_with("@@") {
            if let Some(update) = current_update.as_mut() {
                update.start_chunk(parse_change_context(line));
            }
            index += 1;
            continue;
        }
        if line == "*** End of File" {
            if let Some(update) = current_update.as_mut()
                && let Some(chunk) = update.chunks.last_mut()
            {
                chunk.is_end_of_file = true;
            }
            index += 1;
            continue;
        }
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(update_line) = line.chars().next().filter(|c| matches!(c, '+' | '-' | ' ')) {
            if let Some(update) = current_update.as_mut()
                && !update.chunks.is_empty()
            {
                update.push_line(update_line, &line[update_line.len_utf8()..]);
            }
            index += 1;
            continue;
        }
        if line.starts_with("*** ") {
            return Err(format!("unknown patch operation: {line}"));
        }
        // Match opencode's permissive patch parser by ignoring unsupported
        // lines inside the envelope instead of failing the entire patch.
        index += 1;
    }
    flush_update(&mut hunks, &mut current_update)?;

    if hunks.is_empty() {
        return Err("no hunks found".to_string());
    }
    Ok(hunks)
}

#[derive(Debug)]
struct UpdateAccumulator {
    path: String,
    move_path: Option<String>,
    chunks: Vec<UpdateChunk>,
}

impl UpdateAccumulator {
    /// Create an accumulator for a single update hunk.
    fn new(path: String) -> Self {
        Self {
            path,
            move_path: None,
            chunks: Vec::new(),
        }
    }

    /// Start a new chunk and preserve its optional disambiguating context.
    fn start_chunk(&mut self, change_context: Option<String>) {
        self.chunks.push(new_update_chunk(change_context));
    }

    /// Return the active chunk, creating it when update lines appear before `@@`.
    fn ensure_chunk(&mut self, change_context: Option<String>) -> &mut UpdateChunk {
        if self.chunks.is_empty() {
            self.start_chunk(change_context);
        }
        self.chunks
            .last_mut()
            .expect("chunk exists after insertion")
    }

    /// Add a parsed update line to the active chunk.
    fn push_line(&mut self, prefix: char, line: &str) {
        let chunk = self.ensure_chunk(None);
        match prefix {
            '-' => chunk.old_lines.push(line.to_string()),
            '+' => chunk.new_lines.push(line.to_string()),
            ' ' => {
                chunk.old_lines.push(line.to_string());
                chunk.new_lines.push(line.to_string());
            }
            _ => {}
        }
    }

    /// Convert the accumulator into the final hunk type.
    fn into_hunk(self) -> Hunk {
        Hunk::Update {
            path: self.path,
            move_path: self.move_path,
            chunks: self.chunks,
        }
    }
}

/// Build an empty update chunk with optional context.
fn new_update_chunk(change_context: Option<String>) -> UpdateChunk {
    if let Some(change_context) = change_context {
        // `strip_option` keeps call sites ergonomic, so set this only when context exists.
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

/// Strip supported shell heredoc wrappers around a patch payload.
fn strip_heredoc_wrappers(text: &str) -> String {
    let mut lines = text.lines().collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| matches!(line.trim(), "cat <<'EOF'" | "cat <<EOF"))
        && lines.last().is_some_and(|line| line.trim() == "EOF")
    {
        lines.remove(0);
        lines.pop();
        lines.join("\n")
    } else {
        text.to_string()
    }
}

/// Parse the optional context that follows an `@@` chunk marker.
fn parse_change_context(line: &str) -> Option<String> {
    let context = line.trim_start_matches('@').trim();
    (!context.is_empty()).then(|| context.to_string())
}

/// Parse the body for an `Add File` operation.
/// SAFETY: the loop guard `index < lines.len()` ensures `index` is always
/// a valid index into `lines`.
#[allow(clippy::indexing_slicing)]
fn parse_add_file(lines: &[&str], mut index: usize) -> Result<(String, usize), String> {
    let mut content = Vec::new();
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("*** ") {
            break;
        }
        // opencode ignores non-prefixed lines in add-file bodies, so copied
        // prose around a patch does not become file content or reject the edit.
        if let Some(added) = line.strip_prefix('+') {
            content.push(added.to_string());
        }
        index += 1;
    }
    Ok((content.join("\n"), index))
}

/// Flush the current update accumulator into the hunk list.
fn flush_update(
    hunks: &mut Vec<Hunk>,
    current_update: &mut Option<UpdateAccumulator>,
) -> Result<(), String> {
    if let Some(update) = current_update.take() {
        hunks.push(update.into_hunk());
    }
    Ok(())
}

/// Validate all parsed hunks and prepare disk writes without mutating files.
async fn prepare_hunks(cwd: &Path, hunks: &[Hunk]) -> Result<Vec<PreparedHunk>, String> {
    let mut prepared = Vec::with_capacity(hunks.len());
    let mut planned_write_paths = HashSet::new();
    for hunk in hunks {
        match hunk {
            Hunk::Add { path, contents } => {
                let path = resolve_path(cwd, path);
                planned_write_paths.insert(path.clone());
                prepared.push(PreparedHunk::Add {
                    path,
                    contents: ensure_trailing_newline(contents),
                });
            }
            Hunk::Delete { path } => {
                let path = resolve_path(cwd, path);
                let contents = fs::read(&path)
                    .await
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                prepared.push(PreparedHunk::Delete {
                    path,
                    contents: String::from_utf8_lossy(&contents).into_owned(),
                });
            }
            Hunk::Update {
                path,
                move_path,
                chunks,
            } => {
                let path = resolve_path(cwd, path);
                let content = fs::read_to_string(&path)
                    .await
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                let new_contents = derive_new_contents_from_chunks(&content, chunks)?;
                let resolved_move_path = move_path.as_ref().map(|target| resolve_path(cwd, target));
                if let Some(target) = &resolved_move_path
                    && target != &path
                    && planned_write_paths.contains(target)
                {
                    return Err(format!("move target already planned: {}", target.display()));
                }
                if let Some(target) = &resolved_move_path
                    && target != &path
                    && fs::try_exists(target)
                        .await
                        .map_err(|e| format!("failed to inspect {}: {e}", target.display()))?
                {
                    return Err(format!("move target already exists: {}", target.display()));
                }
                planned_write_paths
                    .insert(resolved_move_path.clone().unwrap_or_else(|| path.clone()));
                prepared.push(PreparedHunk::Update {
                    path,
                    move_path: resolved_move_path,
                    old_contents: content,
                    contents: new_contents,
                });
            }
        }
    }
    Ok(prepared)
}

/// Build final file-change display payloads from validated hunks.
fn file_changes_from_prepared_hunks(prepared: &[PreparedHunk]) -> Vec<FileChange> {
    let mut changes = Vec::<FileChange>::new();
    for hunk in prepared {
        let (path, old_text, new_text) = match hunk {
            PreparedHunk::Add { path, contents } => (path.clone(), None, contents.clone()),
            PreparedHunk::Delete { path, contents } => {
                (path.clone(), Some(contents.clone()), String::new())
            }
            PreparedHunk::Update {
                path,
                move_path,
                old_contents,
                contents,
            } => (
                move_path.clone().unwrap_or_else(|| path.clone()),
                Some(old_contents.clone()),
                contents.clone(),
            ),
        };

        // Multiple hunks can target the same final path; keep the first old
        // state and replace only the final new state so the UI shows net change.
        if let Some(existing) = changes.iter_mut().find(|change| change.path == path) {
            existing.new_text = new_text;
        } else {
            let change = if let Some(old_text) = old_text {
                FileChange::builder()
                    .path(path)
                    .old_text(old_text)
                    .new_text(new_text)
                    .build()
            } else {
                FileChange::builder().path(path).new_text(new_text).build()
            };
            changes.push(change);
        }
    }
    changes
}

/// Resolve a patch path relative to the execution working directory.
fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Create the parent directory for a write target when it has one.
async fn create_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("failed to create parent dir {}: {e}", parent.display()))?;
    }
    Ok(())
}

/// Ensure created files end with a newline, matching opencode patch semantics.
fn ensure_trailing_newline(contents: &str) -> String {
    if contents.ends_with('\n') {
        contents.to_string()
    } else {
        format!("{contents}\n")
    }
}

/// Compute replacements and apply them back-to-front to derive new file contents.
fn derive_new_contents_from_chunks(
    content: &str,
    chunks: &[UpdateChunk],
) -> Result<String, String> {
    let (body, had_bom) = split_utf8_bom(content);
    let replacements = compute_replacements(body, chunks)?;
    let mut result = body.to_string();
    for replacement in replacements {
        result.replace_range(
            replacement.start_byte..replacement.end_byte,
            &replacement.new_text,
        );
    }
    let result = ensure_update_trailing_newline(&result);
    if had_bom {
        Ok(format!("\u{feff}{result}"))
    } else {
        Ok(result)
    }
}

/// Split an optional UTF-8 BOM from text before line-based patch matching.
fn split_utf8_bom(content: &str) -> (&str, bool) {
    if let Some(body) = content.strip_prefix('\u{feff}') {
        (body, true)
    } else {
        (content, false)
    }
}

/// Ensure non-empty updated files end with a newline, matching opencode updates.
fn ensure_update_trailing_newline(contents: &str) -> String {
    if contents.is_empty() || contents.ends_with('\n') {
        contents.to_string()
    } else {
        format!("{contents}\n")
    }
}

/// Compute byte-range replacements for all update chunks.
fn compute_replacements(content: &str, chunks: &[UpdateChunk]) -> Result<Vec<Replacement>, String> {
    let content_lines = content.lines().collect::<Vec<_>>();
    let line_starts = line_start_byte_offsets(content);
    let mut line_index = 0;
    let mut replacements = Vec::new();

    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            let context_index = seek_change_context(context, &content_lines, line_index)?;
            line_index = context_index + 1;
        }

        let old_refs = chunk
            .old_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let start_line = if old_refs.is_empty() {
            if chunk.change_context.is_some() {
                line_index
            } else {
                content_lines.len()
            }
        } else if chunk.is_end_of_file {
            seek_sequence_from_end(&old_refs, &content_lines, line_index)
                .ok_or_else(|| "Could not find matching lines for end-of-file chunk".to_string())?
        } else {
            seek_sequence(&old_refs, &content_lines, line_index)
                .ok_or_else(|| "Could not find matching lines for update chunk".to_string())?
        };
        let end_line = start_line + old_refs.len();
        let start_byte = line_starts
            .get(start_line)
            .copied()
            .ok_or_else(|| "start_line out of bounds".to_string())?;
        let end_byte = line_starts
            .get(end_line)
            .copied()
            .ok_or_else(|| "end_line out of bounds".to_string())?;
        let new_text = build_replacement_text(content, start_byte, end_byte, &chunk.new_lines);

        replacements.push(Replacement {
            start_byte,
            end_byte,
            new_text,
        });
        line_index = end_line;
    }

    replacements.reverse();
    Ok(replacements)
}

/// Find an old line sequence in content lines starting at `start`.
fn seek_sequence(old_lines: &[&str], content_lines: &[&str], start: usize) -> Option<usize> {
    find_sequence_with(old_lines, content_lines, start, |line| line.to_string())
        .or_else(|| {
            find_sequence_with(old_lines, content_lines, start, |line| {
                line.trim_end().to_string()
            })
        })
        .or_else(|| {
            find_sequence_with(old_lines, content_lines, start, |line| {
                line.trim().to_string()
            })
        })
        .or_else(|| {
            find_sequence_with(old_lines, content_lines, start, |line| {
                normalize_unicode_punctuation(line).trim().to_string()
            })
        })
}

/// Find an `@@` context line, falling back to a unique substring match.
fn seek_change_context(
    context: &str,
    content_lines: &[&str],
    start: usize,
) -> Result<usize, String> {
    let context_refs = [context];
    if let Some(line_index) = seek_sequence(&context_refs, content_lines, start) {
        return Ok(line_index);
    }
    seek_unique_context_substring(context, content_lines, start)
}

/// Find a unique substring context match after full-line matching fails.
fn seek_unique_context_substring(
    context: &str,
    content_lines: &[&str],
    start: usize,
) -> Result<usize, String> {
    let mut matched = None;
    for (line_index, line) in content_lines.iter().enumerate().skip(start) {
        if context_substring_matches(context, line) {
            if matched.is_some() {
                return Err(format!("Ambiguous context '{context}'"));
            }
            matched = Some(line_index);
        }
    }
    matched.ok_or_else(|| format!("Failed to find context '{context}'"))
}

/// Check whether one context string is a tolerant substring of one content line.
fn context_substring_matches(context: &str, line: &str) -> bool {
    let trimmed_context = context.trim();
    line.contains(context)
        || line.trim().contains(trimmed_context)
        || normalize_unicode_punctuation(line)
            .trim()
            .contains(&normalize_unicode_punctuation(trimmed_context))
}

/// Find an old line sequence by scanning backwards from the end of the file.
fn seek_sequence_from_end(
    old_lines: &[&str],
    content_lines: &[&str],
    start: usize,
) -> Option<usize> {
    if old_lines.len() > content_lines.len() {
        return None;
    }
    let max_start = content_lines.len() - old_lines.len();
    (start..=max_start)
        .rev()
        .find(|candidate| {
            sequence_matches(old_lines, content_lines, *candidate, |line| {
                line.to_string()
            })
        })
        .or_else(|| {
            (start..=max_start).rev().find(|candidate| {
                sequence_matches(old_lines, content_lines, *candidate, |line| {
                    line.trim_end().to_string()
                })
            })
        })
        .or_else(|| {
            (start..=max_start).rev().find(|candidate| {
                sequence_matches(old_lines, content_lines, *candidate, |line| {
                    line.trim().to_string()
                })
            })
        })
        .or_else(|| {
            (start..=max_start).rev().find(|candidate| {
                sequence_matches(old_lines, content_lines, *candidate, |line| {
                    normalize_unicode_punctuation(line).trim().to_string()
                })
            })
        })
}

/// Find an old line sequence using one normalization strategy for all lines.
fn find_sequence_with<F>(
    old_lines: &[&str],
    content_lines: &[&str],
    start: usize,
    normalize: F,
) -> Option<usize>
where
    F: Fn(&str) -> String + Copy,
{
    if old_lines.is_empty() || old_lines.len() > content_lines.len() || start > content_lines.len()
    {
        return None;
    }
    let max_start = content_lines.len() - old_lines.len();
    (start..=max_start)
        .find(|candidate| sequence_matches(old_lines, content_lines, *candidate, normalize))
}

/// Check one candidate sequence using a single normalization strategy.
/// SAFETY: the caller `find_sequence_with` validates that `candidate + old_lines.len()`
/// does not exceed `content_lines.len()`, so `candidate + offset` is always in bounds.
#[allow(clippy::indexing_slicing)]
fn sequence_matches<F>(
    old_lines: &[&str],
    content_lines: &[&str],
    candidate: usize,
    normalize: F,
) -> bool
where
    F: Fn(&str) -> String + Copy,
{
    old_lines
        .iter()
        .enumerate()
        .all(|(offset, old)| normalize(old) == normalize(content_lines[candidate + offset]))
}

/// Normalize common Unicode punctuation variants before fuzzy line matching.
fn normalize_unicode_punctuation(line: &str) -> String {
    line.replace(['\u{201c}', '\u{201d}'], "\"")
        .replace('\u{2014}', "--")
        .replace('\u{2013}', "-")
        .replace('\u{2026}', "...")
        .replace('\u{00a0}', " ")
}

/// Return byte offsets for the start of each logical line plus the content end.
fn line_start_byte_offsets(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    if *starts.last().unwrap_or(&0) != content.len() {
        starts.push(content.len());
    }
    starts
}

/// Build replacement text and preserve the replaced region's trailing newline when present.
#[allow(clippy::string_slice)]
fn build_replacement_text(
    content: &str,
    start_byte: usize,
    end_byte: usize,
    new_lines: &[String],
) -> String {
    let mut new_text = new_lines.join("\n");
    if end_byte > start_byte && content[..end_byte].ends_with('\n') && !new_text.ends_with('\n') {
        new_text.push('\n');
    }
    if start_byte == end_byte
        && start_byte < content.len()
        && !new_text.is_empty()
        && !new_text.ends_with('\n')
    {
        // Middle-of-file insertions must end before the existing target line.
        new_text.push('\n');
    }
    new_text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    /// Verifies that parser keeps all supported operation kinds in order.
    #[test]
    fn parse_patch_reads_add_update_delete_and_move_operations() {
        let hunks = parse_patch(
            "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** Update File: old.txt\n*** Move to: new.txt\n@@ def main()\n-old\n+new\n*** Delete File: gone.txt\n*** End Patch",
        )
        .unwrap();

        assert_eq!(hunks.len(), 3);
    }

    /// Verifies that heredoc wrappers are stripped before parsing.
    #[test]
    fn parse_patch_strips_heredoc_wrappers() {
        let hunks = parse_patch(
            "cat <<'EOF'\n*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End Patch\nEOF",
        )
        .unwrap();

        assert_eq!(hunks.len(), 1);
    }

    /// Verifies that parser accepts prose before and after the patch envelope.
    #[test]
    fn parse_patch_ignores_outer_prose() {
        let hunks = parse_patch(
            "before\n*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End Patch\nafter",
        )
        .unwrap();

        assert_eq!(hunks.len(), 1);
    }

    /// Verifies that line matching tolerates Unicode punctuation differences.
    #[test]
    fn seek_sequence_matches_unicode_punctuation() {
        let content_lines = vec!["say “hello”—now"];
        let old_lines = vec!["say \"hello\"--now"];

        assert_eq!(seek_sequence(&old_lines, &content_lines, 0), Some(0));
    }

    /// Verifies that an empty patch is rejected with a clear message.
    #[test]
    fn parse_patch_rejects_empty_patch() {
        let error = parse_patch("*** Begin Patch\n*** End Patch").unwrap_err();
        assert!(error.contains("empty patch"));
    }

    /// Verifies that patches without a begin marker are rejected.
    #[test]
    fn parse_patch_rejects_missing_begin_marker() {
        let error = parse_patch("plain text without markers").unwrap_err();
        assert!(error.contains("missing *** Begin Patch"));
    }

    /// Verifies that patches without an end marker are rejected.
    #[test]
    fn parse_patch_rejects_missing_end_marker() {
        let error = parse_patch("*** Begin Patch\n*** Add File: a.txt\n+hello").unwrap_err();
        assert!(error.contains("missing *** End Patch"));
    }

    /// Verifies that whitespace-only patch bodies have no hunks.
    #[test]
    fn parse_patch_rejects_no_hunks() {
        let error = parse_patch("*** Begin Patch\n   \n*** End Patch").unwrap_err();
        assert!(error.contains("no hunks"));
    }

    /// Verifies that add-file parsing ignores lines without `+` prefixes.
    #[test]
    fn parse_patch_ignores_add_without_plus_prefix() {
        let hunks =
            parse_patch("*** Begin Patch\n*** Add File: a.txt\nhello\n+kept\n*** End Patch")
                .unwrap();
        assert!(matches!(
            hunks.as_slice(),
            [Hunk::Add { contents, .. }] if contents == "kept"
        ));
    }

    /// Verifies that invalid update prefixes are ignored like opencode.
    #[test]
    fn parse_patch_ignores_update_with_invalid_prefix() {
        let hunks = parse_patch(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n*bad\n-ok\n+OK\n*** End Patch",
        )
        .unwrap();
        assert!(matches!(
            hunks.as_slice(),
            [Hunk::Update { chunks, .. }] if chunks[0].old_lines == ["ok"] && chunks[0].new_lines == ["OK"]
        ));
    }

    /// Verifies that update lines before an explicit chunk marker are ignored like opencode.
    #[test]
    fn parse_patch_ignores_update_lines_before_chunk_marker() {
        let hunks = parse_patch(
            "*** Begin Patch\n*** Update File: a.txt\n-old\n@@\n-old\n+new\n*** End Patch",
        )
        .unwrap();

        assert!(matches!(
            hunks.as_slice(),
            [Hunk::Update { chunks, .. }] if chunks.len() == 1 && chunks[0].old_lines == ["old"]
        ));
    }

    /// Verifies that move metadata outside an update hunk is ignored.
    #[test]
    fn parse_patch_ignores_move_without_update() {
        let error = parse_patch("*** Begin Patch\n*** Move to: b.txt\n*** End Patch").unwrap_err();
        assert!(error.contains("no hunks"));
    }

    /// Verifies that chunk markers outside an update hunk are ignored.
    #[test]
    fn parse_patch_ignores_chunk_without_update() {
        let error = parse_patch("*** Begin Patch\n@@\n*** End Patch").unwrap_err();
        assert!(error.contains("no hunks"));
    }

    /// Verifies that end-of-file markers outside an update hunk are ignored.
    #[test]
    fn parse_patch_ignores_eof_without_update() {
        let error = parse_patch("*** Begin Patch\n*** End of File\n*** End Patch").unwrap_err();
        assert!(error.contains("no hunks"));
    }

    /// Verifies that unknown operation headers get a targeted parser error.
    #[test]
    fn parse_patch_rejects_unknown_operation_header() {
        let error =
            parse_patch("*** Begin Patch\n*** Unknown File: a.txt\n*** End Patch").unwrap_err();
        assert!(error.contains("unknown patch operation"));
    }

    /// Verifies that exact line-sequence matching returns the expected position.
    #[test]
    fn seek_sequence_exact_match() {
        let content_lines = vec!["one", "two", "three"];
        let old_lines = vec!["two", "three"];
        assert_eq!(seek_sequence(&old_lines, &content_lines, 0), Some(1));
    }

    /// Verifies that line-sequence matching tolerates trailing whitespace differences.
    #[test]
    fn seek_sequence_rstrip_match() {
        let content_lines = vec!["one", "two   ", "three"];
        let old_lines = vec!["two", "three"];
        assert_eq!(seek_sequence(&old_lines, &content_lines, 0), Some(1));
    }

    /// Verifies that line-sequence matching tolerates leading and trailing whitespace differences.
    #[test]
    fn seek_sequence_trim_match() {
        let content_lines = vec!["one", "  two   ", "three"];
        let old_lines = vec!["two", "three"];
        assert_eq!(seek_sequence(&old_lines, &content_lines, 0), Some(1));
    }

    /// Verifies that unrelated lines do not match.
    #[test]
    fn seek_sequence_not_found() {
        let content_lines = vec!["one", "two", "three"];
        let old_lines = vec!["four"];
        assert_eq!(seek_sequence(&old_lines, &content_lines, 0), None);
    }

    /// Verifies that reverse line matching finds the last eligible match.
    #[test]
    fn seek_sequence_from_end_finds_last_match() {
        let content_lines = vec!["target", "middle", "target"];
        let old_lines = vec!["target"];
        assert_eq!(
            seek_sequence_from_end(&old_lines, &content_lines, 0),
            Some(2)
        );
    }

    /// Verifies that EOF matching falls back to forward search when the tail differs.
    #[test]
    fn seek_sequence_from_end_falls_back_to_forward_match() {
        let content_lines = vec!["target", "middle", "tail"];
        let old_lines = vec!["target"];
        assert_eq!(
            seek_sequence_from_end(&old_lines, &content_lines, 0),
            Some(0)
        );
    }

    /// Verifies that adding an already existing file overwrites it like opencode.
    #[tokio::test]
    async fn apply_patch_add_overwrites_file_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("exists.txt"), "old").unwrap();
        let patch = "*** Begin Patch\n*** Add File: exists.txt\n+new\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("exists.txt")).unwrap(),
            "new\n"
        );
    }

    /// Verifies that add-file bodies ignore unprefixed lines like opencode.
    #[tokio::test]
    async fn apply_patch_add_ignores_unprefixed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: added.txt\nignored\n+kept\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("added.txt")).unwrap(),
            "kept\n"
        );
    }

    /// Verifies that deleting a missing file is rejected.
    #[tokio::test]
    async fn apply_patch_rejects_delete_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        result.unwrap_err();
    }

    /// Verifies that a later validation failure prevents earlier writes.
    #[tokio::test]
    async fn apply_patch_atomic_across_multiple_hunks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("delete.txt"), "delete me").unwrap();
        let patch = "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Delete File: missing.txt\n*** Delete File: delete.txt\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        result.unwrap_err();
        assert!(!dir.path().join("created.txt").exists());
        assert!(dir.path().join("delete.txt").exists());
    }

    /// Verifies that move-only updates preserve file contents like opencode.
    #[tokio::test]
    async fn apply_patch_update_move_only_preserves_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("move.txt"), "move me").unwrap();
        let patch =
            "*** Begin Patch\n*** Update File: move.txt\n*** Move to: moved.txt\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert!(!dir.path().join("move.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("moved.txt")).unwrap(),
            "move me\n"
        );
    }

    /// Verifies that move targets are rejected when they would overwrite a file.
    #[tokio::test]
    async fn apply_patch_rejects_move_target_that_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.txt"), "source\n").unwrap();
        std::fs::write(dir.path().join("target.txt"), "target\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: source.txt\n*** Move to: target.txt\n@@\n-source\n+updated\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        let error = result.unwrap_err();
        assert!(error.contains("move target already exists"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
            "target\n"
        );
    }

    /// Verifies that move targets cannot overwrite files created earlier in the same patch.
    #[tokio::test]
    async fn apply_patch_rejects_move_target_planned_by_add_hunk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.txt"), "source\n").unwrap();
        let patch = "*** Begin Patch\n*** Add File: target.txt\n+created\n*** Update File: source.txt\n*** Move to: target.txt\n@@\n-source\n+updated\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        let error = result.unwrap_err();
        assert!(error.contains("move target already planned"));
        assert!(!dir.path().join("target.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
            "source\n"
        );
    }

    /// Verifies that a missing change context rejects the update instead of falling back.
    #[tokio::test]
    async fn apply_patch_rejects_missing_change_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("context.txt"), "fn a\nx = 1\nfn b\nx = 1\n").unwrap();
        let patch =
            "*** Begin Patch\n*** Update File: context.txt\n@@ fn c\n-x = 1\n+x = 2\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        let error = result.unwrap_err();
        assert!(error.contains("Failed to find context"));
    }

    /// Verifies that change context can locate a unique substring inside a line.
    #[tokio::test]
    async fn apply_patch_change_context_accepts_unique_substring() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("context.txt"),
            "async fn a() {\n    value = 1\n}\nasync fn b() {\n    value = 1\n}\n",
        )
        .unwrap();
        let patch = "*** Begin Patch\n*** Update File: context.txt\n@@ fn b\n-    value = 1\n+    value = 2\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("context.txt")).unwrap(),
            "async fn a() {\n    value = 1\n}\nasync fn b() {\n    value = 2\n}\n"
        );
    }

    /// Verifies that ambiguous substring context is rejected instead of guessing.
    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_substring_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("context.txt"),
            "async fn handler_a() {\n    value = 1\n}\nasync fn handler_b() {\n    value = 1\n}\n",
        )
        .unwrap();
        let patch = "*** Begin Patch\n*** Update File: context.txt\n@@ handler\n-    value = 1\n+    value = 2\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        let error = result.unwrap_err();
        assert!(error.contains("Ambiguous context"));
    }

    /// Verifies that pure insertion chunks append before the final trailing newline.
    #[tokio::test]
    async fn apply_patch_pure_addition_appends_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("append.txt"), "one\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: append.txt\n@@\n+two\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("append.txt")).unwrap(),
            "one\ntwo\n"
        );
    }

    /// Verifies that append-only chunks use their own anchors in multi-hunk updates.
    #[tokio::test]
    async fn apply_patch_multi_hunk_append_only_uses_each_anchor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("append.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: append.txt\n@@ alpha\n+after alpha\n@@ beta\n+after beta\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("append.txt")).unwrap(),
            "alpha\nafter alpha\nbeta\nafter beta\ngamma\n"
        );
    }

    /// Verifies that EOF anchors update the final matching block when duplicates exist.
    #[tokio::test]
    async fn apply_patch_eof_anchor_prefers_last_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tail.txt"),
            "start\nmarker\nmiddle\nmarker\nend\n",
        )
        .unwrap();
        let patch = "*** Begin Patch\n*** Update File: tail.txt\n@@\n-marker\n-end\n+marker changed\n+end\n*** End of File\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("tail.txt")).unwrap(),
            "start\nmarker\nmiddle\nmarker changed\nend\n"
        );
    }

    /// Verifies that BOM-prefixed files can update the first visible line and preserve the BOM.
    #[tokio::test]
    async fn apply_patch_preserves_bom_when_updating_first_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bom.txt"), "\u{feff}first\nsecond\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: bom.txt\n@@\n-first\n+FIRST\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("bom.txt")).unwrap(),
            "\u{feff}FIRST\nsecond\n"
        );
    }

    /// Verifies that separate update hunks can both succeed.
    #[tokio::test]
    async fn apply_patch_two_separate_updates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.path().join("two.txt"), "x\ny\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: one.txt\n@@\n-a\n+A\n*** Update File: two.txt\n@@\n-y\n+Y\n*** End Patch";

        ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("one.txt")).unwrap(),
            "A\nb\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two.txt")).unwrap(),
            "x\nY\n"
        );
    }

    /// Verifies that the patch tool validates all hunks before writing any file.
    #[tokio::test]
    async fn apply_patch_does_not_write_when_later_hunk_fails() {
        let dir = tempfile::tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Delete File: missing.txt\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        result.unwrap_err();
        assert!(!dir.path().join("created.txt").exists());
    }

    /// Verifies that the patch tool applies all supported file operations.
    #[tokio::test]
    async fn apply_patch_adds_updates_deletes_and_moves_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("modify.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("delete.txt"), "remove me").unwrap();
        std::fs::write(dir.path().join("move.txt"), "move me").unwrap();

        let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Update File: modify.txt\n@@ two\n-three\n+THREE\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved/renamed.txt\n@@\n move me\n*** End Patch";
        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            result,
            format!(
                "A {}\nM {}\nD {}\nM {}",
                dir.path().join("nested/new.txt").display(),
                dir.path().join("modify.txt").display(),
                dir.path().join("delete.txt").display(),
                dir.path().join("moved/renamed.txt").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("modify.txt")).unwrap(),
            "one\ntwo\nTHREE\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap(),
            "created\n"
        );
        assert!(!dir.path().join("delete.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("moved/renamed.txt")).unwrap(),
            "move me\n"
        );
    }

    /// Verifies that streaming execution returns final file states for UI diffs.
    #[tokio::test]
    async fn apply_patch_streaming_result_includes_file_changes() {
        use futures::stream::StreamExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("modify.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("delete.txt"), "remove me\n").unwrap();

        let patch = "*** Begin Patch\n*** Add File: added.txt\n+created\n*** Update File: modify.txt\n@@\n-two\n+TWO\n*** Delete File: delete.txt\n*** End Patch";
        let (model_output, mut stream) = ApplyPatch::new()
            .execute_streaming(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert!(!model_output.is_empty());

        let mut changes = Vec::new();
        while let Some(item) = stream.next().await {
            if let protocol::ToolStreamItem::End(protocol::TurnItem::FileChange(item)) = item {
                changes = item.changes;
            }
        }
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].path, dir.path().join("added.txt"));
        assert_eq!(changes[0].old_text, None);
        assert_eq!(changes[0].new_text, "created\n");
        assert_eq!(changes[1].path, dir.path().join("modify.txt"));
        assert_eq!(changes[1].old_text.as_deref(), Some("one\ntwo\n"));
        assert_eq!(changes[1].new_text, "one\nTWO\n");
        assert_eq!(changes[2].path, dir.path().join("delete.txt"));
        assert_eq!(changes[2].old_text.as_deref(), Some("remove me\n"));
        assert_eq!(changes[2].new_text, "");
    }

    /// Verifies that update hunks fail when their exact context is absent.
    #[tokio::test]
    async fn apply_patch_fails_when_context_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("modify.txt"), "one\ntwo").unwrap();
        let patch =
            "*** Begin Patch\n*** Update File: modify.txt\n@@\n-missing\n+new\n*** End Patch";

        let result = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        result.unwrap_err();
    }

    /// Verifies that patch operations always go through approval.
    #[tokio::test]
    async fn apply_patch_always_requires_approval() {
        let tool = ApplyPatch::new();
        assert!(tool.needs_approval(
            &serde_json::json!({
                "patchText": "*** Begin Patch\n*** End Patch"
            }),
            &ToolContext::for_test(Path::new(".")),
        ));
    }

    /// Verifies that apply_patch opts into argument streaming previews.
    #[test]
    fn apply_patch_exposes_arguments_consumer() {
        let tool = ApplyPatch::new();

        assert!(tool.arguments_consumer().is_some());
    }
}

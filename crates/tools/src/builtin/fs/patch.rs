//! Structured multi-file patch tool for agent edits.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
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
- You must prefix new lines with `+` even when creating a new file"#;

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

    fn needs_approval(&self, _: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let patch_text = arguments["patchText"]
            .as_str()
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

        for hunk in prepared {
            hunk.write().await?;
        }

        Ok(summary)
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
    },
    Update {
        path: PathBuf,
        move_path: Option<PathBuf>,
        contents: String,
    },
}

impl PreparedHunk {
    /// Return the opencode-style summary line for one validated hunk.
    fn summary_line(&self) -> String {
        match self {
            Self::Add { path, .. } => format!("A {}", path.display()),
            Self::Delete { path } => format!("D {}", path.display()),
            Self::Update { path, .. } => format!("M {}", path.display()),
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
            Self::Delete { path } => fs::remove_file(&path)
                .await
                .map_err(|e| format!("failed to delete {}: {e}", path.display())),
            Self::Update {
                path,
                move_path,
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
            let update = current_update
                .as_mut()
                .ok_or("Move to must follow Update File")?;
            update.move_path = Some(move_path.to_string());
            index += 1;
            continue;
        }
        if line.starts_with("@@") {
            let update = current_update
                .as_mut()
                .ok_or("update chunk must follow Update File")?;
            update.start_chunk(parse_change_context(line));
            index += 1;
            continue;
        }
        if line == "*** End of File" {
            let update = current_update
                .as_mut()
                .ok_or("End of File must follow Update File")?;
            update.ensure_chunk(None).is_end_of_file = true;
            index += 1;
            continue;
        }
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(update_line) = line.chars().next().filter(|c| matches!(c, '+' | '-' | ' ')) {
            let update = current_update
                .as_mut()
                .ok_or("update content must follow Update File")?;
            update.push_line(update_line, &line[update_line.len_utf8()..]);
            index += 1;
            continue;
        }
        return Err(format!("unsupported patch line: {line}"));
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
fn parse_add_file(lines: &[&str], mut index: usize) -> Result<(String, usize), String> {
    let mut content = Vec::new();
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("*** ") {
            break;
        }
        let added = line
            .strip_prefix('+')
            .ok_or_else(|| format!("add file line must start with '+': {line}"))?;
        content.push(added.to_string());
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
        if update.chunks.is_empty() {
            return Err(format!("update for {} has no chunks", update.path));
        }
        hunks.push(update.into_hunk());
    }
    Ok(())
}

/// Validate all parsed hunks and prepare disk writes without mutating files.
async fn prepare_hunks(cwd: &Path, hunks: &[Hunk]) -> Result<Vec<PreparedHunk>, String> {
    let mut prepared = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        match hunk {
            Hunk::Add { path, contents } => {
                let path = resolve_path(cwd, path);
                if fs::try_exists(&path)
                    .await
                    .map_err(|e| format!("failed to inspect {}: {e}", path.display()))?
                {
                    return Err(format!("file already exists: {}", path.display()));
                }
                prepared.push(PreparedHunk::Add {
                    path,
                    contents: ensure_trailing_newline(contents),
                });
            }
            Hunk::Delete { path } => {
                let path = resolve_path(cwd, path);
                fs::read(&path)
                    .await
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                prepared.push(PreparedHunk::Delete { path });
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
                prepared.push(PreparedHunk::Update {
                    path,
                    move_path: move_path.as_ref().map(|target| resolve_path(cwd, target)),
                    contents: new_contents,
                });
            }
        }
    }
    Ok(prepared)
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
    let replacements = compute_replacements(content, chunks)?;
    let mut result = content.to_string();
    for replacement in replacements {
        result.replace_range(
            replacement.start_byte..replacement.end_byte,
            &replacement.new_text,
        );
    }
    Ok(result)
}

/// Compute byte-range replacements for all update chunks.
fn compute_replacements(content: &str, chunks: &[UpdateChunk]) -> Result<Vec<Replacement>, String> {
    let content_lines = content.lines().collect::<Vec<_>>();
    let line_starts = line_start_byte_offsets(content);
    let mut line_index = 0;
    let mut replacements = Vec::new();

    for chunk in chunks {
        if let Some(context) = &chunk.change_context
            && let Some(context_index) = content_lines[line_index..]
                .iter()
                .position(|line| line.contains(context))
        {
            line_index += context_index;
        }

        let old_refs = chunk
            .old_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let start_line = if old_refs.is_empty() {
            content_lines.len()
        } else if chunk.is_end_of_file {
            seek_sequence_from_end(&old_refs, &content_lines, line_index)
                .ok_or_else(|| "Could not find matching lines for end-of-file chunk".to_string())?
        } else {
            seek_sequence(&old_refs, &content_lines, line_index)
                .ok_or_else(|| "Could not find matching lines for update chunk".to_string())?
        };
        let end_line = start_line + old_refs.len();
        let start_byte = line_starts[start_line];
        let end_byte = line_starts[end_line];
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

    /// Verifies that added file contents must use `+` prefixes.
    #[test]
    fn parse_patch_rejects_add_without_plus_prefix() {
        let error =
            parse_patch("*** Begin Patch\n*** Add File: a.txt\nhello\n*** End Patch").unwrap_err();
        assert!(error.contains("add file line must start"));
    }

    /// Verifies that invalid update prefixes produce an unsupported-line error.
    #[test]
    fn parse_patch_rejects_update_with_invalid_prefix() {
        let error = parse_patch("*** Begin Patch\n*** Update File: a.txt\n*bad\n*** End Patch")
            .unwrap_err();
        assert!(error.contains("unsupported patch line"));
    }

    /// Verifies that move metadata cannot appear outside an update hunk.
    #[test]
    fn parse_patch_rejects_move_without_update() {
        let error = parse_patch("*** Begin Patch\n*** Move to: b.txt\n*** End Patch").unwrap_err();
        assert!(error.contains("Move to must follow Update File"));
    }

    /// Verifies that chunk markers cannot appear outside an update hunk.
    #[test]
    fn parse_patch_rejects_chunk_without_update() {
        let error = parse_patch("*** Begin Patch\n@@\n*** End Patch").unwrap_err();
        assert!(error.contains("update chunk must follow Update File"));
    }

    /// Verifies that end-of-file markers cannot appear outside an update hunk.
    #[test]
    fn parse_patch_rejects_eof_without_update() {
        let error = parse_patch("*** Begin Patch\n*** End of File\n*** End Patch").unwrap_err();
        assert!(error.contains("End of File must follow Update File"));
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

    /// Verifies that adding an already existing file is rejected.
    #[tokio::test]
    async fn apply_patch_rejects_file_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("exists.txt"), "old").unwrap();
        let patch = "*** Begin Patch\n*** Add File: exists.txt\n+new\n*** End Patch";

        let error = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap_err();

        assert!(error.contains("already exists"));
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

    /// Verifies that move-only updates are rejected as empty update hunks.
    #[tokio::test]
    async fn apply_patch_update_update_move_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("move.txt"), "move me").unwrap();
        let patch =
            "*** Begin Patch\n*** Update File: move.txt\n*** Move to: moved.txt\n*** End Patch";

        let error = ApplyPatch::new()
            .execute(
                serde_json::json!({"patchText": patch}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap_err();

        assert!(error.contains("no chunks"));
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

        let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Update File: modify.txt\n@@ two\n two\n-three\n+THREE\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved/renamed.txt\n@@\n move me\n*** End Patch";
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
                dir.path().join("move.txt").display()
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
            "move me"
        );
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
}

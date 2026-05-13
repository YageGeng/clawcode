//! Built-in tool for exact string edits.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::Tool;

const EDIT_DESCRIPTION: &str = r#"Performs exact string replacements in files.

Usage:
- You must use your `Read` tool at least once in the conversation before editing. This tool will error if you attempt an edit without reading the file.
- When editing text from Read tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: line number + tab. Everything after that is the actual file content to match. Never include any part of the line number prefix in the oldString or newString.
- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.
- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked.
- The edit will FAIL if `oldString` is not found in the file with an error "oldString not found in content".
- The edit will FAIL if `oldString` is found multiple times in the file with an error "Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match." Either provide a larger string with more surrounding context to make it unique or use `replaceAll` to change every instance of `oldString`.
- Use `replaceAll` for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance."#;

/// Performs exact string replacements in files.
pub struct EditFile;

impl EditFile {
    /// Create a new edit tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        EDIT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "The absolute path to the file to modify" },
                "oldString": { "type": "string", "description": "The text to replace" },
                "newString": { "type": "string", "description": "The text to replace it with (must be different from oldString)" },
                "replaceAll": { "type": "boolean", "description": "Replace all occurrences of oldString (default false)" }
            },
            "required": ["filePath", "oldString", "newString"]
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
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            file_path: String,
            old_string: String,
            new_string: String,
            #[serde(default)]
            replace_all: bool,
        }

        let args: Args =
            serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;

        let resolved = resolve_file_path(&ctx.cwd, &args.file_path);

        if args.old_string == args.new_string {
            return Err("oldString and newString must be different".to_string());
        }

        if args.old_string.is_empty() {
            let content = normalize_for_existing_line_endings(&resolved, &args.new_string).await?;
            write_full_file(&resolved, &content).await?;
            return Ok(format!(
                "edited {}: wrote {} bytes",
                resolved.display(),
                content.len()
            ));
        }

        let resolved = fs::canonicalize(&resolved)
            .await
            .map_err(|e| format!("failed to resolve {}: {e}", resolved.display()))?;

        // Read the file and normalize line endings so CRLF/LF mismatches
        // between the model-supplied oldString and the on-disk content
        // do not cause spurious failures.
        let original = fs::read_to_string(&resolved)
            .await
            .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;

        let line_ending = detect_line_ending(&original);
        let old_string = normalize_line_endings(&args.old_string, line_ending);
        let new_string = normalize_line_endings(&args.new_string, line_ending);

        // Run the 9-level matching pipeline. When replaceAll is set,
        // collect every occurrence; otherwise demand a single unique match.
        let (result, match_count) = if args.replace_all {
            let candidates = find_replace_all_matches(&original, &old_string)?;
            // Apply from back to front so earlier byte offsets stay valid.
            (
                apply_replacements(&original, &candidates, &new_string),
                candidates.len(),
            )
        } else {
            let (offset, actual_search) = find_unique_match(&original, &old_string)?;
            (
                apply_single_replacement(&original, offset, actual_search, &new_string),
                1,
            )
        };
        fs::write(&resolved, &result)
            .await
            .map_err(|e| format!("failed to write {}: {e}", resolved.display()))?;
        Ok(format!(
            "edited {}: replaced {match_count} occurrence(s)",
            resolved.display()
        ))
    }
}

type ReplacerFn = for<'content> fn(&'content str, &str) -> Vec<(usize, &'content str)>;

const MULTIPLE_MATCHES_ERROR: &str = "Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match.";

/// Find a unique match by trying increasingly permissive replacer strategies.
fn find_unique_match<'content>(
    content: &'content str,
    old_str: &str,
) -> Result<(usize, &'content str), String> {
    let replacers: &[ReplacerFn] = &[
        simple_replacer,
        line_trimmed_replacer,
        block_anchor_replacer,
        whitespace_normalized_replacer,
        indentation_flexible_replacer,
        escape_normalized_replacer,
        trimmed_boundary_replacer,
        context_aware_replacer,
    ];
    let mut saw_multiple = false;
    for replacer in replacers {
        let candidates = replacer(content, old_str);
        if candidates.is_empty() {
            continue;
        }
        if candidates.len() == 1 {
            return Ok(candidates[0]);
        }
        // Multiple candidates are not immediately fatal because later strategies may disambiguate.
        saw_multiple = true;
    }
    if saw_multiple {
        Err(MULTIPLE_MATCHES_ERROR.to_string())
    } else {
        Err("oldString not found in content".to_string())
    }
}

/// Find all exact matches for replaceAll mode.
fn find_replace_all_matches<'content>(
    content: &'content str,
    old_str: &str,
) -> Result<Vec<(usize, &'content str)>, String> {
    let candidates = multi_occurrence_replacer(content, old_str);
    if candidates.is_empty() {
        Err("oldString not found in content".to_string())
    } else {
        Ok(candidates)
    }
}

/// Apply one replacement by byte offset and original matched text.
fn apply_single_replacement(
    content: &str,
    offset: usize,
    actual_search: &str,
    new_string: &str,
) -> String {
    format!(
        "{}{}{}",
        &content[..offset],
        new_string,
        &content[offset + actual_search.len()..]
    )
}

/// Apply multiple non-overlapping replacements from back to front.
fn apply_replacements(content: &str, candidates: &[(usize, &str)], new_string: &str) -> String {
    let mut result = content.to_string();
    for (offset, actual_search) in candidates.iter().rev() {
        result.replace_range(*offset..*offset + actual_search.len(), new_string);
    }
    result
}

/// Match exact substrings in the content.
fn simple_replacer<'content>(content: &'content str, old_str: &str) -> Vec<(usize, &'content str)> {
    content.match_indices(old_str).collect()
}

/// Match line sequences after trimming each corresponding line.
fn line_trimmed_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_lines = old_str.lines().collect::<Vec<_>>();
    find_line_windows(content, old_str, old_lines.len(), |block_lines| {
        block_lines
            .iter()
            .zip(old_lines.iter())
            .all(|(content_line, old_line)| content_line.trim() == old_line.trim())
    })
}

/// Match multi-line blocks by first/last anchors and middle-line edit similarity.
fn block_anchor_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_lines = old_str.lines().collect::<Vec<_>>();
    if old_lines.len() < 3 {
        return Vec::new();
    }
    let mut candidates = find_line_windows_with_score(content, old_str, old_lines.len(), |block| {
        if block.first().map(|line| line.trim()) != old_lines.first().map(|line| line.trim())
            || block.last().map(|line| line.trim()) != old_lines.last().map(|line| line.trim())
        {
            return None;
        }
        Some(middle_similarity(&old_lines, block))
    });
    if candidates.len() == 1 {
        return vec![candidates.remove(0).0];
    }
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    candidates
        .into_iter()
        .next()
        .filter(|(_, score)| *score >= 0.3)
        .map(|(candidate, _)| vec![candidate])
        .unwrap_or_default()
}

/// Match text after normalizing whitespace runs to one ASCII space.
fn whitespace_normalized_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_line_count = old_str.lines().count().max(1);
    let old_normalized = normalize_whitespace(old_str);
    find_line_windows(content, old_str, old_line_count, |block_lines| {
        normalize_whitespace(&block_lines.join(
            "
",
        )) == old_normalized
    })
}

/// Match line blocks after removing their minimum common indentation.
fn indentation_flexible_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_line_count = old_str.lines().count().max(1);
    let old_normalized = strip_min_common_indentation(old_str);
    find_line_windows(content, old_str, old_line_count, |block_lines| {
        strip_min_common_indentation(&block_lines.join(
            "
",
        )) == old_normalized
    })
}

/// Match after interpreting common escaped sequences in both pattern and content.
fn escape_normalized_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_unescaped = unescape_common_sequences(old_str);
    let (normalized_content, byte_map) = normalize_escaped_content(content);
    normalized_content
        .match_indices(&old_unescaped)
        .filter_map(|(normalized_offset, actual)| {
            let normalized_end = normalized_offset + actual.len();
            let original_start = byte_map.get(normalized_offset).map(|range| range.0)?;
            let original_end = byte_map
                .get(normalized_end.checked_sub(1)?)
                .map(|range| range.1)?;
            Some((original_start, &content[original_start..original_end]))
        })
        .collect()
}

/// Match after trimming only the oldString boundaries.
fn trimmed_boundary_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let trimmed = old_str.trim();
    if trimmed.is_empty() || trimmed == old_str {
        Vec::new()
    } else {
        simple_replacer(content, trimmed)
    }
}

/// Match blocks using first/last context and at least half of middle non-empty lines.
fn context_aware_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    let old_lines = old_str.lines().collect::<Vec<_>>();
    if old_lines.len() < 3 {
        return Vec::new();
    }
    find_line_windows(content, old_str, old_lines.len(), |block| {
        if block.first().map(|line| line.trim()) != old_lines.first().map(|line| line.trim())
            || block.last().map(|line| line.trim()) != old_lines.last().map(|line| line.trim())
        {
            return false;
        }
        let middle_old = &old_lines[1..old_lines.len() - 1];
        let middle_block = &block[1..block.len() - 1];
        let required = middle_old
            .iter()
            .filter(|line| !line.trim().is_empty())
            .count();
        if required == 0 {
            return true;
        }
        let matched = middle_old
            .iter()
            .zip(middle_block.iter())
            .filter(|(old, actual)| !old.trim().is_empty() && old.trim() == actual.trim())
            .count();
        matched * 2 >= required
    })
}

/// Collect every exact occurrence for replaceAll mode.
fn multi_occurrence_replacer<'content>(
    content: &'content str,
    old_str: &str,
) -> Vec<(usize, &'content str)> {
    simple_replacer(content, old_str)
}

/// Search same-length line windows and return original content slices for matches.
fn find_line_windows<'content, F>(
    content: &'content str,
    old_str: &str,
    line_count: usize,
    predicate: F,
) -> Vec<(usize, &'content str)>
where
    F: Fn(&[&str]) -> bool,
{
    find_line_windows_with_score(content, old_str, line_count, |block| {
        predicate(block).then_some(1.0)
    })
    .into_iter()
    .map(|(candidate, _)| candidate)
    .collect()
}

/// Search line windows and keep a score for strategies that need ranking.
fn find_line_windows_with_score<'content, F>(
    content: &'content str,
    old_str: &str,
    line_count: usize,
    scorer: F,
) -> Vec<((usize, &'content str), f64)>
where
    F: Fn(&[&str]) -> Option<f64>,
{
    if line_count == 0 {
        return Vec::new();
    }
    let spans = line_spans(content);
    if spans.len() < line_count {
        return Vec::new();
    }
    (0..=spans.len() - line_count)
        .filter_map(|start_index| {
            let block = spans[start_index..start_index + line_count]
                .iter()
                .map(|(_, _, _, line)| *line)
                .collect::<Vec<_>>();
            let score = scorer(&block)?;
            let start = spans[start_index].0;
            let end = matched_line_end(&spans, start_index, line_count, old_str);
            Some(((start, &content[start..end]), score))
        })
        .collect()
}

/// Return line spans as start, content-end, next-line-start, and text without line endings.
fn line_spans(content: &str) -> Vec<(usize, usize, usize, &str)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for segment in content.split_inclusive('\n') {
        let next_offset = offset + segment.len();
        let line = segment.trim_end_matches(['\r', '\n']);
        let line_end = offset + line.len();
        spans.push((offset, line_end, next_offset, line));
        offset = next_offset;
    }
    if offset < content.len() {
        let line = &content[offset..];
        spans.push((offset, content.len(), content.len(), line));
    }
    spans
}

/// Choose whether a matched line block should include the trailing line ending.
fn matched_line_end(
    spans: &[(usize, usize, usize, &str)],
    start_index: usize,
    line_count: usize,
    old_str: &str,
) -> usize {
    let last = spans[start_index + line_count - 1];
    if old_str.ends_with('\n') {
        last.2
    } else {
        last.1
    }
}

/// Convert all whitespace runs to a single ASCII space.
fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove the minimum shared indentation from non-empty lines.
fn strip_min_common_indentation(value: &str) -> String {
    let lines = value.lines().collect::<Vec<_>>();
    let min_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|ch| ch.is_whitespace()).count())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| line.chars().skip(min_indent).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compute a normalized similarity score for middle lines in a block.
fn middle_similarity(old_lines: &[&str], content_lines: &[&str]) -> f64 {
    let old_middle = old_lines[1..old_lines.len() - 1]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let content_middle = content_lines[1..content_lines.len() - 1]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let max_len = old_middle
        .chars()
        .count()
        .max(content_middle.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein_distance(&old_middle, &content_middle) as f64 / max_len as f64)
}

/// Unescape common model-provided escape sequences.
fn unescape_common_sequences(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

/// Normalize escaped content while preserving a byte map back to the original string.
fn normalize_escaped_content(content: &str) -> (String, Vec<(usize, usize)>) {
    let mut output = String::new();
    let mut byte_map = Vec::new();
    let mut iterator = content.char_indices().peekable();
    while let Some((start, ch)) = iterator.next() {
        let original_end = iterator
            .peek()
            .map(|(index, _)| *index)
            .unwrap_or(content.len());
        let (normalized, end) = if ch == '\\' {
            if let Some(&(next_start, next_ch)) = iterator.peek() {
                let next_end = next_start + next_ch.len_utf8();
                match next_ch {
                    'n' => {
                        iterator.next();
                        ('\n', next_end)
                    }
                    't' => {
                        iterator.next();
                        ('\t', next_end)
                    }
                    '"' => {
                        iterator.next();
                        ('"', next_end)
                    }
                    '\\' => {
                        iterator.next();
                        ('\\', next_end)
                    }
                    _ => (ch, original_end),
                }
            } else {
                (ch, original_end)
            }
        } else {
            (ch, original_end)
        };
        let mut encoded = [0; 4];
        let normalized_bytes = normalized.encode_utf8(&mut encoded).len();
        output.push(normalized);
        byte_map.extend(std::iter::repeat_n((start, end), normalized_bytes));
    }
    (output, byte_map)
}

/// Compute Levenshtein edit distance with a standard dynamic-programming table.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev = (0..=b_chars.len()).collect::<Vec<_>>();
    let mut curr = vec![0; b_chars.len() + 1];
    for (i, ca) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min((prev[j + 1] + 1).min(prev[j] + cost));
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

/// Resolve a file tool path relative to the execution working directory.
fn resolve_file_path(cwd: &Path, path: &str) -> PathBuf {
    if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    }
}

/// Write a full file and create parent directories when necessary.
async fn write_full_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("failed to create parent dir: {e}"))?;
    }
    fs::write(path, content)
        .await
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// Normalize new content to an existing file's line endings when the target exists.
async fn normalize_for_existing_line_endings(path: &Path, content: &str) -> Result<String, String> {
    if fs::try_exists(path)
        .await
        .map_err(|e| format!("failed to inspect {}: {e}", path.display()))?
    {
        let original = fs::read_to_string(path)
            .await
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        Ok(normalize_line_endings(
            content,
            detect_line_ending(&original),
        ))
    } else {
        Ok(content.to_string())
    }
}

/// Detect whether file content primarily uses CRLF or LF line endings.
fn detect_line_ending(content: &str) -> LineEnding {
    if content.contains("\r\n") {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

/// Normalize arbitrary incoming line endings to the target file's convention.
fn normalize_line_endings(value: &str, line_ending: LineEnding) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    match line_ending {
        LineEnding::Lf => normalized,
        LineEnding::CrLf => normalized.replace('\n', "\r\n"),
    }
}

#[derive(Debug, Clone, Copy)]
enum LineEnding {
    Lf,
    CrLf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    /// Verifies that the edit tool replaces only the requested exact text.
    #[tokio::test]
    async fn edit_file_replaces_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patch.txt");

        std::fs::write(&path, "before\ntarget\nafter").unwrap();

        let tool = EditFile::new();
        let result = tool
            .execute(
                serde_json::json!({"filePath": "patch.txt", "oldString": "target", "newString": "REPLACED"}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.contains("edited"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "before\nREPLACED\nafter"
        );
    }

    /// Verifies that missing edit context is reported as the opencode error.
    #[tokio::test]
    async fn edit_file_search_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        std::fs::write(&path, "hello").unwrap();

        let tool = EditFile::new();
        let result = tool
            .execute(
                serde_json::json!({"filePath": "nope.txt", "oldString": "xyz", "newString": "abc"}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        assert_eq!(result.unwrap_err(), "oldString not found in content");
    }

    /// Verifies that edit operations always go through approval.
    #[tokio::test]
    async fn edit_file_always_requires_approval() {
        let tool = EditFile::new();
        assert!(tool.needs_approval(
            &serde_json::json!({
                "filePath": "test.txt",
                "oldString": "t",
                "newString": "r"
            }),
            &ToolContext::for_test(Path::new(".")),
        ));
    }

    /// Verifies that edit writes the whole file when oldString is empty.
    #[tokio::test]
    async fn edit_file_empty_old_string_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = EditFile::new();

        tool.execute(
            serde_json::json!({"filePath": "new.txt", "oldString": "", "newString": "created"}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "created"
        );
    }

    /// Verifies that edit rejects ambiguous matches without replaceAll.
    #[tokio::test]
    async fn edit_file_rejects_multiple_matches_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dupe.txt"), "a\na\n").unwrap();
        let tool = EditFile::new();

        let result = tool
            .execute(
                serde_json::json!({"filePath": "dupe.txt", "oldString": "a", "newString": "b"}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        assert_eq!(
            result.unwrap_err(),
            "Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match."
        );
    }

    /// Verifies that replaceAll changes every exact match.
    #[tokio::test]
    async fn edit_file_replace_all_replaces_every_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("all.txt"), "a\na\n").unwrap();
        let tool = EditFile::new();

        tool.execute(
            serde_json::json!({"filePath": "all.txt", "oldString": "a", "newString": "b", "replaceAll": true}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("all.txt")).unwrap(),
            "b\nb\n"
        );
    }

    /// Verifies that edit normalizes incoming strings to CRLF files.
    #[tokio::test]
    async fn edit_file_normalizes_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
        let tool = EditFile::new();

        tool.execute(
            serde_json::json!({"filePath": "crlf.txt", "oldString": "one\ntwo", "newString": "one\nTWO"}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("crlf.txt")).unwrap(),
            "one\r\nTWO\r\n"
        );
    }

    /// Verifies that line-trimmed matching can replace uniquely padded content.
    #[tokio::test]
    async fn edit_file_uses_line_trimmed_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("trimmed.txt"), "before\n  target  \nafter").unwrap();
        let tool = EditFile::new();

        tool.execute(
            serde_json::json!({"filePath": "trimmed.txt", "oldString": "target\n", "newString": "replacement\n"}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("trimmed.txt")).unwrap(),
            "before\nreplacement\nafter"
        );
    }

    /// Verifies that whitespace-normalized matching can replace flexible spacing.
    #[tokio::test]
    async fn edit_file_uses_whitespace_normalized_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("space.txt"), "hello   world").unwrap();
        let tool = EditFile::new();

        tool.execute(
            serde_json::json!({"filePath": "space.txt", "oldString": "hello world", "newString": "hello rust"}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("space.txt")).unwrap(),
            "hello rust"
        );
    }

    /// Verifies that edit rejects no-op replacements.
    #[tokio::test]
    async fn edit_file_rejects_identical_old_and_new_strings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("same.txt"), "same").unwrap();
        let tool = EditFile::new();

        let result = tool
            .execute(
                serde_json::json!({"filePath": "same.txt", "oldString": "same", "newString": "same"}),
                &ToolContext::for_test(dir.path()),
            )
            .await;

        assert!(result.unwrap_err().contains("must be different"));
    }
}

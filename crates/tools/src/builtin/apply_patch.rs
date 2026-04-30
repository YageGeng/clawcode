use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    ApprovalRequirement, Result, RiskLevel,
    context::{
        ApplyPatchFileMetadata, ApplyPatchMetadata, ApplyPatchStructuredOutput,
        StructuredToolOutput, ToolInvocation, ToolMetadata, ToolOutput,
    },
    error::{ToolExecutionSnafu, ToolIoSnafu},
    handler::ToolHandler,
};

/// Parses arguments for the built-in `apply_patch` tool.
#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    #[serde(default, rename = "patchText", alias = "patch")]
    patch_text: Option<String>,
}

/// Applies structured patch streams directly against the local workspace root.
pub struct ApplyPatchTool {
    root_dir: PathBuf,
}

/// Captures one parsed file operation from the patch stream.
enum PatchAction {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

/// Stores one parsed update hunk after old/new lines have been normalized.
struct UpdateFileChunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    change_context: Option<String>,
    is_end_of_file: bool,
}

/// Keeps UTF-8 file text together with its BOM state so writes can preserve it.
struct TextFileContents {
    text: String,
    has_bom: bool,
}

/// Describes one verified file mutation ready to be written to disk.
struct VerifiedChange {
    source_path: String,
    absolute_source_path: PathBuf,
    move_path: Option<String>,
    absolute_move_path: Option<PathBuf>,
    operation_type: &'static str,
    summary_line: String,
    diff: String,
    additions: usize,
    deletions: usize,
    new_contents: Option<TextFileContents>,
}

/// Tracks a deferred replacement while validating an update file action.
struct Replacement {
    start_idx: usize,
    old_len: usize,
    new_lines: Vec<String>,
}

/// Declares the supported fuzzy line comparators used by patch matching.
#[derive(Clone, Copy)]
enum LineComparator {
    Exact,
    TrimEnd,
    Trim,
    NormalizedUnicode,
}

impl ApplyPatchTool {
    /// Creates an apply-patch tool rooted at the provided workspace directory.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    /// Resolves all patch actions, verifies them in memory, then writes the final results.
    pub(crate) fn apply_patch_text(&self, patch: &str) -> Result<ToolOutput> {
        let actions = parse_patch_text(patch, self.name())?;
        let verified_changes = self.verify_actions(&actions)?;
        self.write_verified_changes(&verified_changes)?;
        Ok(render_tool_output(&verified_changes))
    }

    /// Verifies every action before any writes happen so patch application remains atomic.
    fn verify_actions(&self, actions: &[PatchAction]) -> Result<Vec<VerifiedChange>> {
        let mut verified_changes = Vec::with_capacity(actions.len());
        for action in actions {
            verified_changes.push(self.verify_action(action)?);
        }
        Ok(verified_changes)
    }

    /// Verifies one action and prepares the metadata and final content needed for writing.
    fn verify_action(&self, action: &PatchAction) -> Result<VerifiedChange> {
        match action {
            PatchAction::Add { path, contents } => self.verify_add(path, contents),
            PatchAction::Delete { path } => self.verify_delete(path),
            PatchAction::Update {
                path,
                move_to,
                chunks,
            } => self.verify_update(path, move_to.as_deref(), chunks),
        }
    }

    /// Verifies a new-file addition and precomputes the unified diff metadata.
    fn verify_add(&self, path: &str, contents: &str) -> Result<VerifiedChange> {
        let absolute_path = self.resolve_write_path(path, "apply-patch-add-path")?;
        let normalized_contents = ensure_trailing_newline(contents);
        let diff = render_unified_diff(None, Some(path), "", &normalized_contents);
        Ok(VerifiedChange {
            source_path: path.to_string(),
            absolute_source_path: absolute_path,
            move_path: None,
            absolute_move_path: None,
            operation_type: "add",
            summary_line: format!("A {path}"),
            additions: count_file_lines(&normalized_contents),
            deletions: 0,
            diff,
            new_contents: Some(TextFileContents {
                text: normalized_contents,
                has_bom: false,
            }),
        })
    }

    /// Verifies a delete operation and captures the original file for diff reporting.
    fn verify_delete(&self, path: &str) -> Result<VerifiedChange> {
        let absolute_path = self.resolve_existing_path(
            path,
            "apply-patch-delete-path",
            format!("apply_patch verification failed: Failed to read file to delete: {path}"),
        )?;
        let original =
            self.read_text_contents(&absolute_path, "apply-patch-delete-read", path, "delete")?;
        let diff = render_unified_diff(Some(path), None, &original.text, "");
        Ok(VerifiedChange {
            source_path: path.to_string(),
            absolute_source_path: absolute_path,
            move_path: None,
            absolute_move_path: None,
            operation_type: "delete",
            summary_line: format!("D {path}"),
            additions: 0,
            deletions: count_file_lines(&original.text),
            diff,
            new_contents: None,
        })
    }

    /// Verifies an in-place update or move before any file-system mutation occurs.
    fn verify_update(
        &self,
        path: &str,
        move_to: Option<&str>,
        chunks: &[UpdateFileChunk],
    ) -> Result<VerifiedChange> {
        let absolute_source_path = self.resolve_existing_path(
            path,
            "apply-patch-update-path",
            format!("apply_patch verification failed: Failed to read file to update: {path}"),
        )?;
        let original = self.read_text_contents(
            &absolute_source_path,
            "apply-patch-update-read",
            path,
            "update",
        )?;
        let updated_text =
            derive_new_contents_from_chunks(self.name(), path, &original.text, chunks)?;
        let resolved_move = match move_to {
            Some(target) if target != path => {
                Some(self.resolve_write_path(target, "apply-patch-move-path")?)
            }
            _ => None,
        };
        let (display_target, operation_type, summary_line) = match move_to {
            Some(target) if target != path => {
                (target.to_string(), "move", format!("M {path} -> {target}"))
            }
            _ => (path.to_string(), "update", format!("M {path}")),
        };
        let diff = render_unified_diff(
            Some(path),
            Some(display_target.as_str()),
            &original.text,
            &updated_text,
        );
        let (additions, deletions) = count_chunk_changes(chunks);
        Ok(VerifiedChange {
            source_path: path.to_string(),
            absolute_source_path,
            move_path: resolved_move.as_ref().map(|_| display_target),
            absolute_move_path: resolved_move,
            operation_type,
            summary_line,
            additions,
            deletions,
            diff,
            new_contents: Some(TextFileContents {
                text: updated_text,
                has_bom: original.has_bom,
            }),
        })
    }

    /// Writes all verified changes to disk after validation has succeeded for the full patch.
    fn write_verified_changes(&self, verified_changes: &[VerifiedChange]) -> Result<()> {
        for change in verified_changes {
            match change.operation_type {
                "add" => self.write_text_contents(
                    &change.absolute_source_path,
                    change
                        .new_contents
                        .as_ref()
                        .expect("add keeps new contents"),
                    "apply-patch-add-write",
                )?,
                "delete" => fs::remove_file(&change.absolute_source_path).context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "apply-patch-delete-file".to_string(),
                })?,
                "update" => self.write_text_contents(
                    &change.absolute_source_path,
                    change
                        .new_contents
                        .as_ref()
                        .expect("update keeps replacement text"),
                    "apply-patch-update-write",
                )?,
                "move" => {
                    let destination = change
                        .absolute_move_path
                        .as_ref()
                        .expect("move keeps destination path");
                    self.write_text_contents(
                        destination,
                        change
                            .new_contents
                            .as_ref()
                            .expect("move keeps replacement text"),
                        "apply-patch-move-write",
                    )?;
                    fs::remove_file(&change.absolute_source_path).context(ToolIoSnafu {
                        tool: self.name().to_string(),
                        stage: "apply-patch-move-remove-source".to_string(),
                    })?;
                }
                _ => unreachable!("unsupported verified change type"),
            }
        }
        Ok(())
    }

    /// Writes a UTF-8 text file while preserving the original BOM choice.
    fn write_text_contents(
        &self,
        path: &Path,
        contents: &TextFileContents,
        stage: &'static str,
    ) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: format!("{stage}-parent"),
            })?;
        }

        let mut bytes = Vec::new();
        if contents.has_bom {
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        bytes.extend_from_slice(contents.text.as_bytes());
        fs::write(path, bytes).context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: stage.to_string(),
        })?;
        Ok(())
    }

    /// Reads one UTF-8 text file and strips an optional UTF-8 BOM for patch processing.
    fn read_text_contents(
        &self,
        path: &Path,
        stage: &'static str,
        display_path: &str,
        operation: &str,
    ) -> Result<TextFileContents> {
        let bytes = fs::read(path).map_err(|_| {
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: format!(
                    "apply_patch verification failed: Failed to read file to {operation}: {display_path}"
                ),
            }
            .build()
        })?;
        let (has_bom, text_bytes) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (true, &bytes[3..])
        } else {
            (false, bytes.as_slice())
        };
        let text = String::from_utf8(text_bytes.to_vec()).map_err(|_| {
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: format!(
                    "apply_patch verification failed: file is not valid UTF-8: {display_path}"
                ),
            }
            .build()
        })?;
        Ok(TextFileContents { text, has_bom })
    }

    /// Resolves an existing path under the workspace root and rejects traversal or symlink escape.
    fn resolve_existing_path(
        &self,
        raw_path: &str,
        stage: &'static str,
        missing_message: String,
    ) -> Result<PathBuf> {
        let canonical_root = self.canonical_root(stage)?;
        let relative_path = validate_relative_path(raw_path, self.name(), stage)?;
        let candidate = canonical_root.join(relative_path);
        let canonical_candidate = candidate.canonicalize().map_err(|_| {
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: missing_message.clone(),
            }
            .build()
        })?;
        ensure!(
            canonical_candidate.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "target must stay inside the tool root".to_string(),
            }
        );
        Ok(canonical_candidate)
    }

    /// Resolves a write path under the workspace root while guarding against parent symlink escape.
    fn resolve_write_path(&self, raw_path: &str, stage: &'static str) -> Result<PathBuf> {
        let canonical_root = self.canonical_root(stage)?;
        let relative_path = validate_relative_path(raw_path, self.name(), stage)?;
        let candidate = canonical_root.join(&relative_path);
        let existing_parent = nearest_existing_parent(&candidate).context(ToolExecutionSnafu {
            tool: self.name().to_string(),
            stage: stage.to_string(),
            message: "target must stay inside the tool root".to_string(),
        })?;
        let canonical_parent = existing_parent.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: format!("{stage}-parent-canonicalize"),
        })?;
        ensure!(
            canonical_parent.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "target must stay inside the tool root".to_string(),
            }
        );
        Ok(candidate)
    }

    /// Resolves and canonicalizes the workspace root once per path operation.
    fn canonical_root(&self, stage: &'static str) -> Result<PathBuf> {
        self.root_dir.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: format!("{stage}-root-canonicalize"),
        })
    }
}

#[async_trait]
impl ToolHandler for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Apply a structured patch that can add, delete, update, or move workspace files."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Edit workspace files by applying structured patch blocks.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patchText": {
                    "type": "string",
                    "description": "完整的 patch 文本，描述所有文件变更。"
                }
            },
            "required": ["patchText"]
        })
    }

    /// Marks the patch tool as high risk because it can mutate multiple files at once.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Parses a patch stream and applies it relative to this tool's configured root.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: ApplyPatchArgs = invocation.parse_function_arguments("apply-patch-parse-args")?;
        let patch_text = args.patch_text.context(ToolExecutionSnafu {
            tool: self.name().to_string(),
            stage: "apply-patch-parse-args".to_string(),
            message: "missing required argument `patchText`".to_string(),
        })?;
        self.apply_patch_text(&patch_text)
    }
}

/// Parses a complete patch string after extracting the `*** Begin Patch` envelope.
fn parse_patch_text(patch_text: &str, tool: &'static str) -> Result<Vec<PatchAction>> {
    let patch_lines = extract_patch_lines(patch_text, tool)?;
    let mut actions = Vec::new();
    let mut index = 0usize;

    while index < patch_lines.len() {
        let line = patch_lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut content_lines = Vec::new();
            while index < patch_lines.len() && !patch_lines[index].starts_with("*** ") {
                let added = patch_lines[index]
                    .strip_prefix('+')
                    .context(ToolExecutionSnafu {
                        tool: tool.to_string(),
                        stage: "apply-patch-parse-add".to_string(),
                        message:
                            "apply_patch verification failed: add file body must use `+` lines"
                                .to_string(),
                    })?;
                content_lines.push(added.to_string());
                index += 1;
            }
            actions.push(PatchAction::Add {
                path: path.to_string(),
                contents: ensure_trailing_newline(&content_lines.join("\n")),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            actions.push(PatchAction::Delete {
                path: path.to_string(),
            });
            index += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut move_to = None;
            if index < patch_lines.len()
                && let Some(target) = patch_lines[index].strip_prefix("*** Move to: ")
            {
                move_to = Some(target.to_string());
                index += 1;
            }
            let mut chunks = Vec::new();
            while index < patch_lines.len() && !patch_lines[index].starts_with("*** ") {
                let hunk_header = patch_lines[index];
                ensure!(
                    hunk_header.starts_with("@@"),
                    ToolExecutionSnafu {
                        tool: tool.to_string(),
                        stage: "apply-patch-parse-update".to_string(),
                        message: format!(
                            "apply_patch verification failed: invalid update hunk header `{hunk_header}`"
                        ),
                    }
                );
                index += 1;
                chunks.push(parse_update_chunk(
                    &patch_lines,
                    &mut index,
                    hunk_header,
                    tool,
                )?);
            }
            actions.push(PatchAction::Update {
                path: path.to_string(),
                move_to,
                chunks,
            });
            continue;
        }

        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: "apply-patch-parse".to_string(),
            message: format!("apply_patch verification failed: unsupported patch header `{line}`"),
        }
        .fail();
    }

    ensure!(
        !actions.is_empty(),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: "apply-patch-parse".to_string(),
            message: "patch rejected: empty patch".to_string(),
        }
    );
    Ok(actions)
}

/// Extracts only the patch body between the `*** Begin Patch` and `*** End Patch` markers.
fn extract_patch_lines<'a>(patch_text: &'a str, tool: &'static str) -> Result<Vec<&'a str>> {
    let lines = patch_text.lines().collect::<Vec<_>>();
    let begin_index = lines.iter().position(|line| *line == "*** Begin Patch");
    let end_index = lines.iter().rposition(|line| *line == "*** End Patch");
    let (begin_index, end_index) = match (begin_index, end_index) {
        (Some(begin_index), Some(end_index)) if begin_index < end_index => (begin_index, end_index),
        _ => {
            return ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: "apply-patch-parse".to_string(),
                message: "apply_patch verification failed: missing Begin/End markers".to_string(),
            }
            .fail();
        }
    };
    Ok(lines[begin_index + 1..end_index].to_vec())
}

/// Parses one update chunk, preserving the optional `@@ ...` context hint and EOF anchor.
fn parse_update_chunk(
    patch_lines: &[&str],
    index: &mut usize,
    hunk_header: &str,
    tool: &'static str,
) -> Result<UpdateFileChunk> {
    let change_context = hunk_header
        .strip_prefix("@@")
        .map(str::trim_start)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string);
    let mut old_lines = Vec::new();
    let mut new_lines = Vec::new();
    let mut is_end_of_file = false;

    while *index < patch_lines.len()
        && !patch_lines[*index].starts_with("@@")
        && !patch_lines[*index].starts_with("*** ")
    {
        let raw = patch_lines[*index];
        if raw == "*** End of File" {
            is_end_of_file = true;
            *index += 1;
            break;
        }

        let (prefix, content) = raw.split_at(1);
        match prefix {
            " " => {
                old_lines.push(content.to_string());
                new_lines.push(content.to_string());
            }
            "-" => old_lines.push(content.to_string()),
            "+" => new_lines.push(content.to_string()),
            _ => {
                return ToolExecutionSnafu {
                    tool: tool.to_string(),
                    stage: "apply-patch-parse-update".to_string(),
                    message: format!(
                        "apply_patch verification failed: invalid update line `{raw}`"
                    ),
                }
                .fail();
            }
        }
        *index += 1;
    }

    Ok(UpdateFileChunk {
        old_lines,
        new_lines,
        change_context,
        is_end_of_file,
    })
}

/// Applies verified replacements in memory and returns the final file content.
fn derive_new_contents_from_chunks(
    tool: &'static str,
    path: &str,
    original: &str,
    chunks: &[UpdateFileChunk],
) -> Result<String> {
    let old_lines = split_content_lines(original);
    let replacements = compute_replacements(tool, path, &old_lines, chunks)?;
    let mut new_lines = old_lines.clone();

    // Apply replacements from the back so earlier indices stay stable while mutating the vector.
    for replacement in replacements.iter().rev() {
        new_lines.splice(
            replacement.start_idx..replacement.start_idx + replacement.old_len,
            replacement.new_lines.clone(),
        );
    }

    Ok(render_lines_with_trailing_newline(&new_lines))
}

/// Computes all chunk replacements before mutating any file content.
fn compute_replacements(
    tool: &'static str,
    path: &str,
    old_lines: &[String],
    chunks: &[UpdateFileChunk],
) -> Result<Vec<Replacement>> {
    let mut replacements = Vec::with_capacity(chunks.len());
    let mut line_index = 0usize;

    for chunk in chunks {
        let search_start = if let Some(change_context) = chunk.change_context.as_deref() {
            seek_context_line(old_lines, change_context, line_index).context(
                ToolExecutionSnafu {
                    tool: tool.to_string(),
                    stage: "apply-patch-update-context".to_string(),
                    message: format!(
                        "apply_patch verification failed: Failed to find expected lines in {path}"
                    ),
                },
            )?
        } else {
            line_index
        };

        let start_idx = if chunk.old_lines.is_empty() {
            old_lines.len()
        } else {
            seek_sequence(
                old_lines,
                &chunk.old_lines,
                search_start,
                chunk.is_end_of_file,
            )
            .context(ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: "apply-patch-update-match".to_string(),
                message: format!(
                    "apply_patch verification failed: Failed to find expected lines in {path}"
                ),
            })?
        };
        line_index = start_idx + chunk.old_lines.len();
        replacements.push(Replacement {
            start_idx,
            old_len: chunk.old_lines.len(),
            new_lines: chunk.new_lines.clone(),
        });
    }

    replacements.sort_unstable_by_key(|replacement| replacement.start_idx);
    Ok(replacements)
}

/// Seeks the next context hint line so later chunk matching starts in the right area.
fn seek_context_line(lines: &[String], change_context: &str, search_start: usize) -> Option<usize> {
    let comparators = [
        LineComparator::Exact,
        LineComparator::TrimEnd,
        LineComparator::Trim,
        LineComparator::NormalizedUnicode,
    ];
    comparators.iter().find_map(|comparator| {
        lines
            .iter()
            .enumerate()
            .skip(search_start)
            .find(|(_, candidate)| compare_lines(candidate, change_context, *comparator))
            .map(|(index, _)| index)
    })
}

/// Seeks the old-line sequence using progressively looser comparators as the spec requires.
fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    search_start: usize,
    is_end_of_file: bool,
) -> Option<usize> {
    let trimmed_pattern = trim_trailing_empty_lines(pattern);
    let candidates = [pattern, trimmed_pattern];
    let comparators = [
        LineComparator::Exact,
        LineComparator::TrimEnd,
        LineComparator::Trim,
        LineComparator::NormalizedUnicode,
    ];

    for candidate in candidates {
        if candidate.is_empty() {
            return Some(lines.len());
        }

        for comparator in comparators {
            if is_end_of_file
                && let Some(index) =
                    seek_sequence_with_comparator(lines, candidate, search_start, comparator, true)
            {
                return Some(index);
            }
            if let Some(index) =
                seek_sequence_with_comparator(lines, candidate, search_start, comparator, false)
            {
                return Some(index);
            }
        }
    }

    None
}

/// Performs one forward or EOF-biased sequence search with a fixed comparator.
fn seek_sequence_with_comparator(
    lines: &[String],
    pattern: &[String],
    search_start: usize,
    comparator: LineComparator,
    prefer_end_of_file: bool,
) -> Option<usize> {
    if pattern.len() > lines.len() {
        return None;
    }

    if prefer_end_of_file {
        let candidate_start = lines.len().saturating_sub(pattern.len());
        if candidate_start >= search_start
            && matches_sequence(
                &lines[candidate_start..candidate_start + pattern.len()],
                pattern,
                comparator,
            )
        {
            return Some(candidate_start);
        }
    }

    let last_start = lines.len().checked_sub(pattern.len())?;
    (search_start..=last_start).find(|&start_idx| {
        matches_sequence(
            &lines[start_idx..start_idx + pattern.len()],
            pattern,
            comparator,
        )
    })
}

/// Compares two line slices with the selected fuzzy-matching strategy.
fn matches_sequence(window: &[String], pattern: &[String], comparator: LineComparator) -> bool {
    window
        .iter()
        .zip(pattern.iter())
        .all(|(left, right)| compare_lines(left, right, comparator))
}

/// Compares two lines with one of the spec-defined matching modes.
fn compare_lines(left: &str, right: &str, comparator: LineComparator) -> bool {
    match comparator {
        LineComparator::Exact => left == right,
        LineComparator::TrimEnd => left.trim_end() == right.trim_end(),
        LineComparator::Trim => left.trim() == right.trim(),
        LineComparator::NormalizedUnicode => {
            normalize_unicode(left.trim()) == normalize_unicode(right.trim())
        }
    }
}

/// Normalizes the limited Unicode punctuation set defined by the patch spec.
fn normalize_unicode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '…' => result.push_str("..."),
            '‘' | '’' | '‚' | '‛' => result.push('\''),
            '“' | '”' | '„' | '‟' => result.push('"'),
            '‐' | '‑' | '‒' | '–' | '—' | '―' => result.push('-'),
            '\u{00A0}' => result.push(' '),
            other => result.push(other),
        }
    }
    result
}

/// Validates a relative workspace path and rejects traversal before path resolution.
fn validate_relative_path(raw_path: &str, tool: &str, stage: &'static str) -> Result<PathBuf> {
    ensure!(
        !raw_path.trim().is_empty(),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path must not be empty".to_string(),
        }
    );

    let requested = Path::new(raw_path);
    ensure!(
        !requested.is_absolute(),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "absolute paths are not allowed".to_string(),
        }
    );
    ensure!(
        !requested
            .components()
            .any(|component| matches!(component, Component::ParentDir)),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path traversal via `..` is not allowed".to_string(),
        }
    );

    Ok(requested.to_path_buf())
}

/// Walks upward until it finds an existing parent path that can be canonicalized safely.
fn nearest_existing_parent(path: &Path) -> Option<&Path> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

/// Splits a text file into logical lines without the trailing newline marker.
fn split_content_lines(contents: &str) -> Vec<String> {
    contents.lines().map(ToString::to_string).collect()
}

/// Renders logical lines back to file text while always keeping a trailing newline.
fn render_lines_with_trailing_newline(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

/// Ensures file content always ends with a newline so the patch tool writes canonical text files.
fn ensure_trailing_newline(contents: &str) -> String {
    if contents.is_empty() {
        return String::new();
    }
    if contents.ends_with('\n') {
        contents.to_string()
    } else {
        format!("{contents}\n")
    }
}

/// Removes trailing empty lines before the fallback sequence search retries.
fn trim_trailing_empty_lines(lines: &[String]) -> &[String] {
    let mut end = lines.len();
    while end > 0 && lines[end - 1].is_empty() {
        end -= 1;
    }
    &lines[..end]
}

/// Counts additions and deletions directly from the parsed chunks.
fn count_chunk_changes(chunks: &[UpdateFileChunk]) -> (usize, usize) {
    chunks
        .iter()
        .fold((0usize, 0usize), |(add_total, del_total), chunk| {
            let prefix_len = count_common_prefix(&chunk.old_lines, &chunk.new_lines);
            let suffix_len = count_common_suffix(&chunk.old_lines, &chunk.new_lines, prefix_len);
            (
                add_total
                    + chunk
                        .new_lines
                        .len()
                        .saturating_sub(prefix_len + suffix_len),
                del_total
                    + chunk
                        .old_lines
                        .len()
                        .saturating_sub(prefix_len + suffix_len),
            )
        })
}

/// Counts logical lines in the rendered file content for add/delete metadata.
fn count_file_lines(contents: &str) -> usize {
    contents.lines().count()
}

/// Renders a compact unified diff for UI metadata and debugging output.
fn render_unified_diff(
    old_path: Option<&str>,
    new_path: Option<&str>,
    old_contents: &str,
    new_contents: &str,
) -> String {
    let old_lines = split_content_lines(old_contents);
    let new_lines = split_content_lines(new_contents);
    let common_prefix = count_common_prefix(&old_lines, &new_lines);
    let common_suffix = count_common_suffix(&old_lines, &new_lines, common_prefix);
    let old_changed_end = old_lines.len().saturating_sub(common_suffix);
    let new_changed_end = new_lines.len().saturating_sub(common_suffix);
    let old_changed = &old_lines[common_prefix..old_changed_end];
    let new_changed = &new_lines[common_prefix..new_changed_end];

    let old_header = old_path
        .map(|path| format!("a/{path}"))
        .unwrap_or_else(|| "/dev/null".to_string());
    let new_header = new_path
        .map(|path| format!("b/{path}"))
        .unwrap_or_else(|| "/dev/null".to_string());
    let mut diff_lines = vec![format!("--- {old_header}"), format!("+++ {new_header}")];

    if old_changed.is_empty() && new_changed.is_empty() {
        return format!("{}\n", diff_lines.join("\n"));
    }

    let old_start = common_prefix + 1;
    let new_start = common_prefix + 1;
    diff_lines.push(format!(
        "@@ -{} +{} @@",
        format_hunk_range(old_start, old_changed.len()),
        format_hunk_range(new_start, new_changed.len())
    ));
    diff_lines.extend(old_changed.iter().map(|line| format!("-{line}")));
    diff_lines.extend(new_changed.iter().map(|line| format!("+{line}")));
    format!("{}\n", diff_lines.join("\n"))
}

/// Counts the common prefix shared by the old and new file lines.
fn count_common_prefix(old_lines: &[String], new_lines: &[String]) -> usize {
    old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(old_line, new_line)| old_line == new_line)
        .count()
}

/// Counts the common suffix after excluding the already-known common prefix.
fn count_common_suffix(old_lines: &[String], new_lines: &[String], prefix_len: usize) -> usize {
    let max_suffix = old_lines
        .len()
        .min(new_lines.len())
        .saturating_sub(prefix_len);
    (0..max_suffix)
        .take_while(|offset| {
            let old_index = old_lines.len() - 1 - offset;
            let new_index = new_lines.len() - 1 - offset;
            old_lines[old_index] == new_lines[new_index]
        })
        .count()
}

/// Formats one unified-diff hunk range.
fn format_hunk_range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

/// Builds the final tool output using the spec-shaped structured payload.
fn render_tool_output(verified_changes: &[VerifiedChange]) -> ToolOutput {
    let title = "Success. Updated the following files:".to_string();
    let mut output_lines = vec![title.clone()];
    output_lines.extend(
        verified_changes
            .iter()
            .map(|change| change.summary_line.clone()),
    );
    let output = output_lines.join("\n");

    ToolOutput {
        text: output.clone(),
        structured: StructuredToolOutput::ApplyPatch(ApplyPatchStructuredOutput {
            title,
            output,
            metadata: ApplyPatchMetadata {
                diff: verified_changes
                    .iter()
                    .map(|change| change.diff.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                files: verified_changes
                    .iter()
                    .map(|change| ApplyPatchFileMetadata {
                        file_path: change.absolute_source_path.to_string_lossy().to_string(),
                        relative_path: change.source_path.clone(),
                        r#type: change.operation_type.to_string(),
                        patch: change.diff.clone(),
                        additions: change.additions,
                        deletions: change.deletions,
                        move_path: change.move_path.clone(),
                    })
                    .collect::<Vec<_>>(),
                diagnostics: std::collections::BTreeMap::new(),
            },
        }),
    }
}

//! Hashline edit tool implementation.

mod anchor;
mod buffer;
mod cleanup;
mod line_ending;
mod operation;
mod outcome;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::{FsBackend, FsReadRequest, FsWriteRequest, LocalFsBackend, Tool};

use self::buffer::EditBuffer;
use self::line_ending::{LineEnding, normalize_to_lf};
pub use self::operation::{
    HashlineEdit, InsertAfterEdit, ReplaceLinesEdit, SetLineEdit,
};
pub use self::outcome::{HashlineApplyResult, HashlineEditError, NoopEdit};

/// Edits files using hash-verified line references.
pub struct HashlineEditFile {
    /// Backend selected when this tool was registered.
    backend: Arc<dyn FsBackend>,
}

impl HashlineEditFile {
    /// Create a new hashline edit-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalFsBackend::new()))
    }

    /// Create a hashline edit-file tool using the provided filesystem backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn FsBackend>) -> Self {
        Self { backend }
    }
}

impl Default for HashlineEditFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for HashlineEditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file using hash-verified LINE:HASH anchors from read_file output"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "edits": {
                    "type": "array",
                    "description": "Hashline edit operations",
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "set_line": {
                                        "type": "object",
                                        "properties": {
                                            "anchor": { "type": "string", "description": "LINE:HASH anchor" },
                                            "new_text": { "type": "string", "description": "Replacement text; empty deletes the line" }
                                        },
                                        "required": ["anchor", "new_text"]
                                    }
                                },
                                "required": ["set_line"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "replace_lines": {
                                        "type": "object",
                                        "properties": {
                                            "start_anchor": { "type": "string", "description": "Start LINE:HASH anchor" },
                                            "end_anchor": { "type": "string", "description": "End LINE:HASH anchor" },
                                            "new_text": { "type": "string", "description": "Replacement text; empty deletes the range" }
                                        },
                                        "required": ["start_anchor", "new_text"]
                                    }
                                },
                                "required": ["replace_lines"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "insert_after": {
                                        "type": "object",
                                        "properties": {
                                            "anchor": { "type": "string", "description": "LINE:HASH anchor" },
                                            "text": { "type": "string", "description": "Text to insert after the anchor" },
                                            "content": { "type": "string", "description": "Legacy alias for text" }
                                        },
                                        "required": ["anchor"],
                                        "anyOf": [
                                            { "required": ["text"] },
                                            { "required": ["content"] }
                                        ]
                                    }
                                },
                                "required": ["insert_after"]
                            }
                        ]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        self.do_edit(arguments, ctx).await
    }
}

impl HashlineEditFile {
    /// Execute a hashline edit and return model-facing text.
    async fn do_edit(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let args: EditArgs = serde_json::from_value(arguments)
            .map_err(|error| format!("invalid arguments: {error}"))?;
        // Hashline edits must see the complete file because anchors can relocate
        // outside the range originally shown to the model.
        let read_response = self
            .backend
            .read_text_file(
                FsReadRequest::builder()
                    .session_id(ctx.session_id.clone())
                    .cwd(ctx.cwd.clone())
                    .path(PathBuf::from(&args.path))
                    .offset(0)
                    .preserve_full(true)
                    .build(),
            )
            .await
            .map_err(|error| error.to_string())?;

        let original = read_response.content;

        // Apply all hashline matching on LF-normalized content so anchors are
        // stable across LF, CRLF, and bare-CR files.
        let line_ending = LineEnding::detect(&original);
        let normalized_original = normalize_to_lf(&original);
        let apply_result = EditBuffer::new(normalized_original.as_ref())
            .apply(&args.edits)
            .map_err(|error| error.to_string())?;

        // Treat exact no-op writes as tool errors to force the model to re-read
        // stale context instead of reporting a successful edit that changed nothing.
        if normalized_original.as_ref() == apply_result.content {
            return Err(NoopEdit::format_batch_error(
                &args.path,
                &apply_result.noop_edits,
            ));
        }

        // Restore the original line-ending style only after edit application so
        // hash comparisons and merge heuristics stay independent of platform newlines.
        let HashlineApplyResult {
            content,
            warnings,
            first_changed_line: _,
            noop_edits: _,
        } = apply_result;
        let final_content = line_ending.restore_owned(content);

        self.backend
            .write_text_file(
                FsWriteRequest::builder()
                    .session_id(ctx.session_id.clone())
                    .cwd(ctx.cwd.clone())
                    .path(PathBuf::from(&args.path))
                    .content(final_content)
                    .build(),
            )
            .await
            .map_err(|error| error.to_string())?;

        let mut model_output = format!("Updated {}", args.path);

        // Warnings are model-facing recovery hints, such as unique relocated anchors.
        if !warnings.is_empty() {
            model_output.push_str("\n\nWarnings:\n");
            model_output.push_str(&warnings.join("\n"));
        }
        Ok(model_output)
    }
}

#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    edits: Vec<HashlineEdit>,
}

#[cfg(test)]
mod tests {
    use super::super::format::compute_line_hash;
    use super::*;
    use crate::{
        FsBackendError, FsReadResponse, FsWriteRequest, FsWriteResponse,
        ToolContext,
    };
    use futures::StreamExt;
    use std::sync::Mutex;

    /// Build a test tool context rooted at `cwd` with defaults.
    fn test_context(cwd: impl Into<std::path::PathBuf>) -> ToolContext {
        ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(cwd.into())
            .agent_path(protocol::AgentPath::root())
            .approval_mode(protocol::ApprovalMode::default())
            .build()
    }

    /// Apply hashline edits to a test-only in-memory buffer.
    fn test_apply(
        content: &str,
        edits: &[HashlineEdit],
    ) -> Result<HashlineApplyResult, HashlineEditError> {
        EditBuffer::new(content).apply(edits)
    }

    /// Build a `LINE:HASH` anchor for a one-indexed line in content.
    fn anchor(content: &str, line: usize) -> String {
        let line_text = content
            .split('\n')
            .nth(line - 1)
            .expect("test line should exist");
        format!("{line}:{}", compute_line_hash(line_text))
    }

    /// Verifies the tool schema matches supported source-compatible edit shapes.
    #[test]
    fn hashline_edit_parameters_match_supported_aliases() {
        let parameters = HashlineEditFile::new().parameters();
        let replace_lines = &parameters["properties"]["edits"]["items"]["oneOf"]
            [1]["properties"]["replace_lines"];
        let insert_after = &parameters["properties"]["edits"]["items"]["oneOf"]
            [2]["properties"]["insert_after"];

        assert_eq!(
            replace_lines["required"],
            serde_json::json!(["start_anchor", "new_text"])
        );
        assert_eq!(
            insert_after["anyOf"],
            serde_json::json!([
                { "required": ["text"] },
                { "required": ["content"] }
            ])
        );
    }

    /// Verifies that set_line replaces, deletes, and expands anchored lines.
    #[test]
    fn apply_set_line_variants() {
        let content = "aaa\nbbb\nccc";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 2),
                    new_text: "BBB\nBBB2".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "aaa\nBBB\nBBB2\nccc");
    }

    /// Verifies that replace_lines replaces and deletes inclusive ranges.
    #[test]
    fn apply_replace_lines_range() {
        let content = "aaa\nbbb\nccc\nddd";
        let result = test_apply(
            content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(content, 2),
                    end_anchor: Some(anchor(content, 3)),
                    new_text: "MIDDLE".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "aaa\nMIDDLE\nddd");
    }

    /// Verifies explicit same-anchor ranges behave as single-line replacements.
    #[test]
    fn replace_lines_with_same_start_and_end_replaces_single_line() {
        let content = "aaa\nbbb\nccc";
        let result = test_apply(
            content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(content, 2),
                    end_anchor: Some(anchor(content, 2)),
                    new_text: "BBB".to_string(),
                },
            }],
        )
        .expect("same-anchor range should apply");

        assert_eq!(result.content, "aaa\nBBB\nccc");
    }

    /// Verifies empty range replacement text deletes the inclusive range.
    #[test]
    fn replace_lines_with_empty_text_deletes_range() {
        let content = "aaa\nbbb\nccc\nddd";
        let result = test_apply(
            content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(content, 2),
                    end_anchor: Some(anchor(content, 3)),
                    new_text: String::new(),
                },
            }],
        )
        .expect("empty replacement should delete the range");

        assert_eq!(result.content, "aaa\nddd");
    }

    /// Verifies that insert_after inserts text after the anchored line.
    #[test]
    fn apply_insert_after() {
        let content = "aaa\nbbb";
        let result = test_apply(
            content,
            &[HashlineEdit::InsertAfter {
                insert_after: InsertAfterEdit {
                    anchor: anchor(content, 2),
                    text: "ccc".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "aaa\nbbb\nccc");
    }

    /// Verifies insert_after can add multiple lines from one operation.
    #[test]
    fn insert_after_with_multiline_text_inserts_multiple_lines() {
        let content = "aaa\nbbb";
        let result = test_apply(
            content,
            &[HashlineEdit::InsertAfter {
                insert_after: InsertAfterEdit {
                    anchor: anchor(content, 1),
                    text: "one\ntwo".to_string(),
                },
            }],
        )
        .expect("multiline insertion should apply");

        assert_eq!(result.content, "aaa\none\ntwo\nbbb");
    }

    /// Verifies insert_after rejects an empty insertion payload.
    #[test]
    fn insert_after_with_empty_text_is_rejected() {
        let content = "aaa\nbbb";
        let error = test_apply(
            content,
            &[HashlineEdit::InsertAfter {
                insert_after: InsertAfterEdit {
                    anchor: anchor(content, 1),
                    text: String::new(),
                },
            }],
        )
        .expect_err("empty insert_after should fail");

        assert_eq!(
            error.to_string(),
            "Insert-after edit requires non-empty dst"
        );
    }

    /// Verifies an empty file is addressable as its first empty line.
    #[test]
    fn set_line_replaces_the_single_empty_line_in_empty_file() {
        let content = "";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 1),
                    new_text: "created".to_string(),
                },
            }],
        )
        .expect("empty file replacement should apply");

        assert_eq!(result.content, "created");
    }

    /// Verifies set_line with empty replacement deletes the anchored line.
    #[test]
    fn set_line_with_empty_text_deletes_the_line() {
        let content = "aaa\nbbb\nccc";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 2),
                    new_text: String::new(),
                },
            }],
        )
        .expect("empty set_line should delete the target line");

        assert_eq!(result.content, "aaa\nccc");
    }

    /// Verifies that stale hashes return actionable mismatch context.
    #[test]
    fn wrong_hash_returns_mismatch_diagnostic() {
        let error = test_apply(
            "aaa\nbbb\nccc",
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: "2:zz".to_string(),
                    new_text: "new".to_string(),
                },
            }],
        )
        .expect_err("stale hash should fail");

        let text = error.to_string();
        assert!(text.contains("changed since last read"));
        assert!(text.contains(">>> 2:"));
    }

    /// Verifies that a unique hash can relocate when the line moved.
    #[test]
    fn unique_hash_relocates_to_current_line() {
        let old = "aaa\nbbb\nccc";
        let current = "aaa\nccc\nbbb";
        let result = test_apply(
            current,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(old, 2),
                    new_text: "BBB".to_string(),
                },
            }],
        )
        .expect("unique hash should relocate");

        assert_eq!(result.content, "aaa\nccc\nBBB");
    }

    /// Verifies duplicate hashes are not relocated because they are ambiguous.
    #[test]
    fn duplicate_hash_does_not_relocate() {
        let error = test_apply(
            "prefix\nline3\nline14",
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: "1:83".to_string(),
                    new_text: "new".to_string(),
                },
            }],
        )
        .expect_err("duplicate hash should be ambiguous");

        assert!(error.to_string().contains("changed since last read"));
    }

    /// Verifies prefix and echoed-context cleanup for model-provided replacement text.
    #[test]
    fn strips_prefixes_and_echoed_context() {
        let content = "aaa\nbbb\nccc\nddd";
        let result = test_apply(
            content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(content, 2),
                    end_anchor: Some(anchor(content, 3)),
                    new_text: "1:aa|aaa\n+BBB\n+CCC\n4:bb|ddd".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "aaa\nBBB\nCCC\nddd");
    }

    /// Verifies boundary echo stripping uses original context across batched edits.
    #[test]
    fn boundary_echo_stripping_uses_original_context_after_lower_edit() {
        let content = "keep\nold_a\nold_b\ntail\nend";
        let result = test_apply(
            content,
            &[
                HashlineEdit::ReplaceLines {
                    replace_lines: ReplaceLinesEdit {
                        start_anchor: anchor(content, 2),
                        end_anchor: Some(anchor(content, 3)),
                        new_text: "new_a\nnew_b\ntail".to_string(),
                    },
                },
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 4),
                        new_text: "TAIL".to_string(),
                    },
                },
            ],
        )
        .expect("batched edits should apply");

        assert_eq!(result.content, "keep\nnew_a\nnew_b\nTAIL\nend");
    }

    /// Verifies blank replacement lines are preserved rather than treated as echo.
    #[test]
    fn preserves_blank_lines_near_blank_boundaries() {
        let content = "aaa\n\nold\n\nzzz";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 3),
                    new_text: "\nnew\n".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "aaa\n\n\nnew\n\n\nzzz");
    }

    /// Verifies duplicate edits are applied once and multi-edits run bottom-up.
    #[test]
    fn deduplicates_and_applies_bottom_up() {
        let content = "aaa\nbbb\nccc\nddd";
        let result = test_apply(
            content,
            &[
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 1),
                        new_text: "AAA".to_string(),
                    },
                },
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 3),
                        new_text: "CCC\nCCC2".to_string(),
                    },
                },
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 3),
                        new_text: "CCC\nCCC2".to_string(),
                    },
                },
            ],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "AAA\nbbb\nCCC\nCCC2\nddd");
    }

    /// Verifies paired replacements restore indentation when the model omits it.
    #[test]
    fn restores_indent_for_paired_replacement() {
        let content = "fn main() {\n  return 1;\n}";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 2),
                    new_text: "return 2;".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "fn main() {\n  return 2;\n}");
    }

    /// Verifies no-op edits are reported to the caller.
    #[test]
    fn reports_noop_edits() {
        let content = "aaa\nbbb\nccc";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 2),
                    new_text: "bbb".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, content);
        assert_eq!(result.noop_edits.len(), 1);
    }

    /// Verifies source-compatible replace_lines can omit end_anchor for one line.
    #[test]
    fn replace_lines_without_end_anchor_behaves_like_set_line() {
        let content = "aaa\nbbb\nccc";
        let edits = serde_json::from_value::<Vec<HashlineEdit>>(serde_json::json!([
            {"replace_lines": {"start_anchor": anchor(content, 2), "new_text": "BBB"}}
        ]))
        .expect("source-compatible edit shape should deserialize");

        let result = test_apply(content, &edits).expect("edit should apply");

        assert_eq!(result.content, "aaa\nBBB\nccc");
    }

    /// Verifies a one-line range expansion emits a broad-edit warning.
    #[test]
    fn replace_lines_massive_expansion_emits_warning() {
        let content = "old";
        let result = test_apply(
            content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(content, 1),
                    end_anchor: Some(anchor(content, 1)),
                    new_text: [
                        "line01", "line02", "line03", "line04", "line05",
                        "line06", "line07", "line08", "line09", "line10",
                    ]
                    .join("\n"),
                },
            }],
        )
        .expect("large expansion should apply");

        assert_eq!(
            result.content,
            "line01\nline02\nline03\nline04\nline05\nline06\nline07\nline08\nline09\nline10"
        );
        assert_eq!(
            result.warnings,
            vec![
                "Edit resulted in a net change of 9 lines across 1 operations - verify no unintended reformatting."
                    .to_string()
            ]
        );
    }

    /// Verifies a large range contraction emits a broad-edit warning.
    #[test]
    fn replace_lines_massive_contraction_emits_warning() {
        let content = [
            "line01", "line02", "line03", "line04", "line05", "line06",
            "line07", "line08", "line09", "line10", "line11",
        ]
        .join("\n");
        let result = test_apply(
            &content,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(&content, 1),
                    end_anchor: Some(anchor(&content, 11)),
                    new_text: "line00".to_string(),
                },
            }],
        )
        .expect("large contraction should apply");

        assert_eq!(result.content, "line00");
        assert_eq!(
            result.warnings,
            vec![
                "Edit resulted in a net change of 10 lines across 1 operations - verify no unintended reformatting."
                    .to_string()
            ]
        );
    }

    /// Verifies insert_after on the same anchor follows the replaced line.
    #[test]
    fn mixed_set_line_and_insert_after_same_anchor_keeps_insert_after_replacement()
     {
        let content = "aaa\nbbb\nccc";
        let result = test_apply(
            content,
            &[
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 2),
                        new_text: "BBB".to_string(),
                    },
                },
                HashlineEdit::InsertAfter {
                    insert_after: InsertAfterEdit {
                        anchor: anchor(content, 2),
                        text: "inserted".to_string(),
                    },
                },
            ],
        )
        .expect("same-anchor mixed edits should apply");

        assert_eq!(result.content, "aaa\nBBB\ninserted\nccc");
    }

    /// Verifies edits work when the file has no trailing newline.
    #[test]
    fn set_line_expands_file_without_trailing_newline() {
        let content = "aaa\nbbb";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 2),
                    new_text: "BBB\nccc".to_string(),
                },
            }],
        )
        .expect("edit without trailing newline should apply");

        assert_eq!(result.content, "aaa\nBBB\nccc");
    }

    /// Verifies source-compatible insert_after accepts the legacy content alias.
    #[test]
    fn insert_after_accepts_content_alias() {
        let content = "aaa\nbbb";
        let edits = serde_json::from_value::<Vec<HashlineEdit>>(serde_json::json!([
            {"insert_after": {"anchor": anchor(content, 1), "content": "inserted"}}
        ]))
        .expect("source-compatible edit shape should deserialize");

        let result = test_apply(content, &edits).expect("edit should apply");

        assert_eq!(result.content, "aaa\ninserted\nbbb");
    }

    /// Verifies out-of-range anchors report the actual requested line and file length.
    #[test]
    fn out_of_range_anchor_reports_line_count() {
        let error = test_apply(
            "aaa\nbbb\nccc",
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: "99:aa".to_string(),
                    new_text: "new".to_string(),
                },
            }],
        )
        .expect_err("out-of-range line should fail");

        assert_eq!(
            error.to_string(),
            "Line 99 does not exist (file has 3 lines)"
        );
    }

    /// Verifies a single-line replacement can merge a continued next line.
    #[test]
    fn single_line_replacement_merges_continued_next_line() {
        let content = "const value = left &&\n  right;\nnext();";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 1),
                    new_text: "const value = left && right;".to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, "const value = left && right;\nnext();");
    }

    /// Verifies continuation merging does not consume a separately edited neighbor.
    #[test]
    fn single_line_merge_skips_explicitly_touched_neighbor() {
        let content = "const value = left &&\n  right;\nnext();";
        let result = test_apply(
            content,
            &[
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 1),
                        new_text: "const value = left && right;".to_string(),
                    },
                },
                HashlineEdit::SetLine {
                    set_line: SetLineEdit {
                        anchor: anchor(content, 2),
                        new_text: "RIGHT;".to_string(),
                    },
                },
            ],
        )
        .expect("edit should apply");

        assert_eq!(
            result.content,
            "const value = left && right;\n  RIGHT;\nnext();"
        );
    }

    /// Verifies a model split that exactly matches the old canonical line is restored.
    #[test]
    fn restores_old_wrapped_line_when_replacement_splits_it() {
        let content = "const message = formatLongIdentifier(value);";
        let result = test_apply(
            content,
            &[HashlineEdit::SetLine {
                set_line: SetLineEdit {
                    anchor: anchor(content, 1),
                    new_text: "const message =\n  formatLongIdentifier(value);"
                        .to_string(),
                },
            }],
        )
        .expect("edit should apply");

        assert_eq!(result.content, content);
        assert_eq!(result.noop_edits.len(), 1);
    }

    /// Verifies relocated range anchors cannot silently change the range size.
    #[test]
    fn range_relocation_that_changes_scope_returns_mismatch() {
        let old = "aaa\nbbb\nccc";
        let current = "aaa\nccc\nbbb";
        let error = test_apply(
            current,
            &[HashlineEdit::ReplaceLines {
                replace_lines: ReplaceLinesEdit {
                    start_anchor: anchor(old, 2),
                    end_anchor: Some(anchor(old, 3)),
                    new_text: "middle".to_string(),
                },
            }],
        )
        .expect_err("scope-changing relocation should fail");

        let text = error.to_string();
        assert!(text.contains("changed since last read"));
        assert!(text.contains("Range anchor relocation"));
        assert!(text.contains("choose start/end anchors"));
    }

    struct MemoryBackend {
        content: Mutex<String>,
        writes: Mutex<Vec<FsWriteRequest>>,
    }

    #[async_trait::async_trait]
    impl FsBackend for MemoryBackend {
        /// Return the in-memory file content for edit tool tests.
        async fn read_text_file(
            &self,
            _request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            Ok(FsReadResponse {
                content: self
                    .content
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            })
        }

        /// Store written content back into memory for edit tool assertions.
        async fn write_text_file(
            &self,
            request: FsWriteRequest,
        ) -> Result<FsWriteResponse, FsBackendError> {
            *self
                .content
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                request.content.clone();
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request);
            Ok(FsWriteResponse {
                bytes_written: self
                    .content
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
                display_path: PathBuf::from("/workspace/file.txt"),
            })
        }
    }

    /// Create a shared in-memory backend for hashline edit tool tests.
    fn memory_backend(content: &str) -> Arc<MemoryBackend> {
        Arc::new(MemoryBackend {
            content: Mutex::new(content.to_string()),
            writes: Mutex::new(Vec::new()),
        })
    }

    /// Verifies the tool reads, applies, writes, and returns model-facing text.
    #[tokio::test]
    async fn hashline_edit_tool_writes_changed_content() {
        let backend = memory_backend("aaa\nbbb\nccc");
        let tool = HashlineEditFile::with_backend(
            Arc::clone(&backend) as Arc<dyn FsBackend>
        );

        let result = tool
            .execute(
                serde_json::json!({
                    "path": "file.txt",
                    "edits": [{"set_line": {"anchor": anchor("aaa\nbbb\nccc", 2), "new_text": "BBB"}}]
                }),
                &test_context("/workspace"),
            )
            .await
            .expect("edit should succeed");

        assert_eq!(result, "Updated file.txt");
        assert_eq!(
            *backend
                .content
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "aaa\nBBB\nccc"
        );
    }

    /// Verifies no-op edits fail before writing to the backend.
    #[tokio::test]
    async fn hashline_edit_tool_rejects_noop_without_writing() {
        let backend = memory_backend("aaa\nbbb\nccc");
        let tool = HashlineEditFile::with_backend(
            Arc::clone(&backend) as Arc<dyn FsBackend>
        );

        let error = tool
            .execute(
                serde_json::json!({
                    "path": "file.txt",
                    "edits": [{"set_line": {"anchor": anchor("aaa\nbbb\nccc", 2), "new_text": "bbb"}}]
                }),
                &test_context("/workspace"),
            )
            .await
            .expect_err("no-op should fail");

        assert!(error.contains("No changes made"));
        assert!(
            backend
                .writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    /// Verifies hashline edit does not provide argument-stream previews.
    #[test]
    fn hashline_edit_tool_has_no_arguments_consumer() {
        let tool = HashlineEditFile::new();

        assert!(tool.arguments_consumer().is_none());
    }

    /// Verifies execute_streaming falls back to model text without file-change events.
    #[tokio::test]
    async fn hashline_edit_tool_streaming_returns_plain_text_only() {
        let backend = memory_backend("aaa\nbbb\nccc");
        let tool = HashlineEditFile::with_backend(
            Arc::clone(&backend) as Arc<dyn FsBackend>
        );

        let mut stream = tool
            .execute_streaming(
                serde_json::json!({
                    "path": "file.txt",
                    "edits": [{"set_line": {"anchor": anchor("aaa\nbbb\nccc", 2), "new_text": "BBB"}}]
                }),
                &test_context("/workspace"),
            )
            .await
            .expect("streaming edit should succeed");

        let item = stream.next().await.expect("final item should exist");
        assert!(stream.next().await.is_none());
        match item {
            protocol::ToolStreamItem::Final { content, is_error } => {
                assert_eq!(content, "Updated file.txt");
                assert!(!is_error);
            }
            other => panic!("unexpected stream item: {other:?}"),
        }
    }

    /// Verifies the tool restores CRLF endings after applying edits on normalized text.
    #[tokio::test]
    async fn hashline_edit_tool_preserves_crlf_line_endings() {
        let original = "one\r\ntwo\r\nthree";
        let backend = memory_backend(original);
        let tool = HashlineEditFile::with_backend(
            Arc::clone(&backend) as Arc<dyn FsBackend>
        );

        let result = tool
            .execute(
                serde_json::json!({
                    "path": "file.txt",
                    "edits": [{"set_line": {"anchor": anchor(original, 2), "new_text": "TWO"}}]
                }),
                &test_context("/workspace"),
            )
            .await
            .expect("edit should succeed");

        assert_eq!(result, "Updated file.txt");
        assert_eq!(
            *backend
                .content
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "one\r\nTWO\r\nthree"
        );
    }
}

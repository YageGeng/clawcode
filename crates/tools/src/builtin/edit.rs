use async_trait::async_trait;
use protocol::{
    ContentBlock, ToolCall, ToolDefinition, ToolResult, ToolResultDetails,
};
use serde::Deserialize;
use similar::{ChangeTag, TextDiff};
use unicode_normalization::UnicodeNormalization as _;

use super::mutation::with_file_mutation;
use super::path::ResolvedPath;
use crate::{AgentTool, ToolError, ToolExecutionContext};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditReplacement {
    old_text: String,
    new_text: String,
}

#[derive(Debug, Deserialize)]
struct EditArguments {
    path: String,
    edits: Vec<EditReplacement>,
}

impl TryFrom<serde_json::Value> for EditArguments {
    type Error = serde_json::Error;

    /// Normalizes pi's stringified and legacy edit arguments before typed decoding.
    fn try_from(mut value: serde_json::Value) -> Result<Self, Self::Error> {
        if let Some(object) = value.as_object_mut() {
            if let Some(edits) = object.get_mut("edits")
                && let Some(serialized) = edits.as_str()
                && let Ok(parsed) = serde_json::from_str(serialized)
            {
                *edits = parsed;
            }
            let legacy_old = object.remove("oldText");
            let legacy_new = object.remove("newText");
            if let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) {
                let edits = object
                    .entry("edits")
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let Some(edits) = edits.as_array_mut() {
                    edits.push(serde_json::json!({
                        "oldText": old_text,
                        "newText": new_text
                    }));
                }
            }
        }
        serde_json::from_value(value)
    }
}

/// One edit matched against a shared immutable base snapshot.
#[derive(Debug, typed_builder::TypedBuilder)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// Byte span occupied by one LF-normalized source line including its newline.
#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// Adjacent source lines rewritten together by one or more fuzzy replacements.
#[derive(Debug)]
struct ReplacementGroup {
    start_line: usize,
    end_line: usize,
    replacement_indices: Vec<usize>,
}

/// Distinct display diff, standard patch, and first navigation line.
#[derive(Debug)]
struct EditDiff {
    display: String,
    patch: String,
    first_changed_line: Option<usize>,
}

impl EditDiff {
    /// Generates pi's line-number display diff and a separate unified patch.
    fn between(path: &str, old_content: &str, new_content: &str) -> Self {
        let diff = TextDiff::from_lines(old_content, new_content);
        let patch = diff
            .unified_diff()
            .context_radius(4)
            .header(path, path)
            .to_string();
        let line_number_width = old_content
            .split('\n')
            .count()
            .max(new_content.split('\n').count())
            .to_string()
            .len();
        let mut display_lines = Vec::new();
        let groups = diff.grouped_ops(4);
        for (group_index, group) in groups.iter().enumerate() {
            if group_index > 0 {
                display_lines
                    .push(format!(" {} ...", " ".repeat(line_number_width)));
            }
            for operation in group {
                for change in diff.iter_changes(operation) {
                    let value = change
                        .value()
                        .strip_suffix('\n')
                        .unwrap_or(change.value());
                    match change.tag() {
                        ChangeTag::Delete => {
                            let line =
                                change.old_index().unwrap_or_default() + 1;
                            display_lines.push(format!(
                                "-{line:>line_number_width$} {value}"
                            ));
                        }
                        ChangeTag::Insert => {
                            let line =
                                change.new_index().unwrap_or_default() + 1;
                            display_lines.push(format!(
                                "+{line:>line_number_width$} {value}"
                            ));
                        }
                        ChangeTag::Equal => {
                            let line =
                                change.old_index().unwrap_or_default() + 1;
                            display_lines.push(format!(
                                " {line:>line_number_width$} {value}"
                            ));
                        }
                    }
                }
            }
        }
        let first_changed_line = diff
            .iter_all_changes()
            .find(|change| change.tag() != ChangeTag::Equal)
            .and_then(|change| change.new_index().or(change.old_index()))
            .map(|line| line + 1);
        Self {
            display: display_lines.join("\n"),
            patch,
            first_changed_line,
        }
    }
}

/// Pi-compatible exact and conservatively fuzzy multi-block file editor.
pub(super) struct EditTool;

#[async_trait]
impl AgentTool for EditTool {
    /// Returns pi's public multi-edit schema without exposing legacy input fields.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".to_string(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)"
                    },
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text for this targeted edit."
                                }
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        }
    }

    /// Applies all replacements atomically from one original snapshot under the file queue.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments =
            EditArguments::try_from(call.arguments).map_err(|error| {
                ToolError::InvalidArguments {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
        if arguments.edits.is_empty() {
            return Err(ToolError::InvalidArguments {
                tool: call.name,
                message: "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            });
        }
        let absolute = ResolvedPath::new(&arguments.path, &context.cwd)?;
        with_file_mutation(absolute.as_path(), || async {
            context.ensure_active()?;
            let bytes =
                tokio::fs::read(absolute.as_path()).await.map_err(|error| {
                    ToolError::Execution {
                        tool: call.name.clone(),
                        message: format!(
                            "Could not edit file: {}. {}.",
                            arguments.path,
                            IoDiagnostic::from(&error)
                        ),
                    }
                })?;
            context.ensure_active()?;
            let raw = String::from_utf8_lossy(&bytes);
            let (bom, content) = raw
                .strip_prefix('\u{feff}')
                .map_or(("", raw.as_ref()), |content| ("\u{feff}", content));
            let line_ending = if content.find("\r\n").is_some_and(|crlf| {
                content.find('\n').is_some_and(|lf| crlf < lf)
            }) {
                "\r\n"
            } else {
                "\n"
            };
            let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
            let (base, edited) =
                apply_edits(&normalized, &arguments.edits, &arguments.path)?;
            context.ensure_active()?;
            let restored = if line_ending == "\r\n" {
                edited.replace('\n', "\r\n")
            } else {
                edited.clone()
            };
            tokio::fs::write(
                absolute.as_path(),
                format!("{bom}{restored}").as_bytes(),
            )
            .await
            .map_err(|error| ToolError::Execution {
                tool: call.name.clone(),
                message: error.to_string(),
            })?;
            context.ensure_active()?;
            let absolute_path =
                absolute.as_path().to_string_lossy().into_owned();
            let diff = EditDiff::between(&absolute_path, &base, &edited);
            Ok(ToolResult::builder()
                .tool_call_id(call.tool_call_id)
                .blocks(vec![ContentBlock::Text {
                    text: format!(
                        "Successfully replaced {} block(s) in {}.",
                        arguments.edits.len(),
                        arguments.path
                    ),
                }])
                .is_error(false)
                .details(ToolResultDetails::Edit {
                    path: absolute_path,
                    diff: diff.display,
                    patch: diff.patch,
                    first_changed_line: diff.first_changed_line,
                })
                .build())
        })
        .await
    }
}

/// Applies validated disjoint replacements from right to left so source offsets stay stable.
fn apply_edits(
    content: &str,
    edits: &[EditReplacement],
    path: &str,
) -> Result<(String, String), ToolError> {
    let normalized_edits = edits
        .iter()
        .map(|edit| EditReplacement {
            old_text: edit.old_text.replace("\r\n", "\n").replace('\r', "\n"),
            new_text: edit.new_text.replace("\r\n", "\n").replace('\r', "\n"),
        })
        .collect::<Vec<_>>();
    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            let message = if normalized_edits.len() == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{index}].oldText must not be empty in {path}.")
            };
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message,
            });
        }
    }
    let used_fuzzy = normalized_edits
        .iter()
        .any(|edit| !content.contains(&edit.old_text));
    let base_for_matching = if used_fuzzy {
        normalize_fuzzy(content)
    } else {
        content.to_string()
    };
    let mut matched = Vec::with_capacity(normalized_edits.len());
    for (index, edit) in normalized_edits.iter().enumerate() {
        let target = if used_fuzzy {
            normalize_fuzzy(&edit.old_text)
        } else {
            edit.old_text.clone()
        };
        let occurrences =
            base_for_matching.match_indices(&target).collect::<Vec<_>>();
        if occurrences.is_empty() {
            let message = if normalized_edits.len() == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            };
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message,
            });
        }
        if occurrences.len() > 1 {
            let message = if normalized_edits.len() == 1 {
                format!(
                    "Found {} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.",
                    occurrences.len()
                )
            } else {
                format!(
                    "Found {} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.",
                    occurrences.len()
                )
            };
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message,
            });
        }
        let match_index = occurrences
            .first()
            .map(|(index, _target)| *index)
            .unwrap_or_default();
        matched.push(
            MatchedEdit::builder()
                .edit_index(index)
                .match_index(match_index)
                .match_length(target.len())
                .new_text(edit.new_text.clone())
                .build(),
        );
    }
    matched.sort_by_key(|edit| edit.match_index);
    for pair in matched.windows(2) {
        let Some(previous) = pair.first() else {
            continue;
        };
        let Some(current) = pair.get(1) else {
            continue;
        };
        if previous.match_index + previous.match_length > current.match_index {
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message: format!(
                    "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                    previous.edit_index, current.edit_index
                ),
            });
        }
    }
    let edited = if used_fuzzy {
        apply_replacements_preserving_unchanged_lines(
            content,
            &base_for_matching,
            &matched,
        )?
    } else {
        apply_replacements(&base_for_matching, &matched, 0)
    };
    if edited == content {
        let message = if normalized_edits.len() == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!(
                "No changes made to {path}. The replacements produced identical content."
            )
        };
        return Err(ToolError::Execution {
            tool: "edit".to_string(),
            message,
        });
    }
    Ok((content.to_string(), edited))
}

/// Applies replacements in reverse source order within an optional base offset.
fn apply_replacements(
    content: &str,
    replacements: &[MatchedEdit],
    offset: usize,
) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let start = replacement.match_index.saturating_sub(offset);
        let end = start.saturating_add(replacement.match_length);
        result.replace_range(start..end, &replacement.new_text);
    }
    result
}

/// Rewrites only fuzzy-matched line groups and copies all untouched original lines verbatim.
fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[MatchedEdit],
) -> Result<String, ToolError> {
    let original_lines =
        original_content.split_inclusive('\n').collect::<Vec<_>>();
    let mut offset = 0_usize;
    let base_lines = base_content
        .split_inclusive('\n')
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect::<Vec<_>>();
    if original_lines.len() != base_lines.len() {
        return Err(ToolError::Execution {
            tool: "edit".to_string(),
            message: "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        });
    }

    let mut groups: Vec<ReplacementGroup> = Vec::new();
    for (replacement_index, replacement) in replacements.iter().enumerate() {
        let replacement_end =
            replacement.match_index + replacement.match_length;
        let Some(start_line) = base_lines.iter().position(|line| {
            replacement.match_index >= line.start
                && replacement.match_index < line.end
        }) else {
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message: "Replacement range is outside the base content."
                    .to_string(),
            });
        };
        let mut end_line = start_line;
        while base_lines
            .get(end_line)
            .is_some_and(|line| line.end < replacement_end)
        {
            end_line += 1;
        }
        if end_line >= base_lines.len() {
            return Err(ToolError::Execution {
                tool: "edit".to_string(),
                message: "Replacement range is outside the base content."
                    .to_string(),
            });
        }
        let end_line = end_line + 1;
        if let Some(group) = groups.last_mut()
            && start_line < group.end_line
        {
            group.end_line = group.end_line.max(end_line);
            group.replacement_indices.push(replacement_index);
        } else {
            groups.push(ReplacementGroup {
                start_line,
                end_line,
                replacement_indices: vec![replacement_index],
            });
        }
    }

    let mut original_line_index = 0_usize;
    let mut result = String::new();
    for group in groups {
        result.push_str(
            &original_lines
                .get(original_line_index..group.start_line)
                .unwrap_or_default()
                .concat(),
        );
        let group_start = base_lines
            .get(group.start_line)
            .map(|line| line.start)
            .unwrap_or_default();
        let group_end = base_lines
            .get(group.end_line.saturating_sub(1))
            .map(|line| line.end)
            .unwrap_or(base_content.len());
        let group_replacements = group
            .replacement_indices
            .iter()
            .filter_map(|index| replacements.get(*index))
            .map(|replacement| {
                MatchedEdit::builder()
                    .edit_index(replacement.edit_index)
                    .match_index(replacement.match_index)
                    .match_length(replacement.match_length)
                    .new_text(replacement.new_text.clone())
                    .build()
            })
            .collect::<Vec<_>>();
        result.push_str(&apply_replacements(
            base_content.get(group_start..group_end).unwrap_or_default(),
            &group_replacements,
            group_start,
        ));
        original_line_index = group.end_line;
    }
    result.push_str(
        &original_lines
            .get(original_line_index..)
            .unwrap_or_default()
            .concat(),
    );
    Ok(result)
}

/// Applies pi's conservative fuzzy normalization for whitespace and punctuation variants.
fn normalize_fuzzy(content: &str) -> String {
    content
        .nfkc()
        .collect::<String>()
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00a0}'
            | '\u{2002}'..='\u{200a}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Stable platform-independent label for edit access failures.
struct IoDiagnostic(String);

impl From<&std::io::Error> for IoDiagnostic {
    /// Converts common filesystem kinds to the Node-style codes used by pi.
    fn from(error: &std::io::Error) -> Self {
        let message = match error.kind() {
            std::io::ErrorKind::NotFound => "Error code: ENOENT".to_string(),
            std::io::ErrorKind::PermissionDenied => {
                "Error code: EACCES".to_string()
            }
            _ => format!("Error: {error}"),
        };
        Self(message)
    }
}

impl std::fmt::Display for IoDiagnostic {
    /// Writes the stable diagnostic selected from the underlying I/O kind.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

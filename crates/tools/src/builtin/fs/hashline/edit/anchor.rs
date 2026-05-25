//! Hashline anchor validation, relocation, and stale-anchor diagnostics.

use std::collections::{HashMap, HashSet};

use super::super::format::{LineRef, compute_line_hash};
use super::operation::{ParsedEdit, ParsedSpec};
use super::outcome::{HashMismatch, HashMismatchReason, HashlineEditError};

/// Validates hashline anchors and relocates stale anchors when relocation is unambiguous.
pub(super) struct AnchorResolver;

impl AnchorResolver {
    /// Validate anchors and relocate only when the expected hash is unique.
    pub(super) fn validate_and_relocate_refs(
        parsed: &mut [ParsedEdit],
        file_lines: &[String],
    ) -> Result<(), HashlineEditError> {
        let mut unique_line_by_hash = None;
        let mut mismatches = Vec::new();
        for parsed_edit in parsed {
            match &mut parsed_edit.spec {
                ParsedSpec::Single { reference } => {
                    Self::validate_or_relocate_ref(
                        reference,
                        file_lines,
                        &mut unique_line_by_hash,
                        &mut mismatches,
                    )?;
                }
                ParsedSpec::InsertAfter { after } => {
                    Self::validate_or_relocate_ref(
                        after,
                        file_lines,
                        &mut unique_line_by_hash,
                        &mut mismatches,
                    )?;
                }
                ParsedSpec::Range { start, end } => {
                    let original_start = start.line;
                    let original_end = end.line;
                    let original_count = original_end - original_start + 1;
                    let start_relocated = Self::validate_or_relocate_ref(
                        start,
                        file_lines,
                        &mut unique_line_by_hash,
                        &mut mismatches,
                    )?;
                    let end_relocated = Self::validate_or_relocate_ref(
                        end,
                        file_lines,
                        &mut unique_line_by_hash,
                        &mut mismatches,
                    )?;
                    let relocated_count = if start.line <= end.line {
                        Some(end.line - start.line + 1)
                    } else {
                        None
                    };
                    if (start_relocated || end_relocated)
                        && (start.line > end.line || relocated_count != Some(original_count))
                    {
                        // Range edits must keep their original span length after relocation.
                        start.line = original_start;
                        end.line = original_end;
                        mismatches.push(
                            Self::build_mismatch(start, file_lines, original_start)
                                .with_range_span_changed(original_count, relocated_count),
                        );
                        mismatches.push(
                            Self::build_mismatch(end, file_lines, original_end)
                                .with_range_span_changed(original_count, relocated_count),
                        );
                    }
                }
            }
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(HashlineEditError::Mismatch(Self::format_mismatch_message(
                &mismatches,
                file_lines,
            )))
        }
    }

    /// Validate one reference and mutate it to a relocated line when safe.
    fn validate_or_relocate_ref(
        reference: &mut LineRef,
        file_lines: &[String],
        unique_line_by_hash: &mut Option<HashMap<String, Option<usize>>>,
        mismatches: &mut Vec<HashMismatch>,
    ) -> Result<bool, HashlineEditError> {
        if reference.line < 1 || reference.line > file_lines.len() {
            return Err(HashlineEditError::Invalid(format!(
                "Line {} does not exist (file has {} lines)",
                reference.line,
                file_lines.len()
            )));
        }
        let Some(line) = file_lines.get(reference.line - 1) else {
            return Err(HashlineEditError::Invalid(format!(
                "Line {} does not exist (file has {} lines)",
                reference.line,
                file_lines.len()
            )));
        };
        let actual = compute_line_hash(line);
        if actual == reference.hash {
            return Ok(false);
        }
        let unique_line_by_hash =
            unique_line_by_hash.get_or_insert_with(|| Self::build_unique_line_by_hash(file_lines));
        if let Some(Some(relocated)) = unique_line_by_hash.get(&reference.hash) {
            reference.line = *relocated;
            return Ok(true);
        }
        mismatches.push(HashMismatch::anchor_changed(
            reference.line,
            reference.hash.clone(),
            actual,
        ));
        Ok(false)
    }

    /// Build a map where hashes with duplicate occurrences are excluded.
    fn build_unique_line_by_hash(file_lines: &[String]) -> HashMap<String, Option<usize>> {
        let mut map = HashMap::new();
        for (index, line) in file_lines.iter().enumerate() {
            let hash = compute_line_hash(line);
            match map.get_mut(&hash) {
                Some(value) => *value = None,
                None => {
                    map.insert(hash, Some(index + 1));
                }
            }
        }
        map
    }

    /// Build a mismatch entry for a specific line number.
    fn build_mismatch(reference: &LineRef, file_lines: &[String], line: usize) -> HashMismatch {
        HashMismatch::anchor_changed(
            line,
            reference.hash.clone(),
            file_lines
                .get(line.saturating_sub(1))
                .map(|line| compute_line_hash(line))
                .unwrap_or_default(),
        )
    }

    /// Format a stale-anchor diagnostic with nearby updated hashline refs.
    fn format_mismatch_message(mismatches: &[HashMismatch], file_lines: &[String]) -> String {
        let mismatch_lines = mismatches
            .iter()
            .map(|mismatch| (mismatch.line, mismatch))
            .collect::<HashMap<_, _>>();
        let mut display_lines = HashSet::new();
        for mismatch in mismatches {
            let low = mismatch.line.saturating_sub(2).max(1);
            let high = (mismatch.line + 2).min(file_lines.len());
            for line in low..=high {
                display_lines.insert(line);
            }
        }
        let mut sorted = display_lines.into_iter().collect::<Vec<_>>();
        sorted.sort_unstable();

        let plural = if mismatches.len() > 1 {
            "s have"
        } else {
            " has"
        };
        let mut lines = vec![
            format!(
                "{} line{} changed since last read. Use the updated LINE:HASH references shown below (>>> marks changed lines).",
                mismatches.len(),
                plural
            ),
            String::new(),
        ];
        if let Some(reason) = Self::range_span_changed_reason(mismatches) {
            lines.push(reason);
            lines.push(String::new());
        }
        let mut previous = None;
        for line_number in sorted {
            if previous.is_some_and(|prev| line_number > prev + 1) {
                lines.push("    ...".to_string());
            }
            previous = Some(line_number);
            if let Some(content) = file_lines.get(line_number - 1) {
                if let Some(mismatch) = mismatch_lines.get(&line_number) {
                    let prefix = format!("{}:{}", line_number, mismatch.actual);
                    lines.push(format!(">>> {prefix}|{content}"));
                } else {
                    let prefix = format!("{}:{}", line_number, compute_line_hash(content));
                    lines.push(format!("    {prefix}|{content}"));
                }
            }
        }
        lines.push(String::new());
        lines.push("Quick fix - replace stale refs:".to_string());
        for mismatch in mismatches {
            if file_lines.get(mismatch.line.saturating_sub(1)).is_some() {
                lines.push(format!(
                    "\t{}:{} -> {}:{}",
                    mismatch.line, mismatch.expected, mismatch.line, mismatch.actual
                ));
            }
        }
        lines.join("\n")
    }

    /// Return a model-facing explanation when range relocation changed the span.
    fn range_span_changed_reason(mismatches: &[HashMismatch]) -> Option<String> {
        mismatches.iter().find_map(|mismatch| match mismatch.reason {
            HashMismatchReason::AnchorChanged => None,
            HashMismatchReason::RangeSpanChanged {
                original_count,
                relocated_count,
            } => Some(match relocated_count {
                Some(relocated_count) => format!(
                    "Range anchor relocation changed the requested span from {original_count} lines to {relocated_count} lines; re-read the file and choose start/end anchors for the intended range."
                ),
                None => format!(
                    "Range anchor relocation inverted the requested {original_count}-line span; re-read the file and choose start/end anchors for the intended range."
                ),
            }),
        })
    }
}

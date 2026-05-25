//! Hashline edit planning and application over LF-normalized line buffers.

use std::collections::HashSet;
use std::convert::TryFrom;

use super::anchor::AnchorResolver;
use super::cleanup::ReplacementNormalizer;
use super::operation::{HashlineEdit, ParsedEdit, ParsedSpec};
use super::outcome::{HashlineApplyResult, HashlineEditError, NoopEdit};

/// Owns mutable lines and borrowed original lines for one edit application.
pub(super) struct EditBuffer<'a> {
    lines: Vec<String>,
    original_lines: Vec<&'a str>,
}

impl<'a> EditBuffer<'a> {
    /// Create from LF-normalized content.
    pub(super) fn new(content: &'a str) -> Self {
        let original_lines = content.split('\n').collect::<Vec<_>>();
        let lines = original_lines
            .iter()
            .map(|line| (*line).to_string())
            .collect();
        Self {
            lines,
            original_lines,
        }
    }

    /// Apply hashline edits to this buffer, consuming it.
    pub(super) fn apply(
        mut self,
        edits: &[HashlineEdit],
    ) -> Result<HashlineApplyResult, HashlineEditError> {
        EditPlanner::new(&mut self.lines, &self.original_lines, edits).apply()
    }
}

/// Coordinates parsing, validation, planning, and application for one edit batch.
struct EditPlanner<'a, 'b> {
    file_lines: &'a mut Vec<String>,
    original_file_lines: &'a [&'b str],
    edits: &'a [HashlineEdit],
}

impl<'a, 'b> EditPlanner<'a, 'b> {
    /// Create a planner over mutable file lines and immutable original context.
    fn new(
        file_lines: &'a mut Vec<String>,
        original_file_lines: &'a [&'b str],
        edits: &'a [HashlineEdit],
    ) -> Self {
        Self {
            file_lines,
            original_file_lines,
            edits,
        }
    }

    /// Apply the edit batch and return the updated file content.
    fn apply(self) -> Result<HashlineApplyResult, HashlineEditError> {
        if self.edits.is_empty() {
            return Err(HashlineEditError::Invalid(
                "edits must contain at least one operation".to_string(),
            ));
        }

        let mut parsed = self
            .edits
            .iter()
            .map(ParsedEdit::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        AnchorResolver::validate_and_relocate_refs(&mut parsed, self.file_lines)?;
        let explicitly_touched_lines = Self::collect_explicitly_touched_lines(&parsed);
        Self::deduplicate_edits(&mut parsed);

        let mut first_changed_line = None;
        let mut noop_edits = Vec::new();
        let mut planned_replacements = Vec::new();
        let mut annotated = parsed
            .into_iter()
            .enumerate()
            .map(|(index, parsed)| {
                let sort = Self::edit_sort_key(&parsed.spec);
                (sort, index, parsed)
            })
            .collect::<Vec<_>>();
        annotated.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));

        for (_, edit_index, parsed) in annotated {
            match parsed.spec {
                ParsedSpec::Single { reference } => {
                    if let Some(plan) = self.plan_single_replacement(
                        NoopTarget::new(edit_index, reference.location()),
                        reference.line,
                        &parsed.dst_lines,
                        &explicitly_touched_lines,
                        &mut noop_edits,
                    )? {
                        Self::track_first_changed(&mut first_changed_line, plan.changed_line());
                        planned_replacements.push(plan);
                    }
                }
                ParsedSpec::Range { start, end } => {
                    if let Some(plan) = self.plan_range_replacement(
                        NoopTarget::new(edit_index, start.location()),
                        start.line,
                        end.line,
                        &parsed.dst_lines,
                        &mut noop_edits,
                    )? {
                        Self::track_first_changed(&mut first_changed_line, plan.changed_line());
                        planned_replacements.push(plan);
                    }
                }
                ParsedSpec::InsertAfter { after } => {
                    if let Some(plan) = self.plan_insert_after(
                        NoopTarget::new(edit_index, after.location()),
                        after.line,
                        &parsed.dst_lines,
                        &mut noop_edits,
                    )? {
                        Self::track_first_changed(&mut first_changed_line, plan.changed_line());
                        planned_replacements.push(plan);
                    }
                }
            }
        }

        ReplacementPlanSet::new(planned_replacements).apply(self.file_lines);

        let warnings = Self::build_warnings(
            self.original_file_lines.len(),
            self.file_lines.len(),
            self.edits.len() - noop_edits.len(),
        );
        let builder = HashlineApplyResult::builder()
            .content(self.file_lines.join("\n"))
            .warnings(warnings)
            .noop_edits(noop_edits);
        Ok(match first_changed_line {
            Some(line) => builder.first_changed_line(line).build(),
            None => builder.build(),
        })
    }

    /// Plan a single-line replacement or adjacent-line merge expansion.
    fn plan_single_replacement(
        &self,
        target: NoopTarget,
        line: usize,
        dst_lines: &[String],
        explicitly_touched_lines: &HashSet<usize>,
        noop_edits: &mut Vec<NoopEdit>,
    ) -> Result<Option<PlannedReplacement>, HashlineEditError> {
        if let Some(merged) =
            self.maybe_expand_single_line_merge(line, dst_lines, explicitly_touched_lines)
        {
            let orig_lines = self
                .original_file_lines
                .get(merged.start_line - 1..merged.start_line - 1 + merged.delete_count)
                .ok_or_else(|| {
                    HashlineEditError::Invalid(format!(
                        "Line range {}-{} does not exist",
                        merged.start_line,
                        merged.start_line + merged.delete_count - 1
                    ))
                })?
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<_>>();
            let new_lines = ReplacementNormalizer::restore_indent_for_paired_replacement(
                &[orig_lines.first().cloned().ok_or_else(|| {
                    HashlineEditError::Invalid("Merged edit had no original lines".to_string())
                })?],
                &merged.new_lines,
            );
            if orig_lines.as_slice() == new_lines.as_ref() {
                noop_edits.push(target.noop(orig_lines.join("\n")));
                return Ok(None);
            }
            return Ok(Some(PlannedReplacement {
                start_index: merged.start_line - 1,
                delete_count: merged.delete_count,
                new_lines: new_lines.into_owned(),
            }));
        }

        self.plan_range_replacement(target, line, line, dst_lines, noop_edits)
    }

    /// Plan a replacement over an inclusive line range.
    fn plan_range_replacement(
        &self,
        target: NoopTarget,
        start_line: usize,
        end_line: usize,
        dst_lines: &[String],
        noop_edits: &mut Vec<NoopEdit>,
    ) -> Result<Option<PlannedReplacement>, HashlineEditError> {
        let count = end_line - start_line + 1;
        let orig_lines = self
            .original_file_lines
            .get(start_line - 1..start_line - 1 + count)
            .ok_or_else(|| {
                HashlineEditError::Invalid(format!(
                    "Line range {start_line}-{end_line} does not exist"
                ))
            })?
            .iter()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>();
        let stripped = ReplacementNormalizer::strip_range_boundary_echo(
            self.original_file_lines,
            start_line,
            end_line,
            dst_lines,
        );
        let restored =
            ReplacementNormalizer::restore_old_wrapped_lines(&orig_lines, stripped.as_ref());
        let new_lines = ReplacementNormalizer::restore_indent_for_paired_replacement(
            &orig_lines,
            restored.as_ref(),
        );
        if orig_lines.as_slice() == new_lines.as_ref() {
            noop_edits.push(target.noop(orig_lines.join("\n")));
            return Ok(None);
        }
        Ok(Some(PlannedReplacement {
            start_index: start_line - 1,
            delete_count: count,
            new_lines: new_lines.into_owned(),
        }))
    }

    /// Plan an insert-after operation, including anchor echo cleanup.
    fn plan_insert_after(
        &self,
        target: NoopTarget,
        after_line: usize,
        dst_lines: &[String],
        noop_edits: &mut Vec<NoopEdit>,
    ) -> Result<Option<PlannedReplacement>, HashlineEditError> {
        let anchor_line = self
            .original_file_lines
            .get(after_line - 1)
            .ok_or_else(|| {
                HashlineEditError::Invalid(format!("Line {after_line} does not exist"))
            })?;
        let inserted =
            ReplacementNormalizer::strip_insert_anchor_echo_after(anchor_line, dst_lines);
        if inserted.is_empty() {
            noop_edits.push(target.noop((*anchor_line).to_string()));
            return Ok(None);
        }
        Ok(Some(PlannedReplacement {
            start_index: after_line,
            delete_count: 0,
            new_lines: inserted.into_owned(),
        }))
    }

    /// Remove exact duplicate edit operations before applying them.
    fn deduplicate_edits(parsed: &mut Vec<ParsedEdit>) {
        let mut seen = HashSet::new();
        parsed.retain(|parsed_edit| seen.insert(Self::edit_dedup_key(parsed_edit)));
    }

    /// Build a stable deduplication key for one parsed edit.
    fn edit_dedup_key(parsed: &ParsedEdit) -> String {
        let line_key = match &parsed.spec {
            ParsedSpec::Single { reference } => format!("s:{}", reference.line),
            ParsedSpec::Range { start, end } => format!("r:{}:{}", start.line, end.line),
            ParsedSpec::InsertAfter { after } => format!("i:{}", after.line),
        };
        format!("{line_key}|{}", parsed.dst_lines.join("\n"))
    }

    /// Return the bottom-up sorting key for an edit.
    fn edit_sort_key(spec: &ParsedSpec) -> (usize, usize) {
        match spec {
            ParsedSpec::Single { reference } => (reference.line, 0),
            ParsedSpec::Range { end, .. } => (end.line, 0),
            ParsedSpec::InsertAfter { after } => (after.line, 1),
        }
    }

    /// Collect lines explicitly targeted after relocation.
    fn collect_explicitly_touched_lines(parsed: &[ParsedEdit]) -> HashSet<usize> {
        let mut touched = HashSet::new();
        for parsed_edit in parsed {
            match &parsed_edit.spec {
                ParsedSpec::Single { reference } => {
                    touched.insert(reference.line);
                }
                ParsedSpec::Range { start, end } => {
                    for line in start.line..=end.line {
                        touched.insert(line);
                    }
                }
                ParsedSpec::InsertAfter { after } => {
                    touched.insert(after.line);
                }
            }
        }
        touched
    }

    /// Merge a single-line replacement with an adjacent continued line when it clearly includes both.
    fn maybe_expand_single_line_merge(
        &self,
        line: usize,
        dst_lines: &[String],
        explicitly_touched_lines: &HashSet<usize>,
    ) -> Option<ExpandedMerge> {
        // This heuristic handles one-line model replacements that actually fold
        // two source lines into one. It first rejects non-single-line replacements,
        // then tries a forward merge when the target line ends with a continuation
        // token and the next line is untouched, and finally tries a backward merge
        // when the previous line is the continuation and the target line appears
        // after it in the canonical replacement text. If neither direction has
        // ordered canonical matches with a small size delta, the edit stays single-line.
        if dst_lines.len() != 1 || line < 1 || line > self.original_file_lines.len() {
            return None;
        }
        let new_line = dst_lines.first()?;
        let new_canon = ReplacementNormalizer::strip_all_whitespace(new_line);
        let new_canon_for_merge_ops = ReplacementNormalizer::strip_merge_operator_chars(&new_canon);
        if new_canon.is_empty() {
            return None;
        }
        let orig = self.original_file_lines.get(line - 1)?;
        let orig_canon = ReplacementNormalizer::strip_all_whitespace(orig);
        let orig_canon_for_match =
            ReplacementNormalizer::strip_trailing_continuation_tokens(&orig_canon);
        let orig_canon_for_merge_ops =
            ReplacementNormalizer::strip_merge_operator_chars(&orig_canon);
        let orig_looks_like_continuation = orig_canon_for_match.len() < orig_canon.len();
        if orig_canon.is_empty() {
            return None;
        }

        let next_line = line + 1;
        if orig_looks_like_continuation
            && next_line <= self.original_file_lines.len()
            && !explicitly_touched_lines.contains(&next_line)
        {
            let next = self.original_file_lines.get(next_line - 1)?;
            let next_canon = ReplacementNormalizer::strip_all_whitespace(next);
            let orig_pos = new_canon.find(&orig_canon_for_match);
            let next_pos = new_canon.find(&next_canon);
            if orig_pos.zip(next_pos).is_some_and(|(orig_pos, next_pos)| {
                orig_pos < next_pos && new_canon.len() <= orig_canon.len() + next_canon.len() + 32
            }) {
                return Some(ExpandedMerge {
                    start_line: line,
                    delete_count: 2,
                    new_lines: vec![new_line.clone()],
                });
            }
        }

        let previous_line = line.checked_sub(1)?;
        if previous_line >= 1 && !explicitly_touched_lines.contains(&previous_line) {
            let previous = self.original_file_lines.get(previous_line - 1)?;
            let previous_canon = ReplacementNormalizer::strip_all_whitespace(previous);
            let previous_canon_for_match =
                ReplacementNormalizer::strip_trailing_continuation_tokens(&previous_canon);
            let previous_looks_like_continuation =
                previous_canon_for_match.len() < previous_canon.len();
            if !previous_looks_like_continuation {
                return None;
            }
            let previous_pos = new_canon_for_merge_ops.find(
                &ReplacementNormalizer::strip_merge_operator_chars(&previous_canon_for_match),
            );
            let orig_pos = new_canon_for_merge_ops.find(&orig_canon_for_merge_ops);
            if previous_pos
                .zip(orig_pos)
                .is_some_and(|(previous_pos, orig_pos)| {
                    previous_pos < orig_pos
                        && new_canon.len() <= previous_canon.len() + orig_canon.len() + 32
                })
            {
                return Some(ExpandedMerge {
                    start_line: previous_line,
                    delete_count: 2,
                    new_lines: vec![new_line.clone()],
                });
            }
        }
        None
    }

    /// Track the earliest changed line across bottom-up edits.
    fn track_first_changed(first_changed_line: &mut Option<usize>, line: usize) {
        if first_changed_line.is_none_or(|current| line < current) {
            *first_changed_line = Some(line);
        }
    }

    /// Build warnings for suspiciously broad edits.
    fn build_warnings(
        original_line_count: usize,
        current_line_count: usize,
        applied_edits: usize,
    ) -> Vec<String> {
        let size_diff = current_line_count.abs_diff(original_line_count);
        if size_diff > applied_edits * 4 {
            vec![format!(
                "Edit resulted in a net change of {size_diff} lines across {applied_edits} operations - verify no unintended reformatting."
            )]
        } else {
            Vec::new()
        }
    }
}

/// Carries request identity for later no-op diagnostics.
struct NoopTarget {
    edit_index: usize,
    location: String,
}

impl NoopTarget {
    /// Create a no-op diagnostic target for one request edit.
    fn new(edit_index: usize, location: String) -> Self {
        Self {
            edit_index,
            location,
        }
    }

    /// Convert current content into the public no-op record shape.
    fn noop(self, current_content: String) -> NoopEdit {
        NoopEdit {
            edit_index: self.edit_index,
            loc: self.location,
            current_content,
        }
    }
}

/// A concrete splice operation planned against the original file.
struct PlannedReplacement {
    /// Zero-based index where replacement starts.
    start_index: usize,
    /// Number of existing lines removed at the start index.
    delete_count: usize,
    /// Lines inserted at the start index.
    new_lines: Vec<String>,
}

impl PlannedReplacement {
    /// Return the one-indexed line changed by this plan.
    fn changed_line(&self) -> usize {
        self.start_index + 1
    }
}

/// Applies a batch of planned replacements with an efficient strategy when possible.
struct ReplacementPlanSet {
    plans: Vec<PlannedReplacement>,
}

impl ReplacementPlanSet {
    /// Create a plan set from already bottom-up sorted replacements.
    fn new(plans: Vec<PlannedReplacement>) -> Self {
        Self { plans }
    }

    /// Apply replacement plans, rebuilding once for non-overlapping edit batches.
    fn apply(self, file_lines: &mut Vec<String>) {
        if self.plans.is_empty() {
            return;
        }
        if self.can_rebuild() {
            self.rebuild(file_lines);
        } else {
            self.apply_with_splice(file_lines);
        }
    }

    /// Return whether plans can be safely applied by one linear rebuild.
    fn can_rebuild(&self) -> bool {
        let mut sorted = self.plans.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|plan| plan.start_index);
        let mut cursor = 0;
        let mut previous_start = None;
        for plan in sorted {
            if previous_start == Some(plan.start_index) || plan.start_index < cursor {
                return false;
            }
            previous_start = Some(plan.start_index);
            cursor = plan.start_index.saturating_add(plan.delete_count);
        }
        true
    }

    /// Rebuild file lines in one pass while moving unchanged old lines.
    fn rebuild(mut self, file_lines: &mut Vec<String>) {
        self.plans.sort_by_key(|plan| plan.start_index);
        let inserted = self
            .plans
            .iter()
            .map(|plan| plan.new_lines.len())
            .sum::<usize>();
        let deleted = self
            .plans
            .iter()
            .map(|plan| plan.delete_count)
            .sum::<usize>();
        let mut out = Vec::with_capacity(file_lines.len().saturating_sub(deleted) + inserted);
        let old_lines = std::mem::take(file_lines);
        let mut old_iter = old_lines.into_iter().enumerate().peekable();

        for plan in self.plans {
            while old_iter
                .peek()
                .is_some_and(|(index, _)| *index < plan.start_index)
            {
                if let Some((_, line)) = old_iter.next() {
                    out.push(line);
                }
            }
            for _ in 0..plan.delete_count {
                let _ = old_iter.next();
            }
            out.extend(plan.new_lines);
        }
        out.extend(old_iter.map(|(_, line)| line));
        *file_lines = out;
    }

    /// Apply complex or overlapping plans with the original bottom-up splice semantics.
    fn apply_with_splice(self, file_lines: &mut Vec<String>) {
        for plan in self.plans {
            file_lines.splice(
                plan.start_index..plan.start_index + plan.delete_count,
                plan.new_lines,
            );
        }
    }
}

/// A single-line replacement that expands to consume an adjacent continued line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpandedMerge {
    start_line: usize,
    delete_count: usize,
    new_lines: Vec<String>,
}

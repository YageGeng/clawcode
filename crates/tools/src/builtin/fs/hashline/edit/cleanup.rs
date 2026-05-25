//! Replacement-text cleanup heuristics used by hashline edits.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;

/// Normalizes model-provided replacement text before it reaches the edit planner.
pub(super) struct ReplacementNormalizer;

impl ReplacementNormalizer {
    /// Remove hashline or diff prefixes accidentally copied into replacement text.
    pub(super) fn strip_new_line_prefixes(lines: Vec<String>) -> Vec<String> {
        let non_empty = lines.iter().filter(|line| !line.is_empty()).count();
        if non_empty == 0 {
            return lines;
        }
        let hash_prefixes = lines
            .iter()
            .filter(|line| !line.is_empty() && Self::hashline_prefix_len(line).is_some())
            .count();
        let diff_prefixes = lines
            .iter()
            .filter(|line| line.starts_with('+') && !line.starts_with("++"))
            .count();
        let strip_hash = hash_prefixes > 0 && hash_prefixes * 2 >= non_empty;
        let strip_plus = !strip_hash && diff_prefixes > 0 && diff_prefixes * 2 >= non_empty;
        lines
            .into_iter()
            .map(|line| {
                if strip_hash {
                    let without_hash = Self::hashline_prefix_len(&line)
                        .and_then(|len| line.get(len..).map(str::to_string))
                        .unwrap_or(line);
                    Self::strip_diff_plus_prefix(&without_hash)
                } else if strip_plus {
                    Self::strip_diff_plus_prefix(&line)
                } else {
                    line
                }
            })
            .collect()
    }

    /// Remove an echoed anchor line from insert-after content.
    pub(super) fn strip_insert_anchor_echo_after<'a>(
        anchor_line: &str,
        dst_lines: &'a [String],
    ) -> Cow<'a, [String]> {
        let Some(first_line) = dst_lines.first() else {
            return Cow::Borrowed(dst_lines);
        };
        if dst_lines.len() <= 1
            || !Self::is_substantive(anchor_line)
            || !Self::is_substantive(first_line)
        {
            return Cow::Borrowed(dst_lines);
        }
        if Self::equal_ignoring_whitespace(first_line, anchor_line) {
            Cow::Owned(dst_lines.get(1..).unwrap_or_default().to_vec())
        } else {
            Cow::Borrowed(dst_lines)
        }
    }

    /// Remove echoed context immediately outside a replacement range.
    pub(super) fn strip_range_boundary_echo<'a>(
        file_lines: &[&str],
        start_line: usize,
        end_line: usize,
        dst_lines: &'a [String],
    ) -> Cow<'a, [String]> {
        let replaced_count = end_line - start_line + 1;
        if dst_lines.len() <= 1 || dst_lines.len() <= replaced_count {
            return Cow::Borrowed(dst_lines);
        }
        let mut out = None::<Vec<String>>;
        let before_index = start_line.saturating_sub(2);
        if start_line > 1 {
            let should_strip_before = dst_lines
                .first()
                .zip(file_lines.get(before_index))
                .is_some_and(|(first, before)| {
                    Self::is_substantive(first)
                        && Self::is_substantive(before)
                        && Self::equal_ignoring_whitespace(first, before)
                });
            if should_strip_before {
                out = Some(dst_lines.get(1..).unwrap_or_default().to_vec());
            }
        }
        let after_index = end_line;
        let should_strip_after = out
            .as_deref()
            .unwrap_or(dst_lines)
            .last()
            .zip(file_lines.get(after_index))
            .is_some_and(|(last, after)| {
                Self::is_substantive(last)
                    && Self::is_substantive(after)
                    && Self::equal_ignoring_whitespace(last, after)
            });
        if should_strip_after {
            let out = out.get_or_insert_with(|| dst_lines.to_vec());
            out.pop();
        }
        out.map(Cow::Owned).unwrap_or(Cow::Borrowed(dst_lines))
    }

    /// Restore old leading indentation for paired line replacements.
    pub(super) fn restore_indent_for_paired_replacement<'a>(
        old_lines: &[String],
        new_lines: &'a [String],
    ) -> Cow<'a, [String]> {
        if old_lines.len() != new_lines.len() {
            return Cow::Borrowed(new_lines);
        }
        if !old_lines
            .iter()
            .zip(new_lines)
            .any(|(old, new)| Self::should_restore_leading_indent(old, new))
        {
            return Cow::Borrowed(new_lines);
        }
        Cow::Owned(
            old_lines
                .iter()
                .zip(new_lines)
                .map(|(old, new)| Self::restore_leading_indent(old, new))
                .collect(),
        )
    }

    /// Restore old single-line content when replacement text only split it across lines.
    pub(super) fn restore_old_wrapped_lines<'a>(
        old_lines: &[String],
        new_lines: &'a [String],
    ) -> Cow<'a, [String]> {
        // Skip for large replacements where the quadratic scan is not worth it.
        if old_lines.is_empty() || new_lines.len() < 2 || new_lines.len() > 200 {
            return Cow::Borrowed(new_lines);
        }
        let mut canonical_old = HashMap::<String, (String, usize)>::new();
        for line in old_lines {
            let canon = Self::strip_all_whitespace(line);
            canonical_old
                .entry(canon)
                .and_modify(|(_, count)| *count += 1)
                .or_insert_with(|| (line.clone(), 1));
        }

        let mut candidates = Vec::new();
        for start in 0..new_lines.len() {
            for len in 2..=6 {
                let Some(span) = new_lines.get(start..start + len) else {
                    break;
                };
                let canon = Self::strip_all_whitespace(&span.join(""));
                if canon.len() >= 6
                    && let Some((replacement, count)) = canonical_old.get(&canon)
                    && *count == 1
                {
                    candidates.push((start, len, replacement.clone(), canon));
                }
            }
        }
        if candidates.is_empty() {
            return Cow::Borrowed(new_lines);
        }
        let mut canon_counts = HashMap::<String, usize>::new();
        for (_, _, _, canon) in &candidates {
            *canon_counts.entry(canon.clone()).or_default() += 1;
        }
        let mut unique = candidates
            .into_iter()
            .filter(|(_, _, _, canon)| canon_counts.get(canon) == Some(&1))
            .collect::<Vec<_>>();
        unique.sort_by_key(|candidate| Reverse(candidate.0));
        let mut out = new_lines.to_vec();
        for (start, len, replacement, _) in unique {
            out.splice(start..start + len, [replacement]);
        }
        Cow::Owned(out)
    }

    /// Remove all whitespace characters from a string.
    pub(super) fn strip_all_whitespace(value: &str) -> String {
        value.chars().filter(|ch| !ch.is_whitespace()).collect()
    }

    /// Remove continuation operators from the end of a whitespace-stripped line.
    pub(super) fn strip_trailing_continuation_tokens(value: &str) -> String {
        let tokens = [
            "&&", "||", "??", "?", ":", "=", ",", "+", "-", "*", "/", ".", "(",
        ];
        let mut current = value.to_string();
        loop {
            let trimmed = current.trim_end();
            let Some(token) = tokens.iter().find(|token| trimmed.ends_with(**token)) else {
                return current;
            };
            current.truncate(trimmed.len() - token.len());
        }
    }

    /// Remove merge-operator characters used only to compare merged expressions.
    pub(super) fn strip_merge_operator_chars(value: &str) -> String {
        value
            .chars()
            .filter(|ch| !matches!(ch, '|' | '&' | '?'))
            .collect()
    }

    /// Strip a single diff plus prefix unless it is a `++` file header.
    fn strip_diff_plus_prefix(line: &str) -> String {
        if !line.starts_with("++") {
            line.strip_prefix('+').unwrap_or(line).to_string()
        } else {
            line.to_string()
        }
    }

    /// Return the byte length of a `LINE:HASH|` prefix when present.
    fn hashline_prefix_len(line: &str) -> Option<usize> {
        let (line_number, rest) = line.split_once(':')?;
        if line_number.is_empty() || !line_number.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let (hash, _) = rest.split_once('|')?;
        if hash.is_empty() || hash.len() > 16 || !hash.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            return None;
        }
        Some(line_number.len() + 1 + hash.len() + 1)
    }

    /// Return whether a new line should inherit indentation from an old line.
    fn should_restore_leading_indent(template_line: &str, line: &str) -> bool {
        !line.is_empty()
            && Self::leading_whitespace_len(line) == 0
            && Self::leading_whitespace_len(template_line) > 0
    }

    /// Copy leading indentation from the old line when the new line has none.
    fn restore_leading_indent(template_line: &str, line: &str) -> String {
        if line.is_empty() || !Self::leading_whitespace(line).is_empty() {
            return line.to_string();
        }
        let template_indent = Self::leading_whitespace(template_line);
        if template_indent.is_empty() {
            line.to_string()
        } else {
            format!("{template_indent}{line}")
        }
    }

    /// Return the byte length of the leading whitespace prefix of a line.
    fn leading_whitespace_len(line: &str) -> usize {
        line.char_indices()
            .find(|(_, ch)| !ch.is_whitespace())
            .map(|(index, _)| index)
            .unwrap_or(line.len())
    }

    /// Return the leading whitespace prefix of a line.
    fn leading_whitespace(line: &str) -> String {
        line.chars()
            .take_while(|ch| ch.is_whitespace())
            .collect::<String>()
    }

    /// Compare lines after removing all whitespace.
    fn equal_ignoring_whitespace(left: &str, right: &str) -> bool {
        left == right || Self::strip_all_whitespace(left) == Self::strip_all_whitespace(right)
    }

    /// Return whether a line has non-whitespace content.
    fn is_substantive(line: &str) -> bool {
        !line.trim().is_empty()
    }
}

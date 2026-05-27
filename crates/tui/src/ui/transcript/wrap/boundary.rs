//! Wrap boundary helpers for styled characters.

use super::chars::StyledChar;

/// Returns the visible range and next start offset for one wrapped row.
pub(super) fn next_wrap_range(
    chars: &[StyledChar],
    start: usize,
    width: usize,
) -> (usize, usize) {
    let mut used_width = 0usize;
    let mut end = start;
    let mut last_space = None;
    while let Some(styled) = chars.get(end) {
        let next_width = used_width + styled.width;
        if used_width > 0 && next_width > width {
            break;
        }
        used_width = next_width;
        end += 1;
        if styled.ch.is_whitespace() && end > start + 1 {
            last_space = Some(end);
        }
    }

    if end == chars.len() {
        return (end, end);
    }

    if let Some(space_end) = last_space {
        let visible_end = trim_trailing_whitespace(chars, start, space_end);
        return (
            visible_end.max(start + 1),
            skip_leading_whitespace(chars, space_end),
        );
    }

    (end.max(start + 1), end.max(start + 1))
}

/// Removes whitespace at the end of a soft-wrapped row.
fn trim_trailing_whitespace(
    chars: &[StyledChar],
    start: usize,
    mut end: usize,
) -> usize {
    while end > start
        && chars
            .get(end - 1)
            .map(|styled| styled.ch.is_whitespace())
            .unwrap_or(false)
    {
        end -= 1;
    }
    end
}

/// Skips whitespace consumed as the soft-wrap boundary.
fn skip_leading_whitespace(chars: &[StyledChar], mut start: usize) -> usize {
    while chars
        .get(start)
        .map(|styled| styled.ch.is_whitespace())
        .unwrap_or(false)
    {
        start += 1;
    }
    start
}

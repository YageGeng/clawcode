use protocol::{TruncationDetails, TruncationLimit};

/// Pi's default complete-line limit for tool output.
pub const DEFAULT_MAX_LINES: usize = 2_000;

/// Pi's default UTF-8 byte limit for tool output.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1_024;

/// Formats bytes with pi's binary units and one decimal place above 1KB.
#[must_use]
pub fn format_size(bytes: usize) -> String {
    if bytes < 1_024 {
        format!("{bytes}B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1}KB", bytes as f64 / 1_024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1_024.0 * 1_024.0))
    }
}

/// Truncates from the head while retaining only complete source lines.
#[must_use]
pub fn truncate_head(
    content: &str,
    max_lines: usize,
    max_bytes: usize,
) -> TruncationDetails {
    let lines = counted_lines(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationDetails::builder()
            .content(content.to_string())
            .truncated(false)
            .total_lines(total_lines)
            .total_bytes(total_bytes)
            .output_lines(total_lines)
            .output_bytes(total_bytes)
            .last_line_partial(false)
            .first_line_exceeds_limit(false)
            .max_lines(max_lines)
            .max_bytes(max_bytes)
            .build();
    }

    if lines.first().is_some_and(|line| line.len() > max_bytes) {
        return TruncationDetails::builder()
            .content(String::new())
            .truncated(true)
            .truncated_by(TruncationLimit::Bytes)
            .total_lines(total_lines)
            .total_bytes(total_bytes)
            .output_lines(0)
            .output_bytes(0)
            .last_line_partial(false)
            .first_line_exceeds_limit(true)
            .max_lines(max_lines)
            .max_bytes(max_bytes)
            .build();
    }

    let mut selected = Vec::new();
    let mut selected_bytes = 0_usize;
    let mut truncated_by = TruncationLimit::Lines;
    for line in lines.iter().take(max_lines) {
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if selected_bytes.saturating_add(line_bytes) > max_bytes {
            truncated_by = TruncationLimit::Bytes;
            break;
        }
        selected.push(*line);
        selected_bytes += line_bytes;
    }
    if selected.len() >= max_lines && selected_bytes <= max_bytes {
        truncated_by = TruncationLimit::Lines;
    }
    let output = selected.join("\n");
    TruncationDetails::builder()
        .content(output.clone())
        .truncated(true)
        .truncated_by(truncated_by)
        .total_lines(total_lines)
        .total_bytes(total_bytes)
        .output_lines(selected.len())
        .output_bytes(output.len())
        .last_line_partial(false)
        .first_line_exceeds_limit(false)
        .max_lines(max_lines)
        .max_bytes(max_bytes)
        .build()
}

/// Truncates from the tail and may retain a UTF-8-safe suffix of one oversized line.
#[must_use]
pub fn truncate_tail(
    content: &str,
    max_lines: usize,
    max_bytes: usize,
) -> TruncationDetails {
    let lines = counted_lines(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationDetails::builder()
            .content(content.to_string())
            .truncated(false)
            .total_lines(total_lines)
            .total_bytes(total_bytes)
            .output_lines(total_lines)
            .output_bytes(total_bytes)
            .last_line_partial(false)
            .first_line_exceeds_limit(false)
            .max_lines(max_lines)
            .max_bytes(max_bytes)
            .build();
    }

    let mut selected = Vec::new();
    let mut selected_bytes = 0_usize;
    let mut truncated_by = TruncationLimit::Lines;
    let mut last_line_partial = false;
    for line in lines.iter().rev().take(max_lines) {
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if selected_bytes.saturating_add(line_bytes) > max_bytes {
            truncated_by = TruncationLimit::Bytes;
            if selected.is_empty() {
                selected.push(utf8_suffix(line, max_bytes));
                selected_bytes = selected.first().map_or(0, String::len);
                last_line_partial = true;
            }
            break;
        }
        selected.push((*line).to_string());
        selected_bytes += line_bytes;
    }
    if selected.len() >= max_lines && selected_bytes <= max_bytes {
        truncated_by = TruncationLimit::Lines;
    }
    selected.reverse();
    let output = selected.join("\n");
    TruncationDetails::builder()
        .content(output.clone())
        .truncated(true)
        .truncated_by(truncated_by)
        .total_lines(total_lines)
        .total_bytes(total_bytes)
        .output_lines(selected.len())
        .output_bytes(output.len())
        .last_line_partial(last_line_partial)
        .first_line_exceeds_limit(false)
        .max_lines(max_lines)
        .max_bytes(max_bytes)
        .build()
}

/// Splits content using pi's line-count rule where a trailing newline adds no empty line.
fn counted_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Returns the largest UTF-8-safe suffix whose encoded length fits the byte limit.
fn utf8_suffix(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut start = content.len().saturating_sub(max_bytes);
    while start < content.len() && !content.is_char_boundary(start) {
        start += 1;
    }
    content.get(start..).unwrap_or_default().to_string()
}

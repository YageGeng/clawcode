use protocol::TruncationLimit;
use tools::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head,
    truncate_tail,
};

/// Head truncation keeps complete leading lines and reports the line boundary.
#[test]
fn head_truncation_matches_pi_line_limit() {
    let content = (1..=2_500)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    let result = truncate_head(&content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);

    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncationLimit::Lines));
    assert_eq!(result.total_lines, 2_500);
    assert_eq!(result.output_lines, 2_000);
    assert!(result.content.starts_with("Line 1\n"));
    assert!(result.content.ends_with("Line 2000"));
    assert!(!result.content.contains("Line 2001"));
}

/// Head truncation never returns a partial first line when that line exceeds 50KB.
#[test]
fn head_truncation_rejects_an_oversized_first_line() {
    let content = format!("{}\nsmall", "x".repeat(DEFAULT_MAX_BYTES + 1));

    let result = truncate_head(&content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);

    assert_eq!(result.content, "");
    assert!(result.first_line_exceeds_limit);
    assert_eq!(result.truncated_by, Some(TruncationLimit::Bytes));
}

/// Tail truncation keeps the end of an oversized final line on a UTF-8 boundary.
#[test]
fn tail_truncation_keeps_utf8_suffix_of_oversized_last_line() {
    let content = "界".repeat(20);

    let result = truncate_tail(&content, 2_000, 10);

    assert!(result.truncated);
    assert!(result.last_line_partial);
    assert_eq!(result.content, "界界界");
    assert_eq!(result.output_bytes, 9);
}

/// A trailing newline does not invent an additional complete source line.
#[test]
fn untruncated_content_preserves_trailing_newline_and_counts_lines() {
    let result = truncate_head("one\ntwo\n", 10, 100);

    assert_eq!(result.content, "one\ntwo\n");
    assert_eq!(result.total_lines, 2);
    assert_eq!(result.output_lines, 2);
    assert!(!result.truncated);
    assert_eq!(result.truncated_by, None);
}

/// Human-readable sizes use the same binary units and precision as pi.
#[test]
fn byte_sizes_match_pi_labels() {
    assert_eq!(format_size(500), "500B");
    assert_eq!(format_size(1_536), "1.5KB");
    assert_eq!(format_size(1_572_864), "1.5MB");
}

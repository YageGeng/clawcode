//! Hashline line-reference formatting primitives.

use std::convert::TryFrom;

use thiserror::Error;

const HASH_MODULO: u32 = 256;

/// Error returned when parsing a model-provided line reference fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LineRefParseError {
    /// The reference did not match the LINE:HASH shape.
    #[error("Invalid line reference \"{0}\". Expected format \"LINE:HASH\" (e.g. \"5:aa\").")]
    InvalidFormat(String),
    /// The parsed line number was zero.
    #[error("Line number must be >= 1, got {line} in \"{input}\".")]
    InvalidLine { line: usize, input: String },
}

/// A parsed `LINE:HASH` reference copied from hashline output.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineRef {
    /// One-indexed line number.
    pub line: usize,
    /// Lowercase hexadecimal hash text.
    pub hash: String,
}

impl LineRef {
    /// Create a line reference from a one-indexed line number and hash text.
    #[must_use]
    pub fn new(line: usize, hash: impl Into<String>) -> Self {
        Self {
            line,
            hash: hash.into().to_ascii_lowercase(),
        }
    }

    /// Return the model-facing `LINE:HASH` form.
    #[must_use]
    pub fn location(&self) -> String {
        format!("{}:{}", self.line, self.hash)
    }
}

impl TryFrom<&str> for LineRef {
    type Error = LineRefParseError;

    /// Parse a copied line reference, tolerating trailing hashline content.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let source = value.to_string();
        let without_content = value.split('|').next().unwrap_or(value);
        let without_comment = without_content
            .split("  ")
            .next()
            .unwrap_or(without_content);
        let compact = without_comment
            .split(':')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(":");
        let Some((line, hash)) = compact.split_once(':') else {
            return Err(LineRefParseError::InvalidFormat(source));
        };
        if hash.is_empty() || hash.len() > 16 || !hash.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            return Err(LineRefParseError::InvalidFormat(source));
        }
        let line = line
            .parse::<usize>()
            .map_err(|_error| LineRefParseError::InvalidFormat(source.clone()))?;
        if line == 0 {
            return Err(LineRefParseError::InvalidLine {
                line,
                input: source,
            });
        }
        Ok(Self::new(line, hash))
    }
}

/// Compute the two-character hashline hash for one source line.
#[must_use]
pub(super) fn compute_line_hash(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let normalized = line
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let hash = xxhash_rust::xxh32::xxh32(normalized.as_bytes(), 0) % HASH_MODULO;
    format!("{hash:02x}")
}

/// Format file content as `LINE:HASH|content` lines.
#[must_use]
pub fn format_hash_lines(content: &str, start_line: usize) -> String {
    content
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            let line_number = start_line + index;
            format!("{}:{}|{}", line_number, compute_line_hash(line), line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that Rust hash output matches the Bun source implementation.
    #[test]
    fn compute_line_hash_matches_bun_vectors() {
        let cases = [
            ("", "05"),
            ("hello", "f9"),
            ("hello world", "02"),
            ("hello   world", "02"),
            ("one\r", "60"),
            ("  return 1;", "da"),
            ("alpha", "c8"),
            ("beta", "89"),
            ("gamma", "6d"),
            ("unique_beta", "8d"),
        ];

        for (line, expected) in cases {
            assert_eq!(compute_line_hash(line), expected);
        }
    }

    /// Verifies that line references can be copied from hashline output.
    #[test]
    fn line_ref_parses_copied_hashline_prefixes() {
        let parsed = LineRef::try_from("5 : aB|content").expect("line ref should parse");

        assert_eq!(parsed, LineRef::new(5, "ab"));
    }

    /// Verifies that invalid references are rejected before edits can run.
    #[test]
    fn line_ref_rejects_invalid_values() {
        let _ = LineRef::try_from("0:aa").unwrap_err();
        let _ = LineRef::try_from("missing").unwrap_err();
        let _ = LineRef::try_from("3:").unwrap_err();
    }

    /// Verifies that formatted output uses one-indexed line numbers.
    #[test]
    fn format_hash_lines_uses_start_line() {
        let formatted = format_hash_lines("alpha\nbeta", 10);

        assert_eq!(formatted, "10:c8|alpha\n11:89|beta");
    }
}

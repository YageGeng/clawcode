//! Hashline edit operation parsing and normalized internal edit shapes.

use std::convert::TryFrom;

use serde::Deserialize;

use super::super::format::LineRef;
use super::cleanup::ReplacementNormalizer;
use super::outcome::HashlineEditError;

/// One model-provided hashline edit operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HashlineEdit {
    /// Replace one anchored line.
    SetLine { set_line: SetLineEdit },
    /// Replace an anchored inclusive line range.
    ReplaceLines { replace_lines: ReplaceLinesEdit },
    /// Insert text after one anchored line.
    InsertAfter { insert_after: InsertAfterEdit },
}

/// Input payload for a `set_line` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct SetLineEdit {
    /// `LINE:HASH` anchor copied from hashline read output.
    pub anchor: String,
    /// Replacement text, or an empty string to delete the line.
    pub new_text: String,
}

/// Input payload for a `replace_lines` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct ReplaceLinesEdit {
    /// Start `LINE:HASH` anchor.
    pub start_anchor: String,
    /// Optional end `LINE:HASH` anchor; omitted means replace only start_anchor.
    #[serde(default)]
    pub end_anchor: Option<String>,
    /// Replacement text, or an empty string to delete the range.
    pub new_text: String,
}

/// Input payload for an `insert_after` operation.
#[derive(Debug, Clone, Deserialize)]
pub struct InsertAfterEdit {
    /// `LINE:HASH` anchor after which text is inserted.
    pub anchor: String,
    /// Inserted text. Empty text is rejected.
    #[serde(alias = "content")]
    pub text: String,
}

/// Internal anchor shape after parsing request text.
#[derive(Debug, Clone)]
pub(super) enum ParsedSpec {
    Single { reference: LineRef },
    Range { start: LineRef, end: LineRef },
    InsertAfter { after: LineRef },
}

/// Internal edit shape with parsed anchors and normalized destination lines.
#[derive(Debug, Clone)]
pub(super) struct ParsedEdit {
    pub(super) spec: ParsedSpec,
    pub(super) dst_lines: Vec<String>,
}

impl ParsedEdit {
    /// Split model replacement text into replacement lines.
    fn split_dst_lines(value: &str) -> Vec<String> {
        if value.is_empty() {
            Vec::new()
        } else {
            value.split('\n').map(str::to_string).collect()
        }
    }

    /// Parse a `LINE:HASH` reference and normalize the error into edit context.
    fn parse_line_ref(value: &str) -> Result<LineRef, HashlineEditError> {
        LineRef::try_from(value).map_err(|error| HashlineEditError::Invalid(error.to_string()))
    }
}

impl TryFrom<&HashlineEdit> for ParsedEdit {
    type Error = HashlineEditError;

    /// Parse one model edit into an internal anchored edit.
    fn try_from(edit: &HashlineEdit) -> Result<Self, Self::Error> {
        match edit {
            HashlineEdit::SetLine { set_line } => Ok(Self {
                spec: ParsedSpec::Single {
                    reference: Self::parse_line_ref(&set_line.anchor)?,
                },
                dst_lines: ReplacementNormalizer::strip_new_line_prefixes(Self::split_dst_lines(
                    &set_line.new_text,
                )),
            }),
            HashlineEdit::ReplaceLines { replace_lines } => {
                let start = Self::parse_line_ref(&replace_lines.start_anchor)?;
                let end = match &replace_lines.end_anchor {
                    Some(end_anchor) => Self::parse_line_ref(end_anchor)?,
                    None => start.clone(),
                };
                if start.line > end.line {
                    return Err(HashlineEditError::Invalid(format!(
                        "Range start line {} must be <= end line {}",
                        start.line, end.line
                    )));
                }
                let spec = if start.line == end.line {
                    ParsedSpec::Single { reference: start }
                } else {
                    ParsedSpec::Range { start, end }
                };
                Ok(Self {
                    spec,
                    dst_lines: ReplacementNormalizer::strip_new_line_prefixes(
                        Self::split_dst_lines(&replace_lines.new_text),
                    ),
                })
            }
            HashlineEdit::InsertAfter { insert_after } => {
                let dst_lines = ReplacementNormalizer::strip_new_line_prefixes(
                    Self::split_dst_lines(&insert_after.text),
                );
                if dst_lines.is_empty() {
                    return Err(HashlineEditError::Invalid(
                        "Insert-after edit requires non-empty dst".to_string(),
                    ));
                }
                Ok(Self {
                    spec: ParsedSpec::InsertAfter {
                        after: Self::parse_line_ref(&insert_after.anchor)?,
                    },
                    dst_lines,
                })
            }
        }
    }
}

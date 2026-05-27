//! Hashline edit application outcomes and diagnostics.

use thiserror::Error;

/// A stale hashline anchor observed during validation.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(super) struct HashMismatch {
    pub(super) line: usize,
    pub(super) expected: String,
    pub(super) actual: String,
    #[builder(default)]
    pub(super) reason: HashMismatchReason,
}

/// Additional context for why a stale-anchor mismatch was reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum HashMismatchReason {
    #[default]
    AnchorChanged,
    RangeSpanChanged {
        original_count: usize,
        relocated_count: Option<usize>,
    },
}

impl HashMismatch {
    /// Create a normal stale-anchor mismatch.
    pub(super) fn anchor_changed(
        line: usize,
        expected: String,
        actual: String,
    ) -> Self {
        Self::builder()
            .line(line)
            .expected(expected)
            .actual(actual)
            .build()
    }

    /// Attach range-span relocation context to this mismatch.
    pub(super) fn with_range_span_changed(
        mut self,
        original_count: usize,
        relocated_count: Option<usize>,
    ) -> Self {
        self.reason = HashMismatchReason::RangeSpanChanged {
            original_count,
            relocated_count,
        };
        self
    }
}

/// A no-op edit that targeted content already matching the replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoopEdit {
    /// Original edit index from the request.
    pub edit_index: usize,
    /// `LINE:HASH` location that was targeted.
    pub loc: String,
    /// Current content at that location.
    pub current_content: String,
}

impl NoopEdit {
    /// Format a no-op diagnostic that tells the model to re-read current content.
    pub(super) fn format_batch_error(
        path: &str,
        noop_edits: &[Self],
    ) -> String {
        let mut diagnostic = format!(
            "No changes made to {path}. The edits produced identical content."
        );
        if !noop_edits.is_empty() {
            let details = noop_edits
                .iter()
                .map(|edit| {
                    format!(
                        "Edit {}: replacement for {} is identical to current content:\n  {}| {}",
                        edit.edit_index, edit.loc, edit.loc, edit.current_content
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            diagnostic.push('\n');
            diagnostic.push_str(&details);
            diagnostic.push_str("\nYour content must differ from what the file already contains. Re-read the file to see the current state.");
        }
        diagnostic
    }
}

/// Result returned after applying hashline edits to normalized content.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct HashlineApplyResult {
    /// Updated content after all edits have been applied.
    pub content: String,
    /// First one-indexed line changed by the edit batch.
    #[builder(default, setter(strip_option))]
    pub first_changed_line: Option<usize>,
    /// Warnings intended for model-facing output.
    #[builder(default)]
    pub warnings: Vec<String>,
    /// Edits that did not change content.
    #[builder(default)]
    pub noop_edits: Vec<NoopEdit>,
}

/// Error returned by hashline edit parsing or application.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HashlineEditError {
    /// A model-provided edit shape or range was invalid.
    #[error("{0}")]
    Invalid(String),
    /// One or more anchors were stale.
    #[error("{0}")]
    Mismatch(String),
}

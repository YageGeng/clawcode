//! Line counts for legacy ACP snapshots, using the same line tokens and algorithm as NSH's DiffView.

use imara_diff::{Algorithm, Diff, InternedInput};
use nsh_core::prelude::*;

/// Added and removed lines in a single edit.
#[derive(Debug, JsObject)]
pub struct LineCounts {
    /// Lines inserted into the new snapshot.
    pub added: u32,
    /// Lines deleted from the old snapshot.
    pub removed: u32,
}

/// The desktop bridge for counting a snapshot before its viewer is mounted.
#[module]
pub struct SnapshotDiff;

#[module]
impl SnapshotDiff {
    /// Counts the edited lines with the same histogram and indent heuristic as the native viewer.
    pub fn counts(old_text: String, new_text: String) -> LineCounts {
        let input = InternedInput::new(old_text.as_str(), new_text.as_str());
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        LineCounts {
            added: diff.count_additions(),
            removed: diff.count_removals(),
        }
    }
}

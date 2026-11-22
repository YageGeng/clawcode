//! The active branch of one Session as ordered positions, and the page
//! windows cut from it.

use std::collections::HashMap;
use std::ops::Range;

use protocol::{EntryId, RunId, TrajectoryCursor};

/// One branch entry, without its JSON.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(super) struct BranchEntry {
    /// Entry identifier.
    pub(super) id: EntryId,
    /// Log sequence.
    pub(super) sequence: u64,
    /// Message content type; `None` for non-message entries.
    #[builder(default)]
    pub(super) message_type: Option<String>,
    /// Run open on the lane when the entry was written.
    #[builder(default)]
    pub(super) run_id: Option<RunId>,
}

impl BranchEntry {
    /// Whether the entry answers a tool call.
    fn is_tool_result(&self) -> bool {
        self.message_type.as_deref() == Some("tool_result")
    }

    /// Whether the entry is one model request's output.
    pub(super) fn is_assistant(&self) -> bool {
        self.message_type.as_deref() == Some("assistant")
    }
}

/// Which branch positions a page covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageWindow {
    /// Positions on the branch, root at 0.
    pub(super) range: Range<usize>,
    /// Older entries exist before the window.
    pub(super) has_more_before: bool,
    /// After queries: newer entries exist beyond the window.
    pub(super) has_more_after: bool,
}

/// The branch from the root to the leaf, with request and Run numbering.
#[derive(Debug, Default)]
pub(super) struct BranchIndex {
    entries: Vec<BranchEntry>,
    /// Assistant entries up to and including each position.
    requests: Vec<u32>,
    /// One-based order in which each Run first appears.
    runs: HashMap<RunId, u32>,
}

impl FromIterator<BranchEntry> for BranchIndex {
    /// Indexes entries given root first.
    fn from_iter<I: IntoIterator<Item = BranchEntry>>(iter: I) -> Self {
        let mut index = Self::default();
        let mut requests = 0;
        for entry in iter {
            requests += u32::from(entry.is_assistant());
            index.requests.push(requests);
            if let Some(run) = &entry.run_id {
                let next =
                    u32::try_from(index.runs.len() + 1).unwrap_or(u32::MAX);
                index.runs.entry(run.clone()).or_insert(next);
            }
            index.entries.push(entry);
        }
        index
    }
}

impl BranchIndex {
    /// The entries, root first.
    pub(super) fn entries(&self) -> &[BranchEntry] {
        &self.entries
    }

    /// The positions a page at `cursor` covers, at most `limit` entries
    /// before widening so a tool result never leaves its call's page.
    pub(super) fn window(
        &self,
        cursor: TrajectoryCursor,
        limit: usize,
    ) -> PageWindow {
        let len = self.entries.len();
        let (mut start, mut end) = match cursor {
            TrajectoryCursor::Tail => (len.saturating_sub(limit), len),
            TrajectoryCursor::Before(sequence) => {
                let end = self
                    .entries
                    .partition_point(|entry| entry.sequence < sequence);
                (end.saturating_sub(limit), end)
            }
            TrajectoryCursor::After(sequence) => {
                let start = self
                    .entries
                    .partition_point(|entry| entry.sequence <= sequence);
                (start, len.min(start + limit))
            }
        };
        while start > 0
            && self
                .entries
                .get(start)
                .is_some_and(BranchEntry::is_tool_result)
        {
            start -= 1;
        }
        while self
            .entries
            .get(end)
            .is_some_and(BranchEntry::is_tool_result)
        {
            end += 1;
        }
        PageWindow {
            range: start..end,
            has_more_before: start > 0,
            has_more_after: matches!(cursor, TrajectoryCursor::After(_))
                && end < len,
        }
    }

    /// Assistant entries up to and including `position`: the request
    /// number of an assistant entry there.
    pub(super) fn request_index(&self, position: usize) -> u32 {
        self.requests.get(position).copied().unwrap_or_default()
    }

    /// Whether an entry with this sequence is on the branch.
    pub(super) fn contains(&self, sequence: u64) -> bool {
        self.entries
            .binary_search_by_key(&sequence, |entry| entry.sequence)
            .is_ok()
    }

    /// The entries before `window` and the entries in it.
    pub(super) fn split(
        &self,
        window: &PageWindow,
    ) -> (&[BranchEntry], &[BranchEntry]) {
        (
            self.entries.get(..window.range.start).unwrap_or_default(),
            self.entries.get(window.range.clone()).unwrap_or_default(),
        )
    }

    /// One-based order of the Run on the branch.
    pub(super) fn run_index(&self, run: &RunId) -> Option<u32> {
        self.runs.get(run).copied()
    }
}

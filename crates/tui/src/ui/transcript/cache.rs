//! Cached wrapped transcript rows for the local TUI.

use std::collections::{HashMap, HashSet};

use ratatui::text::Line;

use crate::ui::theme::Theme;
use crate::ui::transcript::cell::TranscriptRenderMode;
use crate::ui::transcript::entry::{TranscriptEntry, TranscriptEntryId};
use crate::ui::transcript::wrap::wrap_display_lines;

/// Cached soft-wrapped rows for one transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct CachedEntryLines {
    /// Entry revision used to build these cached rows.
    revision: u64,
    /// Wrapped physical rows ready for viewport slicing.
    lines: Vec<Line<'static>>,
}

/// Per-entry transcript render cache for one terminal width.
#[derive(Debug, Default)]
pub(in crate::ui) struct TranscriptRenderCache {
    /// Terminal width used by all cached entries.
    width: Option<u16>,
    /// Transcript render mode used by all cached entries.
    render_mode: Option<TranscriptRenderMode>,
    /// Cached rows keyed by stable transcript entry id.
    entries: HashMap<TranscriptEntryId, CachedEntryLines>,
    /// Number of entry rebuilds observed by cache tests.
    #[cfg(test)]
    rebuild_count: usize,
}

impl TranscriptRenderCache {
    /// Creates an empty render cache.
    pub(in crate::ui) fn new() -> Self {
        Self::default()
    }

    /// Returns cached wrapped rows for one entry, rebuilding only when needed.
    pub(in crate::ui) fn entry_lines(
        &mut self,
        width: u16,
        theme: &Theme,
        entry: &TranscriptEntry,
        render_mode: TranscriptRenderMode,
    ) -> &[Line<'static>] {
        if self.width != Some(width) || self.render_mode != Some(render_mode) {
            self.width = Some(width);
            self.render_mode = Some(render_mode);
            self.entries.clear();
        }

        let needs_rebuild = self
            .entries
            .get(&entry.id())
            .map(|cached| cached.revision != entry.revision())
            .unwrap_or(true);

        if needs_rebuild {
            #[cfg(test)]
            {
                self.rebuild_count = self.rebuild_count.saturating_add(1);
            }
            let lines = wrap_display_lines(
                entry
                    .cell()
                    .display_lines_for_mode(width, theme, render_mode),
                width,
            );
            self.entries.insert(
                entry.id(),
                CachedEntryLines {
                    revision: entry.revision(),
                    lines,
                },
            );
        }

        self.entries
            .get(&entry.id())
            .map(|cached| cached.lines.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the number of cached rows for one entry, rebuilding only when needed.
    pub(in crate::ui) fn entry_line_count(
        &mut self,
        width: u16,
        theme: &Theme,
        entry: &TranscriptEntry,
        render_mode: TranscriptRenderMode,
    ) -> usize {
        self.entry_lines(width, theme, entry, render_mode).len()
    }

    /// Retains cache entries that still exist in transcript state.
    pub(in crate::ui) fn retain_entries(
        &mut self,
        ids: impl Iterator<Item = TranscriptEntryId>,
    ) {
        let live = ids.collect::<HashSet<_>>();
        self.entries.retain(|id, _| live.contains(id));
    }

    /// Returns the number of entry rebuilds in tests.
    #[cfg(test)]
    pub(in crate::ui) fn rebuild_count(&self) -> usize {
        self.rebuild_count
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::ui::cell::{TextCell, TextRole};
    use crate::ui::theme::Theme;
    use crate::ui::transcript::entry::{
        TranscriptEntry, TranscriptEntryId, TranscriptEntryState,
    };

    use super::*;

    /// Verifies only changed entries rebuild after an active update.
    #[test]
    fn render_cache_rebuilds_only_changed_entry() {
        let theme = Theme::dark();
        let mut cache = TranscriptRenderCache::new();
        let committed = TranscriptEntry::new(
            TranscriptEntryId::new(1),
            TranscriptEntryState::Committed,
            Arc::new(TextCell::new(TextRole::User, "old history")),
        );
        let mut active = TranscriptEntry::new(
            TranscriptEntryId::new(2),
            TranscriptEntryState::Active,
            Arc::new(TextCell::new(TextRole::Assistant, "hel")),
        );

        let _ = cache.entry_lines(
            80,
            &theme,
            &committed,
            TranscriptRenderMode::Rich,
        );
        let _ =
            cache.entry_lines(80, &theme, &active, TranscriptRenderMode::Rich);
        assert_eq!(cache.rebuild_count(), 2);

        let text = active.text_cell().expect("text cell");
        let mut updated = text.clone();
        updated.push_str("lo");
        active.replace_cell(Arc::new(updated));
        let _ = cache.entry_lines(
            80,
            &theme,
            &committed,
            TranscriptRenderMode::Rich,
        );
        let _ =
            cache.entry_lines(80, &theme, &active, TranscriptRenderMode::Rich);

        assert_eq!(cache.rebuild_count(), 3);
    }

    /// Verifies rich and raw transcript modes do not reuse the same cached rows.
    #[test]
    fn render_cache_rebuilds_when_render_mode_changes() {
        let theme = Theme::dark();
        let mut cache = TranscriptRenderCache::new();
        let entry = TranscriptEntry::new(
            TranscriptEntryId::new(1),
            TranscriptEntryState::Committed,
            Arc::new(TextCell::new(TextRole::User, "copy me")),
        );

        let rich =
            cache.entry_lines(80, &theme, &entry, TranscriptRenderMode::Rich);
        assert_eq!(line_text(rich), vec!["> copy me".to_string()]);

        let raw =
            cache.entry_lines(80, &theme, &entry, TranscriptRenderMode::Raw);
        assert_eq!(line_text(raw), vec!["copy me".to_string()]);
        assert_eq!(cache.rebuild_count(), 2);
    }

    /// Converts cached line slices into owned text for cache tests.
    fn line_text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }
}

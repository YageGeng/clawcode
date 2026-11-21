//! The tab strip's entries and selection, independent of any window.

use crate::state::TabsState;
use crate::ui::TabEntry;

/// Open tabs in strip order and the active one.
///
/// Entries are unique, so every rule that moves tabs finds the active entry
/// again by value instead of tracking index arithmetic.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tabs {
    entries: Vec<TabEntry>,
    active: Option<usize>,
}

impl Tabs {
    /// Tabs restored from `desktop.json`: session tabs only, active index clamped.
    pub fn restored(state: &TabsState) -> Self {
        let entries: Vec<TabEntry> = state
            .open
            .iter()
            .map(|id| TabEntry::Session(id.clone().into()))
            .collect();
        let active = state.active.filter(|index| *index < entries.len());
        Self { entries, active }
    }

    /// Open tabs in order.
    pub fn entries(&self) -> &[TabEntry] {
        &self.entries
    }

    /// Index of the active tab.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// The active tab entry.
    pub fn active_entry(&self) -> Option<&TabEntry> {
        self.active.and_then(|index| self.entries.get(index))
    }

    /// Number of open tabs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no tab is open.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current position of a tab.
    pub fn position(&self, entry: &TabEntry) -> Option<usize> {
        self.entries.iter().position(|tab| tab == entry)
    }

    /// Appends a tab unless it is open already; returns its position.
    pub fn open(&mut self, entry: TabEntry) -> usize {
        match self.position(&entry) {
            Some(index) => index,
            None => {
                self.entries.push(entry);
                self.entries.len() - 1
            }
        }
    }

    /// Removes a tab; the right neighbor slides into an active slot, or the left one at the end.
    pub fn close(&mut self, index: usize) -> Option<TabEntry> {
        if index >= self.entries.len() {
            return None;
        }
        let closed = self.entries.remove(index);
        self.active = match self.active {
            _ if self.entries.is_empty() => None,
            Some(active) if active == index => {
                Some(index.min(self.entries.len() - 1))
            }
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
        Some(closed)
    }

    /// Selects a tab; returns `false` when the index is out of range.
    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.entries.len() {
            return false;
        }
        self.active = Some(index);
        true
    }

    /// Moves the tab at `from` to `to`; the active entry stays active.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.entries.len() || to >= self.entries.len()
        {
            return false;
        }
        let active = self.active_entry().cloned();
        let tab = self.entries.remove(from);
        self.entries.insert(to, tab);
        self.active = active.and_then(|active| self.position(&active));
        true
    }

    /// Replaces `old` in place, keeping its position and selection; `false` when it is gone.
    pub fn replace(&mut self, old: &TabEntry, new: TabEntry) -> bool {
        match self.entries.iter_mut().find(|tab| *tab == old) {
            Some(tab) => {
                *tab = new;
                true
            }
            None => false,
        }
    }

    /// Keeps only matching tabs; the active entry stays active if kept, else the first tab is.
    pub fn retain(&mut self, keep: impl FnMut(&TabEntry) -> bool) {
        let active = self.active_entry().cloned();
        self.entries.retain(keep);
        self.active = active
            .and_then(|active| self.position(&active))
            .or_else(|| (!self.entries.is_empty()).then_some(0));
    }

    /// The persisted form: session tabs only, with the active index among them.
    pub fn persisted(&self) -> TabsState {
        let sessions: Vec<(usize, String)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| match tab {
                TabEntry::Session(id) => Some((index, id.to_string())),
                TabEntry::Draft { .. } | TabEntry::Settings => None,
            })
            .collect();
        let active = self.active.and_then(|active| {
            sessions.iter().position(|(index, _)| *index == active)
        });
        TabsState {
            open: sessions.into_iter().map(|(_, id)| id).collect(),
            active,
        }
    }
}

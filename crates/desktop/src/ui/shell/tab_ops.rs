//! Tab operations of the shell: opening, closing, moving, and activating
//! tabs, the active tab's view, and the tab key actions.

use std::path::PathBuf;

use gpui_kit::{Context, Entity, Window};

use crate::actions::{CloseTab, NextTab, SelectTab};
use crate::backend::SessionId;
use crate::session::SessionHandle;
use crate::ui::{AppView, SessionTarget, TabEntry};

impl AppView {
    /// Activates a session's tab, opening one if needed.
    pub fn open_session(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        let index = self.tabs.open(TabEntry::Session(id.clone()));
        self.activate(index, cx);
    }

    /// Opens a draft tab for a new session in `workspace`, reusing an existing draft for it.
    pub fn open_draft(
        &mut self,
        workspace: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) {
        let index = self.tabs.open(TabEntry::Draft { workspace });
        self.activate(index, cx);
    }

    /// Closes a tab; a session tab releases its session, which closes it
    /// once idle, unless a panel still shows it. The tab's panel views go
    /// too (their saved state stays), releasing the subagents they showed.
    pub fn close_tab(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(closed) = self.tabs.close(index) else {
            return;
        };
        self.views.remove(&closed);
        let mut released = closed
            .panel_owner()
            .map(|owner| self.drop_panel_views(&owner))
            .unwrap_or_default();
        if let TabEntry::Session(id) = &closed {
            released.push(id.clone());
        }
        self.release_unshown(released, cx);
        if let Some(active) = self.tabs.active() {
            self.tab_scroll.scroll_to_item(active);
        }
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Current position of a tab.
    pub(in crate::ui) fn tab_index(&self, entry: &TabEntry) -> Option<usize> {
        self.tabs.position(entry)
    }

    /// Closes this tab wherever it is now; nothing happens when it is already gone.
    pub fn close_entry(
        &mut self,
        entry: &TabEntry,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(index) = self.tab_index(entry) {
            self.close_tab(index, cx);
        }
    }

    /// Moves the tab at `from` to position `to`; the active entry stays active.
    pub fn move_tab(
        &mut self,
        from: usize,
        to: usize,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.tabs.move_tab(from, to) {
            return;
        }
        if let Some(active) = self.tabs.active() {
            self.tab_scroll.scroll_to_item(active);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Makes a tab active and attaches its session if that has not happened yet.
    pub(in crate::ui) fn activate(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.tabs.activate(index) {
            return;
        }
        self.tab_scroll.scroll_to_item(index);
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// The active tab entry.
    pub fn active_entry(&self) -> Option<TabEntry> {
        self.tabs.active_entry().cloned()
    }

    /// The attached session with this id, if any.
    pub fn session_handle(
        &self,
        id: &SessionId,
        cx: &gpui_kit::App,
    ) -> Option<Entity<SessionHandle>> {
        self.host
            .registry
            .as_ref()
            .and_then(|registry| registry.read(cx).handle(id))
    }

    /// Creates the active tab's view on first show, and points it at the
    /// session's current handle (a reattach or a backend restart replaces it).
    ///
    /// Runs at render time because views need the `Window` to build their inputs.
    pub(in crate::ui) fn ensure_active_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.active_entry() else {
            return;
        };
        let target = match &entry {
            TabEntry::Draft { workspace } => SessionTarget::Draft {
                workspace: workspace.clone(),
            },
            TabEntry::Session(id) => match self.session_handle(id, cx) {
                Some(handle) => SessionTarget::Session(handle),
                // Not attached yet: the card shows its loading state.
                None => return,
            },
        };
        let view = match self.views.get(&entry) {
            Some(view) => {
                let stale = match &target {
                    SessionTarget::Session(handle) => {
                        view.read(cx).handle() != Some(handle)
                    }
                    SessionTarget::Draft { .. } => false,
                };
                if stale {
                    // The view renders cached and a notify made while drawing
                    // schedules no frame, so retarget right after this one.
                    let view = view.clone();
                    cx.defer_in(window, move |_, window, cx| {
                        view.update(cx, |view, cx| {
                            view.set_target(target, window, cx)
                        });
                    });
                }
                view.clone()
            }
            None => self.create_view(entry, target, window, cx),
        };
        let client = self.host.client.as_ref();
        view.update(cx, |view, _| view.set_client(client));
    }

    /// Attaches the active session tab; restored tabs attach only when they become active.
    pub(in crate::ui) fn attach_active(&mut self, cx: &mut Context<'_, Self>) {
        let Some(TabEntry::Session(id)) = self.active_entry() else {
            return;
        };
        let (Some(registry), Some(summary)) =
            (&self.host.registry, self.host.catalog.get(&id))
        else {
            return;
        };
        let summary = summary.clone();
        registry.update(cx, |registry, cx| {
            registry.attach(&summary, cx);
        });
    }

    /// `CloseTab`: closes the active tab.
    pub(in crate::ui) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(index) = self.tabs.active() {
            self.close_tab(index, cx);
        }
    }

    /// `NextTab`: cycles forward through the tabs.
    pub(in crate::ui) fn on_next_tab(
        &mut self,
        _: &NextTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self
            .tabs
            .active()
            .map_or(0, |active| (active + 1) % self.tabs.len());
        self.activate(next, cx);
    }

    /// `SelectTab`: jumps to a tab by position.
    pub(in crate::ui) fn on_select_tab(
        &mut self,
        action: &SelectTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.activate(action.index, cx);
    }
}

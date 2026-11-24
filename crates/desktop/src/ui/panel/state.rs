//! The panel tabs each owner saves: opening, closing, and switching them.

use std::path::PathBuf;

use gpui_kit::{Context, Entity, Window};

use super::{FileView, PanelView};

use crate::actions::{ShowChanges, ShowFiles, TogglePanel};
use crate::backend::SessionId;
use crate::state::{PanelState, PanelTabKey};
use crate::ui::{AppView, TabEntry};

/// Whose panel the active tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelOwner {
    /// A session tab's panel.
    Session(SessionId),
    /// A new-session draft's panel, by the workspace the draft runs in.
    Draft(PathBuf),
}

impl PanelOwner {
    /// The key the owner's panel is saved under.
    pub fn key(&self) -> String {
        match self {
            PanelOwner::Session(id) => format!("session:{id}"),
            PanelOwner::Draft(workspace) => {
                format!("draft:{}", workspace.display())
            }
        }
    }
}

impl TabEntry {
    /// The panel owner of a main tab; drafts without a workspace and the settings have none.
    pub fn panel_owner(&self) -> Option<PanelOwner> {
        match self {
            TabEntry::Session(id) => Some(PanelOwner::Session(id.clone())),
            TabEntry::Draft {
                workspace: Some(workspace),
            } => Some(PanelOwner::Draft(workspace.clone())),
            TabEntry::Draft { workspace: None } => None,
        }
    }
}

impl AppView {
    /// The owner of the active tab's panel.
    pub fn panel_owner(&self) -> Option<PanelOwner> {
        self.active_entry()?.panel_owner()
    }

    /// The active tab's panel, when it has one saved.
    pub fn panel(&self) -> Option<&PanelState> {
        self.state.panels.get(&self.panel_owner()?.key())
    }

    /// The open preview of `path` in the active tab's panel.
    pub fn panel_file_view(&self, path: &str) -> Option<Entity<FileView>> {
        let key = self.panel_owner()?.key();
        let tab = PanelTabKey::File {
            path: path.to_owned(),
        };
        match self.panels.runtimes.get(&key)?.views.get(&tab)? {
            PanelView::File(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The active tab's panel for changing, created closed the first time.
    pub(super) fn panel_mut(&mut self) -> Option<&mut PanelState> {
        let key = self.panel_owner()?.key();
        let panel = self.state.panels.entry(key).or_default();
        panel.last_used_ms = crate::utils::now_ms();
        Some(panel)
    }

    /// Opens the panel on `tab`, adding the tab when it is not open yet.
    pub fn open_panel_tab(
        &mut self,
        tab: PanelTabKey,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(panel) = self.panel_mut() else {
            return;
        };
        let index = match panel.tabs.iter().position(|open| *open == tab) {
            Some(index) => index,
            None => {
                panel.tabs.push(tab);
                panel.tabs.len() - 1
            }
        };
        panel.active = Some(index);
        panel.open = true;
        self.focus_panel_sheet(window, cx);
        self.persist(cx);
        cx.notify();
    }

    /// Activates the panel tab at `index`.
    pub(super) fn activate_panel_tab(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(panel) = self.panel_mut()
            && index < panel.tabs.len()
        {
            panel.active = Some(index);
            self.persist(cx);
            cx.notify();
        }
    }

    /// Closes the panel tab at `index` and drops its view; the panel closes with its last tab.
    pub(super) fn close_panel_tab(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(owner) = self.panel_owner() else {
            return;
        };
        let Some(panel) = self.panel_mut() else {
            return;
        };
        if index >= panel.tabs.len() {
            return;
        }
        let tab = panel.tabs.remove(index);
        panel.active = match panel.active {
            _ if panel.tabs.is_empty() => None,
            Some(active) if active >= index => {
                Some(active.saturating_sub(1).min(panel.tabs.len() - 1))
            }
            active => active,
        };
        if panel.tabs.is_empty() {
            panel.open = false;
            panel.maximized = false;
        }
        if let Some(runtime) = self.panels.runtimes.get_mut(&owner.key()) {
            runtime.views.remove(&tab);
        }
        if let PanelTabKey::Subagent { session } = tab {
            self.release_unshown(vec![SessionId::from(session)], cx);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Shows or hides the panel; an empty panel opens on Changes, or Files outside Git.
    pub fn toggle_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let not_git = self.current_workspace().is_some_and(|workspace| {
            self.is_repository(&workspace) == Some(false)
        });
        let Some(panel) = self.panel_mut() else {
            return;
        };
        if panel.open {
            panel.open = false;
            self.persist(cx);
            cx.notify();
            return;
        }
        if panel.tabs.is_empty() {
            let tab = if not_git {
                PanelTabKey::Files
            } else {
                PanelTabKey::Changes
            };
            self.open_panel_tab(tab, window, cx);
            return;
        }
        panel.open = true;
        self.focus_panel_sheet(window, cx);
        self.persist(cx);
        cx.notify();
    }

    /// Lets the panel take the main card's place, or gives it back.
    pub(super) fn toggle_panel_maximized(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(panel) = self.panel_mut() {
            panel.maximized = !panel.maximized;
            self.persist(cx);
            cx.notify();
        }
    }

    /// Closes the panel, keeping its tabs.
    pub(super) fn close_panel(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(panel) = self.panel_mut() {
            panel.open = false;
            self.persist(cx);
            cx.notify();
        }
    }

    /// `TogglePanel`: shows or hides the active tab's panel.
    pub(in crate::ui) fn on_toggle_panel(
        &mut self,
        _: &TogglePanel,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_panel(window, cx);
    }

    /// `ShowFiles`: the panel's Files tab.
    pub(in crate::ui) fn on_show_files(
        &mut self,
        _: &ShowFiles,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_panel_tab(PanelTabKey::Files, window, cx);
    }

    /// `ShowChanges`: the panel's Changes tab.
    pub(in crate::ui) fn on_show_changes(
        &mut self,
        _: &ShowChanges,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_panel_tab(PanelTabKey::Changes, window, cx);
    }

    /// Shows `scope` in the active panel's Changes tab, when it is built.
    pub(in crate::ui) fn set_changes_scope(
        &mut self,
        scope: protocol::WorkspaceDiffScope,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(owner) = self.panel_owner() else {
            return;
        };
        if let Some(PanelView::Changes(changes)) = self
            .panels
            .runtimes
            .get(&owner.key())
            .and_then(|runtime| runtime.views.get(&PanelTabKey::Changes))
        {
            changes.update(cx, |changes, cx| {
                changes.set_scope(super::changes::ChangesScope::Git(scope), cx)
            });
        }
    }
}

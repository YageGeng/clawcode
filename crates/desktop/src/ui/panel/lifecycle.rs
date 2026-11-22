//! The views behind panel tabs: built when first shown, moved, and dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, IntoElement,
    Window,
};
use typed_builder::TypedBuilder;

use super::{
    ChangesView, FileView, FilesEvent, FilesView, PanelOwner, RecordView,
    ViewerSource, changes,
};
use crate::backend::SessionId;
use crate::state::PanelTabKey;
use crate::ui::session::{SessionView, SessionViewEvent};
use crate::ui::{AppView, TabEntry, TrajectoryEvent};

/// The view behind one panel tab.
#[derive(Clone)]
pub enum PanelView {
    /// The workspace's changes.
    Changes(Entity<ChangesView>),
    /// The workspace's file tree.
    Files(Entity<FilesView>),
    /// One file's preview.
    File(Entity<FileView>),
    /// A subagent session.
    Subagent(Entity<SessionView>),
    /// The record selected in the session's trajectory.
    Record(Entity<RecordView>),
}

impl PanelView {
    /// Reads the tab's content again when it becomes active.
    pub(super) fn reload(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        match self {
            PanelView::Changes(view) => {
                view.update(cx, |view, cx| view.reload(cx))
            }
            PanelView::Files(view) => {
                view.update(cx, |view, cx| view.reload(cx))
            }
            PanelView::File(view) => {
                view.update(cx, |view, cx| view.reload(window, cx))
            }
            PanelView::Subagent(_) | PanelView::Record(_) => {}
        }
    }

    /// The view as a child element.
    pub(super) fn element(&self) -> AnyElement {
        match self {
            PanelView::Changes(view) => view.clone().into_any_element(),
            PanelView::Files(view) => view.clone().into_any_element(),
            PanelView::File(view) => view.clone().into_any_element(),
            PanelView::Subagent(view) => view.clone().into_any_element(),
            PanelView::Record(view) => view.clone().into_any_element(),
        }
    }
}

/// The shell's right panel views, and what showed on the last frame.
#[derive(TypedBuilder)]
pub struct PanelHost {
    /// Views of the panels' tabs, by panel owner key.
    #[builder(default)]
    pub(in crate::ui) runtimes: HashMap<String, PanelRuntime>,
    /// The panel tab shown on the last frame; activating another one reloads it.
    #[builder(default)]
    pub(in crate::ui) shown: Option<(String, PanelTabKey)>,
    /// Focus target of the panel sheet, so Esc closes it.
    pub(in crate::ui) sheet_focus: FocusHandle,
    /// Whether the panel showed as a sheet on the last frame.
    #[builder(default)]
    pub(in crate::ui) was_sheet: bool,
    /// A run to show in a panel's Changes tab once that view exists: the
    /// panel owner's key, the run, and the file to bring into view.
    #[builder(default)]
    pub(in crate::ui) pending_changes:
        Option<(String, protocol::RunId, Option<String>)>,
}

impl PanelHost {
    /// No panel views yet; `sheet_focus` becomes the sheet's focus target.
    pub fn new(sheet_focus: FocusHandle) -> Self {
        Self::builder().sheet_focus(sheet_focus).build()
    }
}

/// The views of one owner's panel tabs, built as tabs are first shown.
#[derive(Default)]
pub struct PanelRuntime {
    /// Views by tab.
    pub(super) views: HashMap<PanelTabKey, PanelView>,
}

impl PanelRuntime {
    /// The subagent sessions among a panel's tabs.
    pub(super) fn subagents<'a>(
        tabs: impl Iterator<Item = &'a PanelTabKey>,
    ) -> Vec<SessionId> {
        tabs.filter_map(|tab| match tab {
            PanelTabKey::Subagent { session } => {
                Some(SessionId::from(session.clone()))
            }
            _ => None,
        })
        .collect()
    }
}

impl AppView {
    /// Builds the active panel tab's view the first time it shows, and reads
    /// a tab's content again when it becomes active after another one.
    ///
    /// Runs at render time, where the `Window` the views need is available.
    pub(in crate::ui) fn ensure_panel_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(owner) = self.panel_owner() else {
            return;
        };
        // The owner's own workspace, never the current-workspace fallback.
        let Some(workspace) = self.active_workspace() else {
            return;
        };
        let key = owner.key();
        let Some(tab) = self
            .state
            .panels
            .get(&key)
            .filter(|panel| panel.open)
            .and_then(|panel| panel.tabs.get(panel.active?).cloned())
        else {
            return;
        };
        let shown = Some((key.clone(), tab.clone()));
        let reactivated = self.panels.shown != shown;
        self.panels.shown = shown;
        let existing = self
            .panels
            .runtimes
            .get(&key)
            .and_then(|runtime| runtime.views.get(&tab))
            .cloned()
            // A detail of a trajectory the session dropped is built anew.
            .filter(|view| !self.is_stale_record(view, cx));
        match &existing {
            // The icon theme may have changed since the tree was built.
            Some(PanelView::Files(files)) => {
                let painter = self.file_icon_painter(cx);
                files.update(cx, |files, _| files.set_painter(painter));
            }
            // The tab shows the base and Git availability the shell last read.
            Some(PanelView::Changes(changes)) => {
                let summary =
                    self.git_status(&workspace).map(changes::GitSummary::from);
                let painter = self.file_icon_painter(cx);
                let session = self.owner_session(&owner, cx);
                changes.update(cx, |changes, cx| {
                    changes.set_painter(painter);
                    changes.set_session(session, cx);
                    changes.set_git(summary, cx)
                });
            }
            _ => {}
        }
        // Entering Changes reads the workspace's Git state again (spec §8).
        if reactivated && tab == PanelTabKey::Changes {
            self.refresh_git(workspace.clone(), cx);
        }
        match existing {
            Some(view) if reactivated => {
                cx.defer_in(window, move |_, window, cx| {
                    view.reload(window, cx)
                });
            }
            Some(_) => {}
            None => {
                if let Some(view) =
                    self.build_panel_view(&tab, workspace, window, cx)
                {
                    self.panels
                        .runtimes
                        .entry(key.clone())
                        .or_default()
                        .views
                        .insert(tab.clone(), view);
                }
            }
        }
        // A turn's change card asked for this run; the view exists now.
        if tab == PanelTabKey::Changes
            && self
                .panels
                .pending_changes
                .as_ref()
                .is_some_and(|(owner_key, ..)| *owner_key == key)
            && let Some((_, run, path)) = self.panels.pending_changes.take()
            && let Some(PanelView::Changes(changes)) = self
                .panels
                .runtimes
                .get(&key)
                .and_then(|runtime| runtime.views.get(&tab))
                .cloned()
        {
            changes.update(cx, |changes, cx| changes.show_run(run, path, cx));
        }
    }

    /// The attached session that owns a panel, if a session owns it.
    fn owner_session(
        &self,
        owner: &PanelOwner,
        cx: &gpui_kit::App,
    ) -> Option<Entity<crate::session::SessionHandle>> {
        let PanelOwner::Session(id) = owner else {
            return None;
        };
        self.host.registry.as_ref()?.read(cx).handle(id)
    }

    /// Whether `view` is a record detail following a trajectory its session
    /// no longer shows, as after the session view took a new target.
    fn is_stale_record(&self, view: &PanelView, cx: &gpui_kit::App) -> bool {
        let PanelView::Record(record) = view else {
            return false;
        };
        let current = match self.panel_owner() {
            Some(PanelOwner::Session(id)) => self
                .views
                .get(&TabEntry::Session(id))
                .and_then(|view| view.read(cx).trajectory_view().cloned()),
            _ => None,
        };
        current.as_ref() != Some(record.read(cx).trajectory())
    }

    /// A new view for one panel tab of `workspace`.
    pub(super) fn build_panel_view(
        &mut self,
        tab: &PanelTabKey,
        workspace: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<PanelView> {
        let client = self.host.client.clone();
        Some(match tab {
            PanelTabKey::Changes => PanelView::Changes({
                let painter = self.file_icon_painter(cx);
                let session = self
                    .panel_owner()
                    .and_then(|owner| self.owner_session(&owner, cx));
                cx.new(|cx| {
                    let mut view = ChangesView::new(
                        workspace, client, painter, window, cx,
                    );
                    view.set_session(session, cx);
                    view
                })
            }),
            PanelTabKey::Files => {
                let painter = self.file_icon_painter(cx);
                let view =
                    cx.new(|cx| FilesView::new(workspace, client, painter, cx));
                cx.subscribe_in(&view, window, |this, _, event, window, cx| {
                    let FilesEvent::Open(path) = event;
                    this.open_panel_tab(
                        PanelTabKey::File { path: path.clone() },
                        window,
                        cx,
                    );
                })
                .detach();
                PanelView::Files(view)
            }
            PanelTabKey::File { path } => {
                let source = ViewerSource::builder()
                    .workspace(workspace)
                    .path(path.clone())
                    .client(client)
                    .build();
                PanelView::File(cx.new(|cx| FileView::new(source, window, cx)))
            }
            PanelTabKey::Subagent { session } => {
                // The subagent attaches like a tab's session; it is released when its tab goes.
                let id = SessionId::from(session.clone());
                let summary = self.host.catalog.get(&id)?.clone();
                let registry = self.host.registry.clone()?;
                let handle = registry
                    .update(cx, |registry, cx| registry.attach(&summary, cx));
                let view = cx.new(|cx| {
                    let mut view = SessionView::new(
                        crate::ui::session::SessionTarget::Session(handle),
                        client,
                        window,
                        cx,
                    );
                    // A subagent's history is its owner's to branch or fork.
                    view.set_copy_only();
                    view
                });
                // The overview setting is shared, so its toggle saves here as
                // in the main card. The panel's record tab follows the owner
                // session's trajectory, so a subagent's record opens below
                // the subagent's own trajectory instead.
                cx.subscribe_in(
                    &view,
                    window,
                    |this, view, event, window, cx| match event {
                        SessionViewEvent::Trajectory(
                            TrajectoryEvent::Open(_),
                        ) => {
                            view.update(cx, |view, cx| view.open_detail(cx));
                        }
                        SessionViewEvent::Trajectory(
                            event @ TrajectoryEvent::TimelineToggled(_),
                        ) => this.on_trajectory_event(event, window, cx),
                        _ => {}
                    },
                )
                .detach();
                PanelView::Subagent(view)
            }
            PanelTabKey::Record => {
                // The detail follows the selection of the owner session's trajectory.
                let Some(super::PanelOwner::Session(id)) = self.panel_owner()
                else {
                    return None;
                };
                let view = self.views.get(&TabEntry::Session(id))?.clone();
                let trajectory =
                    view.update(cx, |view, cx| view.trajectory(window, cx))?;
                PanelView::Record(cx.new(|cx| RecordView::new(trajectory, cx)))
            }
            PanelTabKey::Unknown => return None,
        })
    }

    /// Moves a draft's panel to the session it became.
    pub(in crate::ui) fn move_panel(
        &mut self,
        from: &PanelOwner,
        to: &PanelOwner,
    ) {
        if let Some(panel) = self.state.panels.remove(&from.key()) {
            self.state.panels.insert(to.key(), panel);
        }
        if let Some(runtime) = self.panels.runtimes.remove(&from.key()) {
            self.panels.runtimes.insert(to.key(), runtime);
        }
    }

    /// Drops the views of an owner's panel, keeping its saved state, and
    /// returns the subagent sessions they showed.
    pub(in crate::ui) fn drop_panel_views(
        &mut self,
        owner: &PanelOwner,
    ) -> Vec<SessionId> {
        self.panels
            .runtimes
            .remove(&owner.key())
            .map(|runtime| PanelRuntime::subagents(runtime.views.keys()))
            .unwrap_or_default()
    }

    /// Forgets the panels of sessions no longer in the catalog, their views,
    /// and the subagent sessions only those views showed.
    pub(in crate::ui) fn prune_panels(&mut self, cx: &mut Context<'_, Self>) {
        let catalog = &self.host.catalog;
        self.state.prune_panels(|key| {
            key.strip_prefix("session:").is_none_or(|id| {
                catalog.get(&SessionId::from(id.to_string())).is_some()
            })
        });
        let saved = &self.state.panels;
        let mut dropped = Vec::new();
        self.panels.runtimes.retain(|key, runtime| {
            let keep = saved.contains_key(key);
            if !keep {
                dropped.extend(PanelRuntime::subagents(runtime.views.keys()));
            }
            keep
        });
        self.release_unshown(dropped, cx);
    }

    /// Drops every panel view, so they are built again with the current backend.
    pub(in crate::ui) fn reset_panel_views(&mut self) {
        self.panels.runtimes.clear();
        self.panels.shown = None;
    }

    /// Releases the sessions among `ids` that no main tab and no panel shows any more.
    pub(in crate::ui) fn release_unshown(
        &mut self,
        ids: Vec<SessionId>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(registry) = self.host.registry.clone() else {
            return;
        };
        for id in ids {
            let in_tab =
                self.tabs.position(&TabEntry::Session(id.clone())).is_some();
            let in_panel = self.panels.runtimes.values().any(|runtime| {
                runtime.views.contains_key(&PanelTabKey::Subagent {
                    session: id.to_string(),
                })
            });
            if !in_tab && !in_panel {
                registry.update(cx, |registry, cx| registry.release(&id, cx));
            }
        }
    }

    /// Reads the changes again when the shown tab is a Changes tab of `workspace`:
    /// a run there may have changed files. Hidden Changes tabs read again when
    /// they are shown (see `ensure_panel_view`), so they are skipped here.
    pub(in crate::ui) fn reload_changes(
        &mut self,
        workspace: &Path,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((key, PanelTabKey::Changes)) = &self.panels.shown else {
            return;
        };
        if let Some(PanelView::Changes(changes)) = self
            .panels
            .runtimes
            .get(key)
            .and_then(|runtime| runtime.views.get(&PanelTabKey::Changes))
            && changes.read(cx).workspace() == workspace
        {
            changes.update(cx, |changes, cx| changes.reload(cx));
        }
    }
}

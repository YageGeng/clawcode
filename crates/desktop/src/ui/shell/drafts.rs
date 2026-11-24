//! Tab views and drafts: creating a tab's view, turning a draft into a session
//! on its first message, switching a draft's workspace, and composer focus.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Entity, Window};

use crate::actions::FocusComposer;
use crate::backend::{SessionId, SessionStart, StartError};
use crate::catalog::SessionSummary;
use crate::ui::composer::attachments::Outgoing;
use crate::ui::{
    AppView, SessionTarget, SessionView, SessionViewEvent, TabEntry,
};

impl AppView {
    /// The active tab's view, once it has been shown.
    pub fn active_view(&self) -> Option<&Entity<SessionView>> {
        self.active_entry().and_then(|entry| self.views.get(&entry))
    }

    /// Creates and registers the view of a tab; a new draft's composer takes focus.
    pub(in crate::ui) fn create_view(
        &mut self,
        entry: TabEntry,
        target: SessionTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SessionView> {
        let draft = matches!(target, SessionTarget::Draft { .. });
        let client = self.host.client.clone();
        let view = cx.new(|cx| SessionView::new(target, client, window, cx));
        // The subscription ends with the view, so closed tabs leave nothing behind.
        cx.subscribe_in(&view, window, Self::on_view_event).detach();
        if draft {
            let composer = view.read(cx).composer().clone();
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        }
        self.views.insert(entry, view.clone());
        self.push_workspaces(cx);
        view
    }

    /// Routes a view's requests.
    fn on_view_event(
        &mut self,
        view: &Entity<SessionView>,
        event: &SessionViewEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SessionViewEvent::CreateSession { message } => {
                self.create_session(view, message.clone(), window, cx)
            }
            SessionViewEvent::SelectWorkspace(workspace) => {
                self.select_draft_workspace(view, workspace.clone(), window, cx)
            }
            SessionViewEvent::Trajectory(event) => {
                self.on_trajectory_event(event, window, cx)
            }
            SessionViewEvent::Forked(fork) => self.open_fork(fork.clone(), cx),
            SessionViewEvent::OpenChanges { run, path } => {
                // The Changes view may not exist yet; it takes the run when it does.
                if let Some(owner) = self.panel_owner() {
                    self.panels.pending_changes =
                        Some((owner.key(), run.clone(), path.clone()));
                }
                self.open_panel_tab(
                    crate::state::PanelTabKey::Changes,
                    window,
                    cx,
                );
                cx.notify();
            }
        }
    }

    /// Opens a session forked from a reply in a tab of its own, once the
    /// session list, which the tab reads its summary from, holds it.
    fn open_fork(&mut self, fork: SessionId, cx: &mut Context<'_, Self>) {
        let Some(client) = self.host.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| match client.list_sessions().await {
            Ok(sessions) => {
                let catalog = sessions.iter().map(Into::into).collect();
                let _ = this.update(cx, |this, cx| {
                    this.replace_catalog(catalog, cx);
                    this.open_session(&fork, cx);
                });
            }
            Err(error) => {
                tracing::warn!(
                    "failed to list sessions after a fork: {}",
                    error
                )
            }
        })
        .detach();
    }

    /// The tab a view belongs to.
    fn view_entry(&self, view: &Entity<SessionView>) -> Option<TabEntry> {
        self.views
            .iter()
            .find(|(_, candidate)| *candidate == view)
            .map(|(entry, _)| entry.clone())
    }

    /// Creates the backend session of a draft, then sends its first message.
    ///
    /// On failure the text returns to the composer and the tab stays a draft.
    pub fn create_session(
        &mut self,
        view: &Entity<SessionView>,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let cwd = match self.view_entry(view) {
            Some(TabEntry::Draft {
                workspace: Some(cwd),
            }) => Some(cwd),
            _ => None,
        };
        let (Some(cwd), Some(client)) = (cwd, self.host.client.clone()) else {
            Self::creation_failed(
                view,
                message,
                Self::create_failed(),
                window,
                cx,
            );
            return;
        };
        // Choices made in the draft apply before its first message runs.
        let choices = view.read(cx).draft_choices.clone();
        let view = view.clone();
        cx.spawn_in(window, async move |this, cx| {
            let started = client.start_session(&cwd, &choices).await;
            let _ = this.update_in(cx, |this, window, cx| match started {
                Ok(start) => {
                    if this.adopt_draft(&view, &cwd, start, window, cx) {
                        view.update(cx, |view, cx| {
                            view.send(message, window, cx)
                        });
                    } else {
                        let notice = Self::create_failed();
                        Self::creation_failed(
                            &view, message, notice, window, cx,
                        );
                    }
                }
                Err(error) => {
                    let notice = match &error {
                        StartError::Create(_) => Self::create_failed(),
                        StartError::Configure(error) => error.switch_notice(),
                    };
                    // Without a notice the user gave up the sign-in.
                    if notice.is_some() {
                        tracing::warn!(
                            "failed to start a session in {}: {}",
                            cwd.display(),
                            error
                        );
                    }
                    // A listing meanwhile may have shown the deleted session.
                    this.refresh_catalog(cx);
                    Self::creation_failed(&view, message, notice, window, cx);
                }
            });
        })
        .detach();
    }

    /// What to tell the user when a draft's session could not be created.
    fn create_failed() -> Option<String> {
        Some(rust_i18n::t!("Composer.create_failed").into_owned())
    }

    /// Gives the text back to the draft's composer and reports the failure,
    /// when there is something to say.
    fn creation_failed(
        view: &Entity<SessionView>,
        message: Outgoing,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let composer = view.update(cx, |view, cx| {
            view.creating = false;
            // Redraw so the workspace picker is enabled again.
            cx.notify();
            view.composer().clone()
        });
        composer
            .update(cx, |composer, cx| composer.restore(message, window, cx));
        if let Some(notice) = notice {
            window.push_notification(Notification::error(notice), cx);
        }
    }

    /// Turns the draft shown by `view` into the just-created session;
    /// returns `false` when there is no registry to adopt it into.
    fn adopt_draft(
        &mut self,
        view: &Entity<SessionView>,
        cwd: &Path,
        start: SessionStart,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(registry) = self.host.registry.clone() else {
            return false;
        };
        let id = start.id.clone();
        tracing::info!("created session {} in {}", id, cwd.display());
        let handle =
            registry.update(cx, |registry, cx| registry.adopt(start, cwd, cx));
        let now = crate::utils::now_ms();
        self.host.catalog.upsert(
            SessionSummary::builder()
                .session_id(id.clone())
                .cwd(cwd.to_path_buf())
                .created_at_ms(Some(now))
                .modified_at_ms(Some(now))
                .build(),
        );
        let entry = TabEntry::Session(id);
        // Resolve the tab through the view itself: its draft may have moved to
        // another workspace meanwhile, or closed and been replaced by a new draft.
        let old = self.view_entry(view);
        let replaced = old
            .as_ref()
            .is_some_and(|old| self.tabs.replace(old, entry.clone()));
        // The draft closed while the session was being created; keep the message visible.
        if !replaced {
            self.tabs.open(entry.clone());
        }
        if let Some(old) = &old {
            self.views.remove(old);
            // The draft's panel stays with the session it became (M3b spec §6.1).
            if let (Some(from), Some(to)) =
                (old.panel_owner(), entry.panel_owner())
            {
                self.move_panel(&from, &to);
            }
        }
        self.views.insert(entry, view.clone());
        view.update(cx, |view, cx| {
            view.creating = false;
            view.set_target(SessionTarget::Session(handle), window, cx)
        });
        self.persist(cx);
        cx.notify();
        true
    }

    /// Moves a draft to another workspace; an existing draft there is activated instead.
    fn select_draft_workspace(
        &mut self,
        view: &Entity<SessionView>,
        workspace: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(old) = self.view_entry(view) else {
            return;
        };
        let entry = TabEntry::Draft {
            workspace: Some(workspace.clone()),
        };
        if old == entry {
            return;
        }
        // Tab entries key the views, so two drafts cannot share a workspace.
        if let Some(existing) = self.tabs.position(&entry) {
            self.activate(existing, cx);
            return;
        }
        if !self.tabs.replace(&old, entry.clone()) {
            return;
        }
        self.views.remove(&old);
        self.views.insert(entry, view.clone());
        view.update(cx, |view, cx| {
            view.set_target(
                SessionTarget::Draft {
                    workspace: Some(workspace),
                },
                window,
                cx,
            )
        });
        cx.notify();
    }

    /// `FocusComposer`: focuses the active tab's message input.
    pub(in crate::ui) fn on_focus_composer(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(view) = self.active_view() {
            let composer = view.read(cx).composer().clone();
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        }
    }
}

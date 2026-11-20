//! Tab views and drafts: creating a tab's view, turning a draft into a session
//! on its first message, switching a draft's workspace, and composer focus.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Entity, Window};

use super::{AppView, SessionTarget, SessionView, SessionViewEvent, TabEntry};
use crate::actions::FocusComposer;
use crate::backend::SessionId;
use crate::catalog::SessionSummary;

impl AppView {
    /// The active tab's view, once it has been shown.
    pub fn active_view(&self) -> Option<&Entity<SessionView>> {
        self.active_entry().and_then(|entry| self.views.get(&entry))
    }

    /// Creates and registers the view of a tab; a new draft's composer takes focus.
    pub(super) fn create_view(
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
            SessionViewEvent::CreateSession { text } => {
                self.create_session(view, text.clone(), window, cx)
            }
            SessionViewEvent::SelectWorkspace(workspace) => {
                self.select_draft_workspace(view, workspace.clone(), window, cx)
            }
        }
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
        text: String,
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
            Self::creation_failed(view, &text, window, cx);
            return;
        };
        let view = view.clone();
        cx.spawn_in(window, async move |this, cx| {
            let created = client.new_session(&cwd).await;
            let _ = this.update_in(cx, |this, window, cx| match created {
                Ok(id) => {
                    if this.adopt_draft(&view, &cwd, id, window, cx) {
                        view.update(cx, |view, cx| view.send(text, window, cx));
                    } else {
                        Self::creation_failed(&view, &text, window, cx);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to create a session in {}: {}",
                        cwd.display(),
                        error
                    );
                    Self::creation_failed(&view, &text, window, cx);
                }
            });
        })
        .detach();
    }

    /// Gives the text back to the draft's composer and reports the failure.
    fn creation_failed(
        view: &Entity<SessionView>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let composer = view.update(cx, |view, cx| {
            view.creating = false;
            // Redraw so the workspace picker is enabled again.
            cx.notify();
            view.composer().clone()
        });
        composer.update(cx, |composer, cx| composer.restore(text, window, cx));
        window.push_notification(
            Notification::error(
                rust_i18n::t!("Composer.create_failed").into_owned(),
            ),
            cx,
        );
    }

    /// Turns the draft shown by `view` into the just-created session `id`;
    /// returns `false` when there is no registry to adopt it into.
    fn adopt_draft(
        &mut self,
        view: &Entity<SessionView>,
        cwd: &Path,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(registry) = self.host.registry.clone() else {
            return false;
        };
        tracing::info!("created session {} in {}", id, cwd.display());
        let handle =
            registry.update(cx, |registry, cx| registry.adopt(&id, cwd, cx));
        let now = crate::now_ms();
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
        let tab = old
            .as_ref()
            .and_then(|old| self.tabs.iter_mut().find(|tab| *tab == old));
        match tab {
            Some(tab) => *tab = entry.clone(),
            // The draft closed while the session was being created; keep the message visible.
            None => self.tabs.push(entry.clone()),
        }
        if let Some(old) = &old {
            self.views.remove(old);
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
        if let Some(existing) = self.tabs.iter().position(|tab| *tab == entry) {
            self.activate(existing, cx);
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| **tab == old) else {
            return;
        };
        *tab = entry.clone();
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
    pub(super) fn on_focus_composer(
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

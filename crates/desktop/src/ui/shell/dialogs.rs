//! Workspace and session commands: add, remove, reveal, rename, delete.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    AppContext as _, Context, Entity, ParentElement as _, PathPromptOptions,
    Window,
};

use crate::backend::SessionId;
use crate::ui::{AppView, TabEntry};

impl AppView {
    /// Recomputes which registered workspace directories are gone; filesystem reads stay out of render.
    pub(in crate::ui) fn refresh_missing(&mut self) {
        // Local host only: remote workspaces will be checked by their backend (spec §4.6).
        self.missing = self
            .state
            .workspaces
            .entries()
            .iter()
            .filter(|entry| !entry.path.is_dir())
            .map(|entry| entry.path.clone())
            .collect();
    }

    /// Records a change to the workspace list and redraws.
    fn workspaces_changed(&mut self, cx: &mut Context<'_, Self>) {
        self.refresh_missing();
        self.push_workspaces(cx);
        self.persist(cx);
        cx.notify();
    }

    /// `AddWorkspace` (Ctrl+O): asks for a folder to register.
    pub(in crate::ui) fn on_add_workspace(
        &mut self,
        _: &crate::actions::AddWorkspace,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.add_workspace(window, cx);
    }

    /// Asks for a folder and registers it as a workspace.
    pub fn add_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                rust_i18n::t!("Sidebar.add_workspace").into_owned().into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    tracing::warn!("folder picker failed: {}", error);
                    None
                }
                Err(error) => {
                    tracing::warn!(
                        "folder picker closed unexpectedly: {}",
                        error
                    );
                    None
                }
            };
            if let Some(path) = path {
                let _ = this
                    .update(cx, |this, cx| this.register_workspace(&path, cx));
            }
        })
        .detach();
    }

    /// Adds a workspace and makes it the sidebar's current one.
    pub fn register_workspace(
        &mut self,
        path: &Path,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.workspaces.add(path);
        let path = crate::workspace::normalize(path);
        self.sidebar.selected_workspace = Some(path.clone());
        self.workspaces_changed(cx);
        self.refresh_git(path, cx);
    }

    /// Registers the folders among dropped paths (spec §8); files are ignored.
    ///
    /// Returns how many were newly added; the last folder becomes the current one.
    pub fn add_dropped_paths(
        &mut self,
        paths: Vec<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) -> usize {
        let folders: Vec<PathBuf> =
            paths.into_iter().filter(|path| path.is_dir()).collect();
        let Some(last) = folders.last() else {
            return 0;
        };
        let mut added = 0;
        for folder in &folders {
            if self.state.workspaces.add(folder) {
                added += 1;
            }
        }
        self.sidebar.selected_workspace =
            Some(crate::workspace::normalize(last));
        self.workspaces_changed(cx);
        for folder in &folders {
            self.refresh_git(crate::workspace::normalize(folder), cx);
        }
        added
    }

    /// Moves a workspace one place up or down in the sidebar.
    pub fn move_workspace(
        &mut self,
        path: &Path,
        offset: isize,
        cx: &mut Context<'_, Self>,
    ) {
        if self.state.workspaces.move_entry(path, offset) {
            self.workspaces_changed(cx);
        }
    }

    /// Takes a workspace off the list; reversible, so it asks no confirmation.
    pub fn remove_workspace(
        &mut self,
        path: &Path,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.workspaces.remove(path);
        self.workspaces_changed(cx);
    }

    /// Shows a workspace directory in the system file manager.
    pub fn reveal_workspace(&self, path: &Path, cx: &mut Context<'_, Self>) {
        cx.reveal_path(path);
    }

    /// Opens the rename dialog for a session.
    pub fn rename_session(
        &mut self,
        id: &SessionId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let current = self
            .host
            .catalog
            .get(id)
            .and_then(|summary| summary.title.clone())
            .unwrap_or_default();
        let input =
            cx.new(|cx| InputState::new(window, cx).default_value(current));
        // Enter needs no subscription: the dialog binds it to OK, which commits.
        let view = cx.entity().downgrade();
        let id = id.clone();
        let field = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let id = id.clone();
            let field = field.clone();
            dialog
                .title(Label::new(
                    rust_i18n::t!("Dialog.rename_title").into_owned(),
                ))
                .child(Input::new(&field))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(
                            rust_i18n::t!("Dialog.rename_action").into_owned(),
                        )
                        .cancel_text(
                            rust_i18n::t!("Dialog.cancel").into_owned(),
                        )
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.commit_rename(&id, &field, window, cx)
                    });
                    // `commit_rename` closes the dialog itself once the title is valid.
                    false
                })
        });
        // Focus after opening, so the dialog remembers the workspace as the focus to restore.
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Sends the new title and closes the dialog; a blank title keeps the dialog open.
    fn commit_rename(
        &mut self,
        id: &SessionId,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let title = input.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        window.close_dialog(cx);
        let Some(client) = self.host.client.clone() else {
            return;
        };
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            match client.set_title(&id, &title).await {
                Ok(()) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(mut summary) =
                            this.host.catalog.get(&id).cloned()
                        {
                            summary.title = Some(title);
                            this.host.catalog.upsert(summary);
                        }
                        cx.notify();
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to rename session {}: {}",
                        id,
                        error
                    );
                    let _ = this.update_in(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(
                                rust_i18n::t!("Session.rename_failed")
                                    .into_owned(),
                            ),
                            cx,
                        )
                    });
                }
            }
        })
        .detach();
    }

    /// Asks to confirm deleting a session, naming it.
    pub fn delete_session(
        &mut self,
        id: &SessionId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let title = self
            .host
            .catalog
            .get(id)
            .map(|summary| summary.display_title())
            .unwrap_or_default();
        let view = cx.entity().downgrade();
        let id = id.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let id = id.clone();
            dialog
                .confirm()
                .title(Label::new(
                    rust_i18n::t!(
                        "Dialog.delete_title",
                        title = title.as_ref()
                    )
                    .into_owned(),
                ))
                .description(Label::new(
                    rust_i18n::t!("Dialog.delete_body").into_owned(),
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(
                            rust_i18n::t!("Dialog.delete_action").into_owned(),
                        )
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(
                            rust_i18n::t!("Dialog.cancel").into_owned(),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.confirm_delete(&id, window, cx)
                    });
                    true
                })
        });
    }

    /// Closes the session's tab, deletes it in the backend, and drops it from the list.
    fn confirm_delete(
        &mut self,
        id: &SessionId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // Forget the attached session first: closing it would race the delete
        // ("session is closing"), and deleting tears it down in the backend anyway.
        if let Some(registry) = &self.host.registry {
            registry.update(cx, |registry, _| registry.forget(id));
        }
        self.close_entry(&TabEntry::Session(id.clone()), cx);
        let Some(client) = self.host.client.clone() else {
            return;
        };
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            match client.delete_session(&id).await {
                Ok(()) => {
                    let _ = this.update(cx, |this, cx| {
                        this.host.catalog.remove(&id);
                        cx.notify();
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to delete session {}: {}",
                        id,
                        error
                    );
                    // The session never left the backend; reloading restores its row.
                    let _ = this.update_in(cx, |this, window, cx| {
                        window.push_notification(
                            Notification::error(
                                rust_i18n::t!("Session.delete_failed")
                                    .into_owned(),
                            ),
                            cx,
                        );
                        this.refresh_catalog(cx);
                    });
                }
            }
        })
        .detach();
    }
}

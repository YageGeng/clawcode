//! The composer block of a session view: sending and stopping, the queued
//! messages, the status line, the bar under the input, and the draft's
//! workspace picker.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    h_flex,
};
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, ExternalPaths,
    InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, SharedString, Styled as _, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _, rems,
};
use protocol::{QueueId, QueueKind, QueuedMessage};

use super::attachments::Outgoing;
use super::{Composer, ComposerEvent};
use crate::actions::{COMPOSER_CONTEXT, StopRun};
use crate::backend::BackendError;
use crate::projection::{CompactionStatus, RetryState, RunState};
use crate::ui::session::{SessionTarget, SessionView, SessionViewEvent};

impl SessionView {
    /// Reacts to the composer: Enter sends, Escape stops.
    pub(in crate::ui) fn on_composer_event(
        &mut self,
        _: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            ComposerEvent::Submit => self.submit(window, cx),
            ComposerEvent::Stop => self.stop(cx),
        }
    }

    /// Whether the session has an active run.
    fn running(&self, cx: &App) -> bool {
        self.handle().is_some_and(|handle| {
            handle.read(cx).projection().run_state() != RunState::Idle
        })
    }

    /// Sends the composer's text; a draft hands it to the shell to create the session first.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        // Nowhere to run yet (the send button says why), or the draft's session
        // is still being created and further text must wait in the input.
        if matches!(self.target, SessionTarget::Draft { workspace: None })
            || self.creating
        {
            return;
        }
        // Images still being read would be left behind.
        if self.composer.read(cx).reading() {
            return;
        }
        let Some(message) = self
            .composer
            .update(cx, |composer, cx| composer.take_message(window, cx))
        else {
            return;
        };
        match &self.target {
            SessionTarget::Draft { .. } => {
                self.creating = true;
                // Redraw so the workspace picker shows as disabled.
                cx.notify();
                cx.emit(SessionViewEvent::CreateSession { message })
            }
            SessionTarget::Session(_) => self.send(message, window, cx),
        }
    }

    /// Sends `text` to the session; on failure or withdrawal the text returns to the composer.
    ///
    /// A prompt sent during a run is queued by the backend (`when_busy`) and only
    /// resolves once it has run, so the request is detached and only its error matters.
    pub(in crate::ui) fn send(
        &mut self,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(handle), Some(client)) = (self.handle(), self.client.clone())
        else {
            self.composer.update(cx, |composer, cx| {
                composer.restore(message, window, cx)
            });
            return;
        };
        let id = handle.read(cx).id().clone();
        let composer = self.composer.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            // Base64 for up to 20 MiB of images is too slow for the UI thread.
            let encoding = message.clone();
            let blocks = cx
                .background_executor()
                .spawn(async move { encoding.into_blocks() })
                .await;
            let failed = match client.prompt(&id, blocks).await {
                Ok(()) => return,
                // A removed queued prompt, or one withdrawn when its run was
                // stopped, comes back as cancelled: the user's own choice, so
                // the text returns silently (WebUI parity), not as a failure.
                Err(BackendError::Cancelled) => false,
                Err(error) => {
                    tracing::warn!(
                        "failed to send a message to session {}: {}",
                        id,
                        error
                    );
                    true
                }
            };
            let _ = composer.update_in(cx, |composer, window, cx| {
                composer.restore(message, window, cx);
                if failed {
                    window.push_notification(
                        Notification::error(
                            rust_i18n::t!("Composer.send_failed").into_owned(),
                        ),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Asks for image files and attaches the chosen ones.
    fn pick_images(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        let composer = self.composer.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = composer.update_in(cx, |composer, window, cx| {
                composer.attach_paths(paths, window, cx)
            });
        })
        .detach();
    }

    /// Stops the session's run; does nothing while idle.
    fn stop(&self, cx: &mut Context<'_, Self>) {
        if !self.running(cx) {
            return;
        }
        let (Some(handle), Some(client)) = (self.handle(), &self.client) else {
            return;
        };
        let id = handle.read(cx).id();
        if let Err(error) = client.cancel(id) {
            tracing::warn!("failed to stop session {}: {}", id, error);
        }
    }

    /// Moves a queued follow-up into this turn (`promote`) or drops it from the queue.
    fn edit_queue(
        &self,
        queue_id: QueueId,
        promote: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(handle), Some(client)) = (self.handle(), self.client.clone())
        else {
            return;
        };
        let id = handle.read(cx).id().clone();
        cx.spawn_in(window, async move |_, cx| {
            let result = if promote {
                client.promote_pending(&id, &queue_id).await.map(|_| ())
            } else {
                client.remove_pending(&id, &queue_id).await
            };
            // The queue snapshot that follows a success redraws the chips.
            if let Err(error) = result {
                tracing::warn!(
                    "failed to change queued message {} of session {}: {}",
                    queue_id,
                    id,
                    error
                );
                let message = if promote {
                    rust_i18n::t!("Composer.promote_failed")
                } else {
                    rust_i18n::t!("Composer.remove_failed")
                };
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(message.into_owned()),
                        cx,
                    )
                });
            }
        })
        .detach();
    }

    /// The composer block: workspace picker (drafts), queue chips, input, and the send/stop bar.
    pub(in crate::ui) fn render_composer(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (background, border, radius, dark) = (
            theme.background,
            theme.border,
            theme.radius_lg,
            theme.is_dark(),
        );
        let draft_workspace = match &self.target {
            SessionTarget::Draft { workspace } => Some(workspace.clone()),
            SessionTarget::Session(_) => None,
        };
        let chips = self.render_chips(cx);
        let card = div()
            .w_full()
            .max_w(rems(41.25))
            .flex()
            .flex_col()
            .bg(background)
            .border_1()
            .border_color(border)
            .rounded(radius)
            .when(!dark, |this| this.shadow_sm())
            .when_some(chips, |this, chips| this.child(chips))
            .when_some(self.render_requests(cx), |this, cards| {
                this.child(cards)
            })
            .when_some(self.render_questions(cx), |this, panel| {
                this.child(panel)
            })
            .child(self.composer.clone())
            .child(self.render_bar(matches!(draft_workspace, Some(None)), cx));
        let composer = self.composer.downgrade();
        div()
            .id("composer-block")
            // Files dropped onto the composer become attachments.
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.bg(cx.theme().accent.opacity(0.3))
            })
            .on_drop(move |paths: &ExternalPaths, window, cx| {
                let paths = paths.paths().to_vec();
                let _ = composer.update(cx, |composer, cx| {
                    composer.attach_paths(paths, window, cx)
                });
            })
            .flex_none()
            .mx_7()
            .mt_3()
            .mb_4()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            // A draft can always move to another registered workspace, including
            // one added after the draft opened without any.
            .when_some(
                draft_workspace.filter(|_| !self.workspaces.is_empty()),
                |this, workspace| {
                    this.child(
                        self.render_workspace_picker(workspace.as_deref(), cx),
                    )
                },
            )
            .when_some(self.render_plan(cx), |this, plan| this.child(plan))
            .when_some(self.render_status_line(cx), |this, line| {
                this.child(line)
            })
            .child(card)
            .into_any_element()
    }

    /// One line above the composer for a retry or compaction in progress.
    fn render_status_line(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let handle = self.handle()?.read(cx);
        let controls = handle.projection().controls();
        let theme = cx.theme();
        let (text, danger, busy) =
            match (controls.retry(), controls.compaction()) {
                (
                    Some(RetryState::Waiting {
                        attempt,
                        max,
                        due_ms,
                        ..
                    }),
                    _,
                ) => {
                    let seconds =
                        due_ms.saturating_sub(crate::now_ms()).div_ceil(1000);
                    (
                        rust_i18n::t!(
                            "Status.retry_waiting",
                            attempt = attempt,
                            max = max,
                            seconds = seconds
                        ),
                        false,
                        true,
                    )
                }
                (Some(RetryState::Running { attempt, max }), _) => (
                    rust_i18n::t!(
                        "Status.retry_running",
                        attempt = attempt,
                        max = max
                    ),
                    false,
                    true,
                ),
                (Some(RetryState::Failed { .. }), _) => {
                    (rust_i18n::t!("Status.retry_failed"), true, false)
                }
                (None, Some(CompactionStatus::Running)) => {
                    (rust_i18n::t!("Status.compacting"), false, true)
                }
                (None, Some(CompactionStatus::Failed(error))) => (
                    rust_i18n::t!("Status.compaction_failed", error = error),
                    true,
                    false,
                ),
                (None, Some(CompactionStatus::Cancelled)) => {
                    (rust_i18n::t!("Status.compaction_cancelled"), false, false)
                }
                (None, None) => return None,
            };
        let color = if danger {
            theme.danger
        } else {
            theme.muted_foreground
        };
        Some(
            h_flex()
                .id("composer-status")
                .test_support()
                .w_full()
                .max_w(rems(41.25))
                .gap_1p5()
                .px_1()
                .text_sm()
                .text_color(color)
                .when(busy, |this| this.child(Spinner::new().small()))
                .child(
                    Label::new(text.into_owned()).text_sm().text_color(color),
                )
                .into_any_element(),
        )
    }

    /// Bottom bar: the missing-workspace hint, then the send or stop button.
    fn render_bar(
        &self,
        needs_workspace: bool,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let button = if self.running(cx) {
            Button::new("composer-stop")
                .primary()
                .small()
                .rounded_full()
                .icon(IconName::Square)
                .tooltip_with_action(
                    rust_i18n::t!("Composer.stop").into_owned(),
                    &StopRun,
                    Some(COMPOSER_CONTEXT),
                )
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)),
                )
        } else {
            Button::new("composer-send")
                .primary()
                .small()
                .rounded_full()
                .icon(IconName::ArrowUp)
                .tooltip(rust_i18n::t!("Composer.send").into_owned())
                .disabled(needs_workspace)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.submit(window, cx)
                }))
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .pt_1p5()
            .pb_2()
            // With workspaces registered, the picker above asks for one instead.
            .when(needs_workspace && self.workspaces.is_empty(), |this| {
                this.child(
                    Label::new(
                        rust_i18n::t!("Composer.need_workspace").into_owned(),
                    )
                    .text_xs()
                    .text_color(muted),
                )
            })
            .child(
                Button::new("composer-attach")
                    .ghost()
                    .small()
                    .icon(gpui_kit::assets::IconName::Paperclip)
                    .tooltip(rust_i18n::t!("Attachment.add").into_owned())
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            this.pick_images(window, cx)
                        },
                    )),
            )
            .child(self.render_controls_left(cx))
            // The right controls take the room left and clip in a narrow
            // composer (a session in the right panel); the send button stays.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .overflow_hidden()
                    .children(self.render_controls_right(cx)),
            )
            .child(div().flex_none().child(button))
    }

    /// Chips of the queued messages, follow-ups first; `None` when nothing is queued.
    fn render_chips(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let handle = self.handle()?;
        let pending = handle.read(cx).projection().pending();
        if pending.follow_up.is_empty() && pending.steering.is_empty() {
            return None;
        }
        let queued: Vec<QueuedMessage> = pending
            .follow_up
            .iter()
            .chain(&pending.steering)
            .cloned()
            .collect();
        let chips: Vec<AnyElement> = queued
            .into_iter()
            .map(|message| self.render_chip(message, cx))
            .collect();
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .px_2()
                .pt_2()
                .children(chips)
                .into_any_element(),
        )
    }

    /// One queued message: kind, first line, and its promote and remove buttons.
    fn render_chip(
        &self,
        message: QueuedMessage,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, muted_foreground, radius) =
            (theme.muted, theme.muted_foreground, theme.radius);
        let kind = match message.kind {
            QueueKind::FollowUp => rust_i18n::t!("Composer.follow_up"),
            QueueKind::Steering => rust_i18n::t!("Composer.steering"),
        };
        let text = message.message.default_title().unwrap_or_default();
        let queue_id = message.queue_id;
        let promote_id = queue_id.clone();
        let remove_id = queue_id.clone();
        div()
            .id(SharedString::from(format!("chip:{queue_id}")))
            .test_support()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .max_w_full()
            .h(rems(1.625))
            .pl_2()
            .pr_0p5()
            .rounded(radius)
            .bg(muted)
            .text_xs()
            .child(Label::new(kind.into_owned()).text_color(muted_foreground))
            .child(Label::new(text).truncate().max_w(rems(20.)))
            .when(message.kind == QueueKind::FollowUp, |this| {
                this.child(
                    Button::new(SharedString::from(format!(
                        "promote:{queue_id}"
                    )))
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowUp)
                    .tooltip(rust_i18n::t!("Composer.promote").into_owned())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.edit_queue(
                                promote_id.clone(),
                                true,
                                window,
                                cx,
                            )
                        },
                    )),
                )
            })
            .child(
                Button::new(SharedString::from(format!("remove:{queue_id}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip(rust_i18n::t!("Composer.remove").into_owned())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.edit_queue(
                                remove_id.clone(),
                                false,
                                window,
                                cx,
                            )
                        },
                    )),
            )
            .into_any_element()
    }

    /// The draft's workspace: folder name and muted path, or a prompt to choose
    /// one; clicking lists every registered workspace.
    fn render_workspace_picker(
        &self,
        workspace: Option<&std::path::Path>,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let choices = self.workspaces.clone();
        let current = workspace.map(std::path::Path::to_path_buf);
        let label = workspace.map_or_else(
            || rust_i18n::t!("Composer.choose_workspace").into_owned(),
            crate::workspace::display_name,
        );
        div()
            .w_full()
            .max_w(rems(41.25))
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .child(
                Button::new("composer-workspace")
                    .ghost()
                    .small()
                    .icon(IconName::Folder)
                    .label(label)
                    // The session is being created in the chosen workspace.
                    .disabled(self.creating)
                    .dropdown_menu(move |menu, _, _| {
                        choices.iter().fold(menu, |menu, path| {
                            let view = view.clone();
                            let picked: PathBuf = path.clone();
                            menu.item(
                                PopupMenuItem::new(crate::workspace::display_name(path))
                                    .checked(current.as_ref() == Some(path))
                                    .on_click(move |_, _, cx| {
                                        let picked = picked.clone();
                                        let _ = view.update(cx, |_, cx| {
                                            cx.emit(
                                                SessionViewEvent::SelectWorkspace(
                                                    picked,
                                                ),
                                            )
                                        });
                                    }),
                            )
                        })
                    }),
            )
            .when_some(workspace, |this, workspace| {
                this.child(
                    Label::new(workspace.display().to_string())
                        .text_xs()
                        .text_color(muted)
                        .truncate(),
                )
            })
    }
}

//! The composer under the transcript: the message input, the queued-message
//! chips, the draft's workspace picker, and the send/stop button.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity,
    EntityInputHandler as _, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, TestSupportExt as _, Window, div,
    prelude::FluentBuilder as _, rems,
};
use protocol::{QueueId, QueueKind, QueuedMessage};
use typed_builder::TypedBuilder;

use super::session_view::{SessionTarget, SessionView, SessionViewEvent};
use crate::actions::{COMPOSER_CONTEXT, StopRun};
use crate::backend::BackendError;
use crate::i18n::LocaleGeneration;
use crate::projection::RunState;

/// What the composer asks its session view to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Enter was pressed outside an IME composition.
    Submit,
    /// Escape reached the composer with nothing left to dismiss.
    Stop,
}

/// The message input of one tab.
#[derive(TypedBuilder)]
pub struct Composer {
    input: Entity<TextareaState>,
    /// Whether the tab is a draft, which picks the placeholder.
    #[builder(default)]
    draft: bool,
    #[builder(default)]
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    /// Creates an auto-growing input where Enter submits and Shift+Enter inserts a newline.
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
        });
        let enter = cx.subscribe_in(
            &input,
            window,
            |_, input, event: &InputEvent, window, cx| {
                let InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } = event
                else {
                    return;
                };
                // Enter during an IME composition only confirms the candidate.
                let composing = input.update(cx, |input, cx| {
                    let composing =
                        input.marked_text_range(window, cx).is_some();
                    if composing {
                        input.unmark_text(window, cx);
                    }
                    composing
                });
                if !composing {
                    cx.emit(ComposerEvent::Submit);
                }
            },
        );
        // The placeholder is cached in the input, so re-read it after a language switch.
        let locale = cx.observe_global_in::<LocaleGeneration>(
            window,
            |this: &mut Self, window, cx| this.sync_placeholder(window, cx),
        );
        let mut composer = Composer::builder()
            .input(input)
            ._subscriptions(vec![enter, locale])
            .build();
        composer.sync_placeholder(window, cx);
        composer
    }

    /// Shows the draft or the session placeholder in the current language.
    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut App) {
        let placeholder = if self.draft {
            rust_i18n::t!("Composer.placeholder_new")
        } else {
            rust_i18n::t!("Composer.placeholder")
        };
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder.into_owned(), window, cx)
        });
    }

    /// The text input.
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    /// Switches between the draft and the session placeholder.
    pub fn set_draft(
        &mut self,
        draft: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.draft != draft {
            self.draft = draft;
            self.sync_placeholder(window, cx);
            cx.notify();
        }
    }

    /// Takes the text out of the input exactly as typed and clears it; blank
    /// text stays and yields `None`.
    pub fn take_text(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<String> {
        let text = self.input.read(cx).value().to_string();
        // Trimming only decides emptiness; indentation and trailing blank
        // lines are part of the message.
        if text.trim().is_empty() {
            return None;
        }
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        Some(text)
    }

    /// Puts unsent text back; anything typed since stays below it.
    pub fn restore(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.input.update(cx, |input, cx| {
            let current = input.value();
            let restored = if current.trim().is_empty() {
                text.to_string()
            } else {
                format!("{text}\n{current}")
            };
            input.set_value(restored, window, cx);
        });
    }

    /// Moves keyboard focus into the input.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Render for Composer {
    /// Draws the frameless input inside the composer card.
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .id("composer-input")
            .test_support()
            .key_context(COMPOSER_CONTEXT)
            .on_action(
                cx.listener(|_, _: &StopRun, _, cx| {
                    cx.emit(ComposerEvent::Stop)
                }),
            )
            .px_2()
            .pt_1p5()
            .child(Textarea::new(&self.input).appearance(false))
    }
}

impl SessionView {
    /// Reacts to the composer: Enter sends, Escape stops.
    pub(super) fn on_composer_event(
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
        let Some(text) = self
            .composer
            .update(cx, |composer, cx| composer.take_text(window, cx))
        else {
            return;
        };
        match &self.target {
            SessionTarget::Draft { .. } => {
                self.creating = true;
                // Redraw so the workspace picker shows as disabled.
                cx.notify();
                cx.emit(SessionViewEvent::CreateSession { text })
            }
            SessionTarget::Session(_) => self.send(text, window, cx),
        }
    }

    /// Sends `text` to the session; on failure or withdrawal the text returns to the composer.
    ///
    /// A prompt sent during a run is queued by the backend (`when_busy`) and only
    /// resolves once it has run, so the request is detached and only its error matters.
    pub(super) fn send(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(handle), Some(client)) = (self.handle(), self.client.clone())
        else {
            self.composer
                .update(cx, |composer, cx| composer.restore(&text, window, cx));
            return;
        };
        let id = handle.read(cx).id().clone();
        let composer = self.composer.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let failed =
                match client.prompt(&id, vec![text.clone().into()]).await {
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
                composer.restore(&text, window, cx);
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
    pub(super) fn render_composer(
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
            .child(self.composer.clone())
            .child(self.render_bar(matches!(draft_workspace, Some(None)), cx));
        div()
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
            .child(card)
            .into_any_element()
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
            .child(div().flex_1())
            .child(button)
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

//! The composer under the transcript: the message input, the queued-message
//! chips, the draft's workspace picker, and the send/stop button.

pub(in crate::ui) mod attachments;
mod block;
pub(in crate::ui) mod command_palette;
pub(in crate::ui) mod controls;

use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;

use gpui_kit::component::attachment::{Attachment, AttachmentMedia};
use gpui_kit::component::input::{InputEvent, Paste, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardEntry, Context,
    Entity, EntityInputHandler as _, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, TestSupportExt as _, Window, div,
    prelude::FluentBuilder as _,
};
use typed_builder::TypedBuilder;

use crate::actions::{
    COMPOSER_CONTEXT, PaletteAccept, PaletteDismiss, PaletteDown, PaletteUp,
    StopRun,
};
use crate::i18n::LocaleGeneration;
use attachments::{AttachmentError, AttachmentSet, Outgoing};

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
    /// The slash command list.
    #[builder(default)]
    pub(super) palette: command_palette::Palette,
    /// Images to send with the next message.
    #[builder(default)]
    attachments: AttachmentSet,
    /// Files still being read for attaching.
    #[builder(default)]
    reading: usize,
    /// The earlier message being edited (M4b spec §3.3), and what the
    /// composer held before editing began, restored on cancel.
    #[builder(default)]
    editing: Option<(wire::MessageId, Outgoing)>,
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
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_palette_edit(cx);
                    return;
                }
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

    /// Starts editing the earlier message `message_id`: the composer shows
    /// `message`. Its own content is kept to restore on cancel; editing
    /// another message keeps what was kept before the first edit.
    pub fn begin_edit(
        &mut self,
        message_id: wire::MessageId,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let saved = match self.editing.take() {
            Some((_, saved)) => saved,
            None => Outgoing {
                text: self.input.read(cx).value().to_string(),
                images: std::mem::take(&mut self.attachments),
            },
        };
        self.show(message, window, cx);
        self.editing = Some((message_id, saved));
        cx.notify();
    }

    /// Stops editing and puts back what the composer held before.
    pub fn cancel_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some((_, saved)) = self.editing.take() {
            self.show(saved, window, cx);
            cx.notify();
        }
    }

    /// The earlier message being edited, if any.
    pub fn editing(&self) -> Option<&wire::MessageId> {
        self.editing.as_ref().map(|(id, _)| id)
    }

    /// Ends editing once the edit is on its way; returns the content kept
    /// from before, which comes back if the send fails.
    pub fn finish_edit(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) -> Option<Outgoing> {
        cx.notify();
        self.editing.take().map(|(_, kept)| kept)
    }

    /// Replaces the input's text and the attached images with `message`.
    fn show(
        &mut self,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.input
            .update(cx, |input, cx| input.set_value(message.text, window, cx));
        self.attachments = message.images;
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

    /// Takes the message out of the composer: the text exactly as typed and
    /// the images. Blank text without images stays and yields `None`.
    pub fn take_message(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Outgoing> {
        let text = self.input.read(cx).value().to_string();
        // Trimming only decides emptiness; indentation and trailing blank
        // lines are part of the message.
        if text.trim().is_empty() && self.attachments.is_empty() {
            return None;
        }
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let text = if text.trim().is_empty() {
            String::new()
        } else {
            text
        };
        cx.notify();
        Some(Outgoing {
            text,
            images: std::mem::take(&mut self.attachments),
        })
    }

    /// Puts an unsent message back; text typed since stays below it, images added since stay after it.
    pub fn restore(
        &mut self,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let text = message.text;
        if !text.is_empty() {
            self.input.update(cx, |input, cx| {
                let current = input.value();
                let restored = if current.trim().is_empty() {
                    text.clone()
                } else {
                    format!("{text}\n{current}")
                };
                input.set_value(restored, window, cx);
            });
        }
        let added = std::mem::replace(&mut self.attachments, message.images);
        // Images attached since go after the returned ones; any that no longer fit are reported.
        let mut dropped = None;
        for item in added.items() {
            if let Err(error) = self
                .attachments
                .add(item.name.clone(), item.image.bytes().to_vec())
            {
                dropped = Some(error);
            }
        }
        if let Some(error) = dropped {
            self.report_attachment_error(error, window, cx);
        }
        cx.notify();
    }

    /// Attaches an image, or reports why it cannot be attached.
    pub fn add_attachment(
        &mut self,
        name: String,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Err(error) = self.attachments.add(name, bytes) {
            self.report_attachment_error(error, window, cx);
        }
        cx.notify();
    }

    /// Shows why an image could not be attached.
    fn report_attachment_error(
        &self,
        error: AttachmentError,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let message = match error {
            AttachmentError::UnsupportedFormat => {
                rust_i18n::t!("Attachment.unsupported")
            }
            AttachmentError::TooLarge => {
                rust_i18n::t!("Attachment.too_large")
            }
            AttachmentError::TooMany => {
                rust_i18n::t!("Attachment.too_many")
            }
            AttachmentError::TotalTooLarge => {
                rust_i18n::t!("Attachment.total_too_large")
            }
        };
        window
            .push_notification(Notification::warning(message.into_owned()), cx);
        cx.notify();
    }

    /// Reads files off the UI thread and attaches them; unreadable files are reported.
    pub fn attach_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.reading += 1;
        cx.notify();
        // The size limits are checked from the file length, so a huge file is never read.
        let limits = self.attachments.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            let bytes =
                                std::fs::metadata(&path).and_then(|meta| {
                                    match limits.admits(meta.len()) {
                                        Ok(()) => std::fs::read(&path).map(Ok),
                                        Err(rejected) => Ok(Err(rejected)),
                                    }
                                });
                            (name, bytes)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reading = this.reading.saturating_sub(1);
                for (name, bytes) in read {
                    match bytes {
                        Ok(Ok(bytes)) => {
                            this.add_attachment(name, bytes, window, cx)
                        }
                        Ok(Err(rejected)) => {
                            this.report_attachment_error(rejected, window, cx)
                        }
                        Err(error) => {
                            tracing::warn!(
                                "failed to read attachment {}: {}",
                                name,
                                error
                            );
                            window.push_notification(
                                Notification::warning(
                                    rust_i18n::t!("Attachment.read_failed")
                                        .into_owned(),
                                ),
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether files are still being read; sending waits for them.
    pub fn reading(&self) -> bool {
        self.reading > 0
    }

    /// Attaches the clipboard's images; text in the clipboard still pastes into the input.
    fn paste_images(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut found = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    found = true;
                    let name =
                        format!("clipboard-image.{}", image.format.extension());
                    self.add_attachment(name, image.bytes.clone(), window, cx);
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    found = true;
                    self.attach_paths(paths.paths().to_vec(), window, cx);
                }
                _ => {}
            }
        }
        // Only an image-only clipboard is ours alone; text goes on to the input.
        if found && item.text().is_none() {
            cx.stop_propagation();
        } else {
            cx.propagate();
        }
    }

    /// Chips of the attached images, each with a remove button.
    fn render_attachments(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        let chips: Vec<AnyElement> = self
            .attachments
            .items()
            .iter()
            .enumerate()
            .map(|(n, item)| {
                let image = Arc::clone(&item.image);
                div()
                    .id(SharedString::from(format!("attachment-{n}")))
                    .test_support()
                    .child(
                        Attachment::new()
                            .id(SharedString::from(format!(
                                "attachment-remove-{n}"
                            )))
                            .media(AttachmentMedia::new().src(image))
                            .tooltip(item.name.clone())
                            .on_remove(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.attachments.remove(n);
                                    cx.notify();
                                },
                            )),
                    )
                    .into_any_element()
            })
            .collect();
        Some(
            h_flex()
                .gap_2()
                .px_2()
                .pt_2()
                .children(chips)
                .into_any_element(),
        )
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
        let palette = self.render_palette(cx);
        let context = match &palette {
            Some((_, true)) => "Composer CommandPalette CommandMatches",
            Some((_, false)) => "Composer CommandPalette",
            None => COMPOSER_CONTEXT,
        };
        div()
            .id("composer-input")
            .test_support()
            .key_context(context)
            .on_action(cx.listener(|this, _: &PaletteUp, _, cx| {
                this.palette_move(false, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteDown, _, cx| {
                this.palette_move(true, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteAccept, window, cx| {
                this.palette_accept(window, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteDismiss, _, cx| {
                this.palette_dismiss(cx)
            }))
            .on_action(
                cx.listener(|_, _: &StopRun, _, cx| {
                    cx.emit(ComposerEvent::Stop)
                }),
            )
            .when_some(palette, |this, (palette, _)| this.child(palette))
            // Clipboard images become attachments before the input pastes text.
            .capture_action(cx.listener(|this, _: &Paste, window, cx| {
                this.paste_images(window, cx)
            }))
            .when_some(self.render_attachments(cx), |this, chips| {
                this.child(chips)
            })
            .px_2()
            .pt_1p5()
            .child(Textarea::new(&self.input).appearance(false))
    }
}

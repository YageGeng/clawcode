//! The dialog a provider sign-in shows its device code in (M4c spec §4.3).

use agent_client_protocol::v2::schema as wire;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::label::Label;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, Context, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window, div, rems,
};

use crate::backend::{RequestId, SignInPrompt};
use crate::ui::AppView;

/// The device codes waiting for the user and the dialog showing them.
#[derive(Default)]
pub struct SignInHost {
    /// Waiting codes, oldest first; the dialog shows the first.
    prompts: Vec<(RequestId, SignInPrompt)>,
    /// Whether the dialog is open.
    open: bool,
}

impl AppView {
    /// Queues a sign-in's device code for the dialog.
    pub(in crate::ui) fn queue_sign_in(
        &mut self,
        id: RequestId,
        prompt: SignInPrompt,
        cx: &mut Context<'_, Self>,
    ) {
        self.sign_in.prompts.push((id, prompt));
        cx.notify();
    }

    /// Drops a code the agent withdrew: its sign-in ended.
    pub(in crate::ui) fn withdraw_sign_in(
        &mut self,
        id: RequestId,
        cx: &mut Context<'_, Self>,
    ) {
        self.sign_in.prompts.retain(|(waiting, _)| *waiting != id);
        cx.notify();
    }

    /// Drops every code: nobody is left to answer once the backend stopped.
    pub(in crate::ui) fn clear_sign_ins(&mut self) {
        self.sign_in.prompts.clear();
    }

    /// Opens the dialog for a waiting code and closes it after the last one.
    /// Codes arrive without a window; the frame that has one runs this.
    pub(in crate::ui) fn sync_sign_in(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let wanted = !self.sign_in.prompts.is_empty();
        // Only the topmost dialog can be closed or opened over; the quit
        // confirmation goes first.
        if wanted == self.sign_in.open || self.confirming_quit {
            return;
        }
        self.sign_in.open = wanted;
        cx.defer_in(window, move |this, window, cx| {
            if wanted {
                this.open_sign_in(window, cx);
            } else {
                window.close_dialog(cx);
            }
        });
    }

    /// Gives up the sign-in on screen; the next waiting one shows instead.
    fn dismiss_sign_in(&mut self, cx: &mut Context<'_, Self>) {
        self.sign_in.open = false;
        if !self.sign_in.prompts.is_empty() {
            let (id, _) = self.sign_in.prompts.remove(0);
            if let Some(client) = &self.host.client
                && let Err(error) = client.answer_elicitation(
                    id,
                    wire::CreateElicitationResponse::new(
                        wire::ElicitationAction::Cancel,
                    ),
                )
            {
                tracing::warn!("failed to cancel a sign-in: {}", error);
            }
        }
        cx.notify();
    }

    /// Opens the dialog over whatever is on screen, such as the settings.
    fn open_sign_in(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let prompt = view.upgrade().and_then(|view| {
                view.read(cx).sign_in.prompts.first().cloned()
            });
            let dismissed = view.clone();
            let dialog = dialog
                .overlay_closable(false)
                // Enter must neither open a page nor give up.
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = dismissed
                        .update(cx, |this, cx| this.dismiss_sign_in(cx));
                });
            // The last code was withdrawn and the dialog is about to close.
            let Some((_, prompt)) = prompt else {
                return dialog;
            };
            let page = prompt.page().map(str::to_owned);
            dialog
                .title(Label::new(
                    rust_i18n::t!(
                        "SignIn.title",
                        provider = prompt.provider.as_str()
                    )
                    .into_owned(),
                ))
                .child(prompt.render(cx))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("sign-in-cancel")
                                .outline()
                                .label(
                                    rust_i18n::t!("SignIn.cancel").into_owned(),
                                )
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ = view.update(cx, |this, cx| {
                                            this.dismiss_sign_in(cx)
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new("sign-in-open")
                                .primary()
                                .label(
                                    rust_i18n::t!("SignIn.open").into_owned(),
                                )
                                .disabled(page.is_none())
                                // The code is still needed once the page is
                                // open, so this answers nothing.
                                .on_click(move |_, _, cx| {
                                    if let Some(page) = &page {
                                        cx.open_url(page);
                                    }
                                }),
                        ),
                )
        });
    }
}

impl SignInPrompt {
    /// The instruction, the page's address, the code, and that the sign-in
    /// is waiting for it.
    fn render(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let code = self.device.user_code.clone();
        v_flex()
            .gap_3()
            .child(
                Label::new(rust_i18n::t!("SignIn.instruction").into_owned())
                    .text_sm(),
            )
            .child(
                div().text_xs().text_color(theme.muted_foreground).child(
                    TextView::markdown(
                        "sign-in-page",
                        SharedString::from(format!(
                            "`{}`",
                            self.device.verification_uri
                        )),
                    )
                    .selectable(true),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_center()
                    .py_2()
                    .child(
                        Label::new(code.clone())
                            .text_xl()
                            .font_family(theme.mono_font_family.clone()),
                    )
                    .child(
                        Clipboard::new("sign-in-copy")
                            .value(SharedString::from(code)),
                    ),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .min_h(rems(1.25))
                    .child(Spinner::new().small())
                    .child(
                        Label::new(
                            rust_i18n::t!("SignIn.waiting").into_owned(),
                        )
                        .text_sm()
                        .text_color(theme.muted_foreground),
                    ),
            )
            .into_any_element()
    }
}

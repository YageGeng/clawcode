//! The Provider page: signing in to the providers that need it.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::{
    AnyElement, App, Context, IntoElement as _, ParentElement as _,
    Styled as _, Task, WeakEntity, Window,
};
use protocol::{LoginMethod, SignInStatus};

use crate::backend::SignInOutcome;
use crate::ui::AppView;

/// What the Provider page knows about the sign-ins.
#[derive(Default)]
pub struct ProviderStatus {
    /// Status of each method as last read; a method without one is unknown.
    statuses: HashMap<String, SignInStatus>,
    /// The read in flight; a newer one replaces it.
    reading: Option<Task<()>>,
    /// Methods being signed in to from this page.
    signing_in: HashSet<String>,
}

/// What one method's row shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    /// The status is being read.
    Checking,
    /// A sign-in from this page is under way.
    SigningIn,
    /// The last status read.
    Known(SignInStatus),
}

impl AppView {
    /// Reads the sign-in statuses again; the check may wait on the network.
    pub(in crate::ui) fn read_sign_in_status(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.host.client.clone() else {
            return;
        };
        // Nothing to sign in to, so nothing to check.
        if client.login_methods().is_empty() {
            return;
        }
        self.settings.providers.reading =
            Some(cx.spawn(async move |this, cx| {
                let read = client.auth_status().await;
                let _ = this.update(cx, |this, cx| {
                    let providers = &mut this.settings.providers;
                    providers.reading = None;
                    providers.statuses = match read {
                        Ok(statuses) => statuses
                            .into_iter()
                            .map(|entry| (entry.method_id, entry.status))
                            .collect(),
                        Err(error) => {
                            tracing::warn!(
                                "failed to read the sign-in status: {}",
                                error
                            );
                            HashMap::new()
                        }
                    };
                    cx.notify();
                });
            }));
        cx.notify();
    }

    /// Signs in through `method`, then reads the statuses again.
    fn sign_in_from_settings(
        &mut self,
        method: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.host.client.clone() else {
            return;
        };
        let method = method.to_owned();
        if !self.settings.providers.signing_in.insert(method.clone()) {
            return;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = client.sign_in(&method).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.settings.providers.signing_in.remove(&method);
                if outcome == SignInOutcome::Failed {
                    window.push_notification(
                        Notification::error(
                            rust_i18n::t!("SignIn.failed").into_owned(),
                        ),
                        cx,
                    );
                }
                this.read_sign_in_status(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The Provider page: one row per sign-in, then where API keys are set.
    pub(super) fn providers_page(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let view = cx.entity().downgrade();
        let client = self
            .host
            .client
            .as_ref()
            .filter(|_| !self.backend_stopped());
        let group = match client {
            Some(client) => SettingGroup::new().items(
                client.login_methods().iter().map(|method| {
                    self.settings.providers.row(&method.id).item(method, &view)
                }),
            ),
            None => {
                SettingGroup::new().item(SettingItem::render(|_, _, cx| {
                    Label::new(
                        rust_i18n::t!("Settings.backend_not_running")
                            .into_owned(),
                    )
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                }))
            }
        };
        SettingPage::new(rust_i18n::t!("Settings.providers").into_owned())
            .group(group)
            .group(
                SettingGroup::new().item(self.config_row().into()).footer(
                    |_, _| rust_i18n::t!("Settings.api_keys").into_owned(),
                ),
            )
    }
}

impl ProviderStatus {
    /// What the row of `method` shows.
    fn row(&self, method: &str) -> RowState {
        if self.signing_in.contains(method) {
            RowState::SigningIn
        } else if self.reading.is_some() {
            RowState::Checking
        } else {
            let status = self.statuses.get(method).copied();
            RowState::Known(status.unwrap_or(SignInStatus::Unknown))
        }
    }
}

impl RowState {
    /// The provider's name over its status, with what can be done on the right.
    fn item(
        self,
        method: &LoginMethod,
        view: &WeakEntity<AppView>,
    ) -> SettingItem {
        let status = match self {
            Self::Checking => rust_i18n::t!("Settings.sign_in_checking"),
            Self::SigningIn => rust_i18n::t!("SignIn.waiting"),
            Self::Known(SignInStatus::SignedIn) => {
                rust_i18n::t!("Settings.signed_in")
            }
            Self::Known(SignInStatus::SignInRequired) => {
                rust_i18n::t!("Settings.signed_out")
            }
            Self::Known(SignInStatus::Unknown) => {
                rust_i18n::t!("Settings.sign_in_unknown")
            }
        };
        let id = method.id.clone();
        let view = view.clone();
        SettingItem::new(
            method.name.clone(),
            SettingField::render(move |_, _, cx| self.actions(&id, &view, cx)),
        )
        .description(status.into_owned())
    }

    /// The status's icon and what can be done about it.
    fn actions(
        self,
        method: &str,
        view: &WeakEntity<AppView>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let t = |key: &str| rust_i18n::t!(key).into_owned();
        let icon = |icon, color| Icon::new(icon).small().text_color(color);
        let sign_in = |label: &str| {
            let view = view.clone();
            let method = method.to_owned();
            Button::new("sign-in").small().label(t(label)).on_click(
                move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.sign_in_from_settings(&method, window, cx)
                    });
                },
            )
        };
        let row = h_flex().gap_2();
        match self {
            Self::Checking => row.child(Spinner::new().small()),
            Self::SigningIn => {
                let view = view.clone();
                let method = method.to_owned();
                row.child(Spinner::new().small()).child(
                    Button::new("cancel")
                        .ghost()
                        .small()
                        .label(t("SignIn.cancel"))
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, _| {
                                if let Some(client) = &this.host.client {
                                    client.cancel_sign_in(&method);
                                }
                            });
                        }),
                )
            }
            Self::Known(SignInStatus::SignedIn) => row
                .child(icon(LucideIcon::CircleCheck, theme.success))
                .child(sign_in("Settings.sign_in_again").outline()),
            Self::Known(SignInStatus::SignInRequired) => row
                .child(icon(LucideIcon::TriangleAlert, theme.warning))
                .child(sign_in("Settings.sign_in").primary()),
            Self::Known(SignInStatus::Unknown) => {
                let view = view.clone();
                row.child(icon(
                    LucideIcon::CircleQuestionMark,
                    theme.muted_foreground,
                ))
                .child(
                    Button::new("retry")
                        .ghost()
                        .small()
                        .label(t("Settings.retry"))
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.read_sign_in_status(cx)
                            });
                        }),
                )
                .child(sign_in("Settings.sign_in").outline())
            }
        }
        .into_any_element()
    }
}

//! Composer bar controls: the plan-mode chip, the context ring, and the
//! model and thinking-level chip (spec §7.4).

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Side,
    Sizable as _, Theme, WindowExt as _, v_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, Hsla, IntoElement, ParentElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, rems,
};

use crate::backend::BackendError;
use crate::projection::{ConfigOption, ContextUsage};
use crate::ui::format::TokenCount;
use crate::ui::session::{SessionTarget, SessionView};

/// Height of a composer chip: the M1b queue chip's height (plan Global Constraints).
const CHIP_HEIGHT: f32 = 1.625;

/// Context fill at which the ring turns to the warning color, in tenths of a percent.
const WARNING_TENTHS: u64 = 850;

/// Text and icon color of a composer chip (mockup `.chip`).
///
/// gpui-kit `Label` paints `foreground` unless it is given a color, so the
/// chip states are spelled out: muted at rest, `foreground` when selected,
/// and the button's own disabled tone when disabled.
fn chip_tone(theme: &Theme, disabled: bool, selected: bool) -> Hsla {
    if disabled {
        theme.muted_foreground.opacity(0.5)
    } else if selected {
        theme.foreground
    } else {
        theme.muted_foreground
    }
}

impl BackendError {
    /// What to tell the user when changing an option failed; nothing when
    /// they gave up the sign-in it needed.
    pub(in crate::ui) fn switch_notice(&self) -> Option<String> {
        let notice = match self {
            BackendError::Cancelled => return None,
            BackendError::SignInFailed => rust_i18n::t!("SignIn.failed"),
            _ => rust_i18n::t!("Controls.switch_failed"),
        };
        Some(notice.into_owned())
    }
}

impl SessionView {
    /// Options of the attached session, or for a draft, the options a new
    /// session starts with, showing the choices made in the draft.
    fn option(&self, id: &str, cx: &Context<'_, Self>) -> Option<ConfigOption> {
        let Some(handle) = self.handle() else {
            let mut option = self
                .draft_config
                .iter()
                .find(|option| option.id == id)
                .cloned()?;
            if let Some(choice) = self.draft_choices.get(id) {
                option.current.clone_from(choice);
            }
            return Some(option);
        };
        handle.read(cx).projection().controls().config(id).cloned()
    }

    /// Changes one configuration option; the agent's answer replaces the list.
    ///
    /// Nothing changes locally until the agent confirms, so a failure leaves
    /// the current values in place; it only shows a notification.
    pub fn set_config(
        &mut self,
        config_id: &'static str,
        value: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A draft has no session yet: the choice waits until it is created.
        if matches!(self.target, SessionTarget::Draft { .. }) {
            self.draft_choices.insert(config_id, value);
            cx.notify();
            return;
        }
        let (Some(handle), Some(client)) =
            (self.handle().cloned(), self.client.clone())
        else {
            return;
        };
        let id = handle.read(cx).id().clone();
        self.config_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result =
                client.set_config_signing_in(&id, config_id, &value).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.config_busy = false;
                match result {
                    Ok(options) => handle.update(cx, |handle, cx| {
                        handle.controls_mut().set_config(options);
                        cx.notify();
                    }),
                    Err(error) => {
                        if let Some(notice) = error.switch_notice() {
                            tracing::warn!(
                                "failed to set {} of session {}: {}",
                                config_id,
                                id,
                                error
                            );
                            window.push_notification(
                                Notification::error(notice),
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

    /// The bar's left controls: the plan-mode chip.
    pub(in crate::ui) fn render_controls_left(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let mode = self.option("mode", cx);
        let planning = mode.as_ref().is_some_and(|mode| mode.current == "plan");
        let disabled = mode.is_none() || self.config_busy;
        let theme = cx.theme();
        let tone = chip_tone(theme, disabled, planning);
        let selected_fill = theme.sidebar_accent;
        Button::new("composer-plan-mode")
            .ghost()
            .small()
            .h(rems(CHIP_HEIGHT))
            .icon(Icon::new(LucideIcon::ClipboardList).text_color(tone))
            .child(
                Label::new(rust_i18n::t!("Controls.plan_mode").into_owned())
                    .text_xs()
                    .text_color(tone),
            )
            .selected(planning)
            // Spec §6.2: a selected control is filled with `sidebar_accent`,
            // not the ghost variant's `secondary_active`.
            .when(planning, |this| this.bg(selected_fill))
            .disabled(disabled)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                let next = if planning { "default" } else { "plan" };
                this.set_config("mode", next.to_string(), window, cx)
            }))
            .into_any_element()
    }

    /// The bar's right controls: the context ring and the model chip.
    pub(in crate::ui) fn render_controls_right(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        // A draft has no context yet, so it shows no ring.
        let Some(handle) = self.handle() else {
            return vec![self.render_model(cx)];
        };
        let context =
            handle.read(cx).projection().controls().context().cloned();
        vec![self.render_context(context, cx), self.render_model(cx)]
    }

    /// The context ring with its fill; a popover shows the counters.
    fn render_context(
        &self,
        context: Option<ContextUsage>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let tenths = context.as_ref().and_then(ContextUsage::percent_tenths);
        let warning = tenths.is_some_and(|tenths| tenths >= WARNING_TENTHS);
        // Mockup `.ring`: the fill is `foreground`, the chip text is muted;
        // both turn to the warning color when the context is nearly full.
        let (color, text) = if warning {
            (theme.warning, theme.warning)
        } else {
            (theme.foreground, theme.muted_foreground)
        };
        let label = tenths.map_or_else(
            || "--".to_string(),
            |tenths| format!("{}.{}%", tenths / 10, tenths % 10),
        );
        // At most 1000 tenths, so the value always fits.
        let value = tenths
            .and_then(|tenths| u16::try_from(tenths).ok())
            .map_or(0.0, |tenths| f32::from(tenths) / 10.0);
        let trigger = Button::new("composer-context")
            .ghost()
            .small()
            .h(rems(CHIP_HEIGHT))
            .child(
                ProgressCircle::new("composer-context-ring")
                    .value(value)
                    .color(color)
                    .small(),
            )
            .child(Label::new(label).text_xs().text_color(text));
        let muted = theme.muted_foreground;
        Popover::new("composer-context-popover")
            .trigger(trigger)
            .content(move |_, _, _| {
                let unknown = || rust_i18n::t!("Controls.unknown").into_owned();
                let count = |value: Option<u64>| {
                    value.map_or_else(unknown, |value| {
                        TokenCount(value).label().to_string()
                    })
                };
                let (used, size, exact, tenths) = match &context {
                    Some(context) => (
                        count(context.used),
                        TokenCount(context.size).label().to_string(),
                        context.exact.clone(),
                        context.percent_tenths(),
                    ),
                    None => (unknown(), unknown(), None, None),
                };
                v_flex()
                    .gap_1()
                    .p_3()
                    .text_sm()
                    .child(
                        Label::new(
                            rust_i18n::t!("Controls.context").into_owned(),
                        )
                        .text_sm(),
                    )
                    .child(
                        Label::new(format!("{used} / {size} tokens")).text_sm(),
                    )
                    .child(
                        Label::new(format!(
                            "{} {}",
                            rust_i18n::t!("Controls.usage"),
                            tenths.map_or_else(unknown, |t| format!(
                                "{}.{}%",
                                t / 10,
                                t % 10
                            ))
                        ))
                        .text_sm()
                        .text_color(muted),
                    )
                    .when_some(exact, |this, exact| {
                        this.child(
                            Label::new(format!(
                                "{} · {}",
                                rust_i18n::t!(
                                    "Transcript.usage_in",
                                    count = count(exact.input)
                                ),
                                rust_i18n::t!(
                                    "Transcript.usage_out",
                                    count = count(exact.output)
                                ),
                            ))
                            .text_sm()
                            .text_color(muted),
                        )
                    })
            })
            .into_any_element()
    }

    /// The model and thinking-level chip and its menu.
    fn render_model(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let model = self.option("model", cx);
        let thinking = self.option("thinking_level", cx);
        let name = |option: &Option<ConfigOption>| {
            option.as_ref().and_then(|option| {
                option.current_choice().map(|choice| choice.name.clone())
            })
        };
        let label = match (name(&model), name(&thinking)) {
            (Some(model), Some(thinking)) => format!("{model} · {thinking}"),
            (Some(model), None) => model,
            _ => rust_i18n::t!("Controls.models").into_owned(),
        };
        let view = cx.entity().downgrade();
        let disabled = model.is_none() || self.config_busy;
        let tone = chip_tone(cx.theme(), disabled, false);
        Button::new("composer-model")
            .ghost()
            .small()
            .h(rems(CHIP_HEIGHT))
            // In a narrow composer (a session in the right panel) the chip
            // shrinks and cuts its label instead of spilling out of the card.
            .min_w_0()
            .flex_shrink(1.)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(label).text_xs().text_color(tone)),
            )
            .child(Icon::new(IconName::ChevronDown).xsmall().text_color(tone))
            .disabled(disabled)
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.check_side(Side::Right);
                let mut menu = menu;
                for (heading, option) in [
                    (rust_i18n::t!("Controls.models"), &model),
                    (rust_i18n::t!("Controls.thinking"), &thinking),
                ] {
                    let Some(option) = option else { continue };
                    menu =
                        menu.item(PopupMenuItem::label(heading.into_owned()));
                    for choice in &option.choices {
                        let view = view.clone();
                        let config_id: &'static str = if option.id == "model" {
                            "model"
                        } else {
                            "thinking_level"
                        };
                        let value = choice.value.clone();
                        menu = menu.item(
                            PopupMenuItem::new(choice.name.clone())
                                .checked(choice.value == option.current)
                                .on_click(move |_, window, cx| {
                                    let value = value.clone();
                                    let _ = view.update(cx, |this, cx| {
                                        this.set_config(
                                            config_id, value, window, cx,
                                        )
                                    });
                                }),
                        );
                    }
                }
                menu
            })
            .into_any_element()
    }
}

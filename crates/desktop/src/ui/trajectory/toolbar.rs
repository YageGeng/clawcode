//! The trajectory toolbar: fold toggles and the loaded totals.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Side, Sizable as _,
};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window,
    canvas, div, prelude::FluentBuilder as _,
};

use typed_builder::TypedBuilder;

use super::{TimelineVisible, TrajectoryEvent, TrajectoryView};
use crate::trajectory::timeline::DurationMode;
use crate::ui::{Elapsed, TokenCount};

/// Below this toolbar width the toggles move into the "More" menu.
const NARROW_REMS: f32 = 36.0;

/// What a toggle does when pressed.
type ToggleAction = fn(&mut TrajectoryView, &mut Context<'_, TrajectoryView>);

/// One toolbar toggle, shown as a button or as a menu item when narrow.
#[derive(Clone, Copy, TypedBuilder)]
struct Toggle {
    /// Element id of the button.
    id: &'static str,
    /// Label key.
    label: &'static str,
    /// Whether it is on.
    on: bool,
    /// What a press does.
    action: ToggleAction,
}

impl Toggle {
    /// The toggle's label, translated.
    fn text(self) -> String {
        rust_i18n::t!(self.label).into_owned()
    }
}

impl TrajectoryView {
    /// The toggles the toolbar offers.
    fn toggles(&self, cx: &gpui_kit::App) -> [Toggle; 4] {
        [
            Toggle::builder()
                .id("trajectory-duration")
                .label("Trajectory.duration_equal")
                .on(self.timeline.mode() == DurationMode::Equal)
                .action(|this, cx| {
                    let mode = match this.timeline.mode() {
                        DurationMode::Actual => DurationMode::Equal,
                        DurationMode::Equal => DurationMode::Actual,
                    };
                    this.set_duration_mode(mode, cx);
                })
                .build(),
            Toggle::builder()
                .id("trajectory-fold-runs")
                .label("Trajectory.fold_runs")
                .on(self.model.all_runs_collapsed())
                .action(|this, cx| {
                    let collapsed = !this.model.all_runs_collapsed();
                    this.model.set_all_runs_collapsed(collapsed);
                    this.sync_rows(cx);
                })
                .build(),
            Toggle::builder()
                .id("trajectory-fold-calls")
                .label("Trajectory.fold_calls")
                .on(self.model.calls_folded())
                .action(|this, cx| {
                    let folded = !this.model.calls_folded();
                    this.model.set_calls_folded(folded);
                    this.sync_rows(cx);
                })
                .build(),
            Toggle::builder()
                .id("trajectory-timeline-toggle")
                .label("Trajectory.timeline")
                .on(TimelineVisible::get(cx))
                // The shell keeps the setting for every session.
                .action(|_, cx| {
                    let open = !TimelineVisible::get(cx);
                    cx.emit(TrajectoryEvent::TimelineToggled(open));
                })
                .build(),
        ]
    }

    /// Toggles and totals; the toggles fold into a menu when narrow.
    pub(super) fn render_toolbar(
        &self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let narrow_width = window.rem_size() * NARROW_REMS;
        let width = Rc::clone(&self.toolbar_width);
        let narrow = width.get() < narrow_width;
        let view = cx.entity().downgrade();
        let toggles: Vec<AnyElement> = if narrow {
            vec![self.render_more_menu(cx)]
        } else {
            self.toggles(cx)
                .into_iter()
                .map(|toggle| {
                    Button::new(toggle.id)
                        .ghost()
                        .small()
                        .selected(toggle.on)
                        .label(toggle.text())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            (toggle.action)(this, cx)
                        }))
                        .into_any_element()
                })
                .collect()
        };
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h_10()
            .px_2()
            .border_b_1()
            .border_color(border)
            // Measures the toolbar so the next frame knows whether it is narrow.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let was_narrow = width.get() < narrow_width;
                        width.set(bounds.size.width);
                        if was_narrow != (bounds.size.width < narrow_width) {
                            // The shell draws this view cached; only a notify redraws it.
                            cx.defer(move |cx| {
                                let _ = view.update(cx, |_, cx| cx.notify());
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(toggles)
            .when_some(self.interval_label(), |this, label| {
                this.child(Label::new(label).text_sm().text_color(muted))
                    .child(
                        Button::new("trajectory-interval-clear")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(
                                rust_i18n::t!("Trajectory.interval_clear")
                                    .to_string(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_interval(None, cx)
                            })),
                    )
            })
            .when(self.model.has_more_before(), |this| {
                this.child(
                    Button::new("trajectory-load-all")
                        .ghost()
                        .xsmall()
                        .loading(self.loader.loading_all)
                        .label(rust_i18n::t!("Trajectory.load_all").to_string())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.load_all(cx)),
                        ),
                )
            })
            .child(div().flex_1())
            .child(
                Label::new(self.summary_label())
                    .text_sm()
                    .text_color(muted)
                    .truncate(),
            )
    }

    /// The "More" menu holding the toggles in a narrow toolbar.
    fn render_more_menu(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let toggles = self.toggles(cx);
        Button::new("trajectory-more")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip(rust_i18n::t!("Trajectory.more").to_string())
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.check_side(Side::Right);
                for toggle in toggles {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(toggle.text())
                            .checked(toggle.on)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, toggle.action);
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// `Time +3.2s–+9.0s` while the overview filters by time, counted from
    /// the first loaded record.
    fn interval_label(&self) -> Option<String> {
        let interval = self.model.interval()?;
        let first = self
            .model
            .records()
            .iter()
            .filter_map(|record| record.started_at_ms.map(|at| at.get()))
            .min()
            .unwrap_or(interval.start_ms);
        let since = |ms: u64| {
            format!("+{}", Elapsed(ms.saturating_sub(first)).precise())
        };
        Some(
            rust_i18n::t!(
                "Trajectory.interval",
                from = since(interval.start_ms),
                to = since(interval.end_ms)
            )
            .into_owned(),
        )
    }

    /// `3 runs · 12 requests · 12.4k tok · cache hit 85%`, over the loaded records.
    fn summary_label(&self) -> String {
        let summary = self.model.summary();
        let line = rust_i18n::t!(
            "Trajectory.summary",
            runs = summary.runs,
            requests = summary.requests,
            tokens =
                TokenCount(summary.input + summary.output).label().as_ref()
        )
        .into_owned();
        match summary.cache_hit_percent {
            Some(percent) => rust_i18n::t!(
                "Trajectory.summary_cache",
                summary = line,
                percent = percent
            )
            .into_owned(),
            None => line,
        }
    }
}

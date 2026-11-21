//! The collapsible plan panel above the composer: the agent's todo list and
//! the most recently approved plan (spec §7.3).

use gpui_kit::component::label::Label;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, prelude::FluentBuilder as _, rems,
};

use crate::projection::TodoStatus;
use crate::ui::session::SessionView;

impl SessionView {
    /// Whether the panel is open: the automatic state unless the user chose otherwise since.
    fn plan_open(&self, automatic: bool) -> bool {
        match self.plan_override {
            Some((overridden, open)) if overridden == automatic => open,
            _ => automatic,
        }
    }

    /// Opens or closes the panel by hand until the automatic state changes.
    fn toggle_plan(&mut self, cx: &mut Context<'_, Self>) {
        let automatic = self.handle().is_some_and(|handle| {
            handle.read(cx).projection().controls().plan_open()
        });
        self.plan_override = Some((automatic, !self.plan_open(automatic)));
        cx.notify();
    }

    /// The panel; `None` when there is neither a todo list nor an approved plan.
    pub(in crate::ui) fn render_plan(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let handle = self.handle()?.read(cx);
        let controls = handle.projection().controls();
        let todos = controls.todos().to_vec();
        let approved = controls.approved_plan().map(str::to_owned);
        if todos.is_empty() && approved.is_none() {
            return None;
        }
        let open = self.plan_open(controls.plan_open());
        let theme = cx.theme();
        let done = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        let header = h_flex()
            .id("plan-panel-toggle")
            .test_support()
            .gap_1p5()
            .text_sm()
            .cursor_pointer()
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(theme.muted_foreground),
            )
            .child(
                Label::new(rust_i18n::t!("Plan.title").into_owned()).text_sm(),
            )
            .when(!todos.is_empty(), |this| {
                this.child(
                    Label::new(format!("{done}/{}", todos.len()))
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
            })
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_plan(cx)),
            );
        let items = todos.into_iter().map(|todo| {
            let marker = match todo.status {
                TodoStatus::Completed => Icon::new(IconName::Check)
                    .small()
                    .text_color(theme.success)
                    .into_any_element(),
                TodoStatus::InProgress => {
                    Spinner::new().small().into_any_element()
                }
                TodoStatus::Pending => {
                    Icon::new(gpui_kit::assets::IconName::Circle)
                        .small()
                        .text_color(theme.muted_foreground)
                        .into_any_element()
                }
            };
            h_flex()
                .gap_2()
                .text_sm()
                .child(marker)
                .child(Label::new(todo.content).text_sm())
        });
        Some(
            v_flex()
                .w_full()
                .max_w(rems(41.25))
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(header)
                .when(open, |this| {
                    // The body scrolls inside a bounded height, as the plan
                    // approval card does: a long approved plan must not push
                    // the transcript and the composer out of the card.
                    this.child(
                        v_flex()
                            .id("plan-panel-body")
                            .test_support()
                            .gap_1()
                            .max_h(rems(20.))
                            .overflow_y_scroll()
                            .children(items)
                            .when_some(approved, |this, plan| {
                                this.child(
                                    v_flex()
                                        .gap_1()
                                        .pt_1()
                                        .child(
                                            Label::new(
                                                rust_i18n::t!("Plan.approved")
                                                    .into_owned(),
                                            )
                                            .text_xs()
                                            .text_color(theme.muted_foreground),
                                        )
                                        .child(TextView::markdown(
                                            SharedString::new_static(
                                                "plan-approved",
                                            ),
                                            SharedString::from(plan),
                                        )),
                                )
                            }),
                    )
                })
                .into_any_element(),
        )
    }
}

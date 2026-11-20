//! Activity rail: one icon button per sidebar mode; M1 has only Sessions.
//!
//! The rail is a narrow inset card of its own, with the content card's surface (spec §5.3, §6.1).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, TestSupportExt as _, div, prelude::FluentBuilder as _, rems,
};

use super::{AppView, TabEntry};
use crate::actions::{OpenSettings, WORKSPACE_CONTEXT};

impl AppView {
    /// Renders the rail as an inset card floating on the chrome surface.
    pub(super) fn render_rail(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("rail")
            .test_support()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_1p5()
            // 3rem holds a medium icon button with an even margin inside the card's border.
            .w(rems(3.))
            .flex_none()
            // Same gaps as the content card: `sm` from the window's left and bottom edges;
            // its top lines up with the card's because neither has a top margin.
            .ml_2()
            .mb_2()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .when(!theme.is_dark(), |this| this.shadow_sm())
            .child(
                Button::new("rail-sessions")
                    .ghost()
                    .icon(IconName::MessagesSquare)
                    .selected(true)
                    .tooltip(rust_i18n::t!("Rail.sessions").to_string()),
            )
            // Pushes the settings button to the bottom of the rail.
            .child(div().flex_1())
            .child(
                Button::new("rail-settings")
                    .ghost()
                    .icon(IconName::Settings2)
                    .selected(matches!(
                        self.active_entry(),
                        Some(TabEntry::Settings)
                    ))
                    .tooltip_with_action(
                        rust_i18n::t!("Settings.title").to_string(),
                        &OpenSettings,
                        Some(WORKSPACE_CONTEXT),
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.open_settings(cx)),
                    ),
            )
    }
}

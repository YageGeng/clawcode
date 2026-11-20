//! Session status glyph: shape and color together, so status never relies on color alone.

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};

use crate::session::SessionStatus;

/// Small status glyph shown before tab titles and after sidebar rows.
#[derive(IntoElement)]
pub struct StatusIcon {
    status: SessionStatus,
}

impl StatusIcon {
    /// Creates the glyph for `status`; idle renders nothing.
    pub fn new(status: SessionStatus) -> Self {
        Self { status }
    }
}

impl RenderOnce for StatusIcon {
    /// Draws a spinner, a question mark, or an alert depending on the status.
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let glyph = match self.status {
            SessionStatus::Idle => None,
            SessionStatus::Running => {
                Some(Spinner::new().xsmall().into_any_element())
            }
            SessionStatus::Waiting => Some(
                Icon::new(IconName::CircleQuestionMark)
                    .xsmall()
                    .text_color(cx.theme().warning)
                    .into_any_element(),
            ),
            SessionStatus::Failed => Some(
                Icon::new(IconName::CircleAlert)
                    .xsmall()
                    .text_color(cx.theme().danger)
                    .into_any_element(),
            ),
        };
        div().flex().items_center().flex_none().children(glyph)
    }
}

impl SessionStatus {
    /// Localized status word, used in accessible labels and the card header.
    pub fn label(self) -> String {
        match self {
            SessionStatus::Idle => rust_i18n::t!("Status.idle"),
            SessionStatus::Running => rust_i18n::t!("Status.running"),
            SessionStatus::Waiting => rust_i18n::t!("Status.waiting"),
            SessionStatus::Failed => rust_i18n::t!("Status.failed"),
        }
        .into_owned()
    }
}

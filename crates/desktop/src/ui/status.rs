//! Session status glyph: shape and color together, so status never relies on color alone.

use gpui_kit::assets::IconName;
use gpui_kit::component::label::Label;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
    rems,
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

/// A tab's status as a small dot that is always there: gray at rest, blue
/// while running, amber while waiting for the user, red when failed. The
/// tab's title and the card header's word say the same in text.
#[derive(IntoElement)]
pub struct StatusDot {
    status: SessionStatus,
}

impl StatusDot {
    /// Creates the dot for `status`.
    pub fn new(status: SessionStatus) -> Self {
        Self { status }
    }
}

impl RenderOnce for StatusDot {
    /// A filled circle in the status color.
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = match self.status {
            SessionStatus::Idle => theme.muted_foreground.opacity(0.5),
            SessionStatus::Running => theme.info,
            SessionStatus::Waiting => theme.warning,
            SessionStatus::Failed => theme.danger,
        };
        div().flex_none().size(rems(0.5)).rounded_full().bg(color)
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

/// A change's line counts: the added lines in green, the removed ones in
/// red, as the Changes tab and the turn change cards show them.
#[derive(IntoElement)]
pub struct LineCounts {
    added: u64,
    removed: u64,
}

impl LineCounts {
    /// The counts of one change.
    pub fn new(added: u64, removed: u64) -> Self {
        Self { added, removed }
    }

    /// The counts of one changed file; files without counts, such as
    /// binary ones, show zeros.
    pub fn of(change: &protocol::WorkspaceChange) -> Self {
        Self::new(
            change.insertions.unwrap_or_default(),
            change.deletions.unwrap_or_default(),
        )
    }
}

impl RenderOnce for LineCounts {
    /// `+added −removed` in extra small text, each in its color.
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .gap_1()
            .child(
                Label::new(format!("+{}", self.added))
                    .text_xs()
                    .text_color(theme.success),
            )
            .child(
                Label::new(format!("−{}", self.removed))
                    .text_xs()
                    .text_color(theme.danger),
            )
    }
}

//! A changed file's row and line counts, shared by the Changes tab and the
//! turn change cards.

use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use protocol::{WorkspaceChange, WorkspaceChangeKind};
use typed_builder::TypedBuilder;

use super::file_icon::FileIconPainter;

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

    /// The counts of one changed file; `None` for a file that was not
    /// counted, such as a binary or oversized one.
    pub fn of(change: &WorkspaceChange) -> Option<Self> {
        Some(Self::new(change.insertions?, change.deletions?))
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

/// One changed file as the Changes tab and the turn change cards list it: its
/// icon, its name in the status color, its line counts, and its status letter.
#[derive(IntoElement, TypedBuilder)]
pub struct ChangedFileRow {
    icon: AnyElement,
    name: SharedString,
    #[builder(default)]
    counts: Option<LineCounts>,
    kind: WorkspaceChangeKind,
}

impl ChangedFileRow {
    /// The row of `change` under `name`, its icon drawn by `painter`.
    pub fn new(
        painter: &FileIconPainter,
        name: SharedString,
        change: &WorkspaceChange,
    ) -> Self {
        Self::builder()
            .icon(painter.file(&name))
            .name(name)
            .counts(LineCounts::of(change))
            .kind(change.kind)
            .build()
    }
}

impl RenderOnce for ChangedFileRow {
    /// The name truncates first; the counts and the letter stay in view.
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = match self.kind {
            WorkspaceChangeKind::Added | WorkspaceChangeKind::Untracked => {
                theme.success
            }
            WorkspaceChangeKind::Deleted | WorkspaceChangeKind::Conflicted => {
                theme.danger
            }
            WorkspaceChangeKind::Modified => theme.warning,
            WorkspaceChangeKind::Renamed => theme.info,
        };
        h_flex()
            .gap_1p5()
            .text_sm()
            .child(self.icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(self.name).text_sm().text_color(color)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .font_family(theme.mono_font_family.clone())
                    .children(self.counts)
                    // The letter says the status in text, too.
                    .child(
                        Label::new(self.kind.letter().to_string())
                            .text_xs()
                            .font_semibold()
                            .text_color(color),
                    ),
            )
    }
}

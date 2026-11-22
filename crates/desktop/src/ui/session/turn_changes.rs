//! The card under a turn's last reply listing the files its run changed
//! (M4b spec §6.1), which opens the run in the panel's Changes tab.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, div, rems,
};

use agent_client_protocol::v2::schema as wire;

use super::{SessionView, SessionViewEvent};
use crate::ui::LineCounts;

impl SessionView {
    /// The card under a turn's last reply listing the files its run changed
    /// (M4b spec §6.1): a line with the file count and line totals, unfolding
    /// to the files. The line opens the run in the Changes tab, a file opens
    /// it there too.
    pub(super) fn render_turn_changes(
        &self,
        id: &wire::MessageId,
        cx: &Context<'_, Self>,
    ) -> Option<AnyElement> {
        let run = self.marks.turn_changes.get(id)?;
        let theme = cx.theme();
        let changes = &run.changes;
        let open = self.open_turn_changes.contains(&run.run_id);
        let run_id = run.run_id.clone();
        let toggle = run.run_id.clone();
        let header = h_flex()
            .id(SharedString::from(format!("turn-changes:{}", run.run_id)))
            .test_support()
            .gap_2()
            .px_3()
            .py_1p5()
            .cursor_pointer()
            .child(
                Icon::new(IconName::FileDiff)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                Label::new(
                    rust_i18n::t!(
                        "Transcript.turn_changed",
                        count = changes.changes.len()
                    )
                    .into_owned(),
                )
                .text_sm()
                .flex_1(),
            )
            .child(LineCounts::new(changes.insertions, changes.deletions))
            .child(
                Button::new(SharedString::from(format!(
                    "turn-changes-toggle:{}",
                    run.run_id
                )))
                .ghost()
                .xsmall()
                .icon(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if !this.open_turn_changes.remove(&toggle) {
                            this.open_turn_changes.insert(toggle.clone());
                        }
                        cx.notify();
                    },
                )),
            )
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(SessionViewEvent::OpenChanges {
                    run: run_id.clone(),
                    path: None,
                })
            }));
        let files = open.then(|| {
            let nodes = crate::file::ChangeNode::tree(
                changes.changes.iter().map(|change| change.path.as_str()),
            );
            v_flex()
                .px_3()
                .pb_2()
                .children(self.render_turn_files(run, &nodes, 0, cx))
        });
        Some(
            v_flex()
                .mt_1()
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius)
                .child(header)
                .children(files)
                .into_any_element(),
        )
    }

    /// The rows of a turn card's file tree: folders, then their files, each
    /// file opening the run's change at it.
    fn render_turn_files(
        &self,
        run: &protocol::RunCheckpoint,
        nodes: &[crate::file::ChangeNode],
        depth: usize,
        cx: &Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let mut rows = Vec::new();
        for node in nodes {
            let indent = rems(0.75 * depth as f32);
            let Some(index) = node.file else {
                rows.push(
                    h_flex()
                        .pl(indent)
                        .gap_1p5()
                        .child(
                            Icon::new(IconName::Folder)
                                .small()
                                .text_color(theme.muted_foreground),
                        )
                        .child(Label::new(node.name.clone()).text_sm())
                        .into_any_element(),
                );
                rows.extend(self.render_turn_files(
                    run,
                    &node.children,
                    depth + 1,
                    cx,
                ));
                continue;
            };
            let Some(change) = run.changes.changes.get(index) else {
                continue;
            };
            let run_id = run.run_id.clone();
            let path = change.path.clone();
            rows.push(
                h_flex()
                    .id(SharedString::from(format!(
                        "turn-file:{}:{index}",
                        run.run_id
                    )))
                    .pl(indent)
                    .gap_1p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .child(
                        Icon::new(IconName::File)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(Label::new(node.name.clone()).text_sm()),
                    )
                    .child(LineCounts::of(change))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(SessionViewEvent::OpenChanges {
                            run: run_id.clone(),
                            path: Some(path.clone()),
                        })
                    }))
                    .into_any_element(),
            );
        }
        rows
    }
}

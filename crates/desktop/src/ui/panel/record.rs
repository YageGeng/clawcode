//! The record detail panel tab (M4a spec §7): the record selected in the
//! owner session's trajectory, its header, and a switch between sections.

use std::collections::HashSet;
use std::mem::Discriminant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::DiffState;
use gpui_kit::component::label::Label;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::{
    AnyElement, Context, DismissEvent, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, div, prelude::FluentBuilder as _,
};
use protocol::{TrajectoryRecord, TrajectoryRecordKind};
use typed_builder::TypedBuilder;

use super::record_sections::RequestPair;

use crate::trajectory::RecordText as _;
use crate::ui::TrajectoryView;
use crate::ui::trajectory::{KindLook as _, RequestTitle as _};

/// One section of the record detail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordSection {
    /// What the record says.
    #[default]
    Summary,
    /// Request times, latency, and throughput.
    Timing,
    /// Token usage of the request and of the session so far.
    Usage,
    /// The system prompt the request sent.
    SystemPrompt,
    /// The tools the request offered.
    Tools,
    /// The request's generation options.
    Options,
    /// A tool call's arguments.
    Arguments,
    /// A tool call's result.
    Result,
    /// The record as JSON.
    Raw,
}

impl RecordSection {
    /// The sections a record offers, in order (spec §7). The request
    /// sections need the request's snapshot, which sessions recorded before
    /// snapshots lack.
    pub fn for_record(
        record: &TrajectoryRecord,
        recorded: bool,
    ) -> &'static [RecordSection] {
        use RecordSection::{
            Arguments, Options, Raw, Result, Summary, SystemPrompt, Timing,
            Tools, Usage,
        };
        match record.kind {
            TrajectoryRecordKind::Request(_)
            | TrajectoryRecordKind::Assistant(_)
                if recorded =>
            {
                &[Summary, Timing, Usage, SystemPrompt, Tools, Options, Raw]
            }
            TrajectoryRecordKind::Assistant(_) => {
                &[Summary, Timing, Usage, Raw]
            }
            TrajectoryRecordKind::Tool(_) => {
                &[Summary, Arguments, Result, Timing, Raw]
            }
            _ => &[Summary, Raw],
        }
    }

    /// The section's title.
    pub fn label(self) -> SharedString {
        let key = match self {
            Self::Summary => "Record.section_summary",
            Self::Timing => "Record.section_timing",
            Self::Usage => "Record.section_usage",
            Self::SystemPrompt => "Record.section_system_prompt",
            Self::Tools => "Record.section_tools",
            Self::Options => "Record.section_options",
            Self::Arguments => "Record.section_arguments",
            Self::Result => "Record.section_result",
            Self::Raw => "Record.section_raw",
        };
        rust_i18n::t!(key).into_owned().into()
    }
}

/// How the system prompt shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptMode {
    /// The whole prompt.
    #[default]
    Full,
    /// What changed since the previous request.
    Diff,
}

/// The detail of the record selected in one session's trajectory.
#[derive(TypedBuilder)]
pub struct RecordView {
    pub(super) trajectory: Entity<TrajectoryView>,
    #[builder(default)]
    pub(super) section: RecordSection,
    #[builder(default)]
    pub(super) prompt_mode: PromptMode,
    /// Shown inline below a trajectory rather than as a panel tab: the
    /// header offers a close button that dismisses it.
    #[builder(default)]
    closable: bool,
    /// Kind of the record shown last; another kind starts at its summary.
    #[builder(default)]
    shown_kind: Option<Discriminant<TrajectoryRecordKind>>,
    /// The text the code viewer shows, as built for its key: the raw text
    /// for copying and the Markdown drawn.
    #[builder(default)]
    pub(super) code: Option<(String, SharedString, SharedString)>,
    /// Summary parts the user folded, by title; kept while stepping
    /// through records.
    #[builder(default)]
    pub(super) folded: HashSet<&'static str>,
    /// The prompt diff and the pair of hashes it was built for.
    #[builder(default)]
    pub(super) diff: Option<(String, Entity<DiffState>)>,
    /// Tools whose description and schema are unfolded.
    #[builder(default)]
    pub(super) open_tools: HashSet<String>,
    _observe: Subscription,
}

impl EventEmitter<DismissEvent> for RecordView {}

impl RecordView {
    /// Shows the selection of `trajectory`, following it as it changes.
    pub fn new(
        trajectory: Entity<TrajectoryView>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let observe = cx.observe(&trajectory, |this, trajectory, cx| {
            let kind = trajectory
                .read(cx)
                .model()
                .selected_record()
                .map(|record| std::mem::discriminant(&record.kind));
            if kind != this.shown_kind {
                this.shown_kind = kind;
                this.section = RecordSection::Summary;
                this.prompt_mode = PromptMode::Full;
            }
            cx.notify();
        });
        let shown_kind = trajectory
            .read(cx)
            .model()
            .selected_record()
            .map(|record| std::mem::discriminant(&record.kind));
        Self::builder()
            .trajectory(trajectory)
            .shown_kind(shown_kind)
            ._observe(observe)
            .build()
    }

    /// The same detail shown inline, with a close button that emits
    /// [`DismissEvent`].
    pub fn closable(mut self) -> Self {
        self.closable = true;
        self
    }

    /// The trajectory whose selection this detail shows.
    pub fn trajectory(&self) -> &Entity<TrajectoryView> {
        &self.trajectory
    }

    /// The notice for a content the session's log does not hold.
    pub fn missing_text() -> String {
        rust_i18n::t!("Record.missing").into_owned()
    }

    /// Kind label, title, status, and the previous and next buttons.
    fn render_header(
        &self,
        record: &TrajectoryRecord,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (title, failed) = match &record.kind {
            TrajectoryRecordKind::Request(request) => (request.title(), false),
            TrajectoryRecordKind::Assistant(assistant) => (
                assistant.title(),
                assistant.error.is_some() || assistant.cancelled,
            ),
            TrajectoryRecordKind::Tool(tool) => {
                (tool.name.clone(), tool.is_error)
            }
            _ => (record.summary(), false),
        };
        let running = matches!(
            &record.kind,
            TrajectoryRecordKind::Tool(tool) if tool.result_text.is_none()
        );
        let status = match (failed, running) {
            (true, _) => rust_i18n::t!("Record.status_failed"),
            (_, true) => rust_i18n::t!("Record.status_running"),
            _ => rust_i18n::t!("Record.status_done"),
        };
        let step = |id: &'static str,
                    icon,
                    label: &str,
                    delta: isize,
                    cx: &mut Context<'_, Self>| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(rust_i18n::t!(label).to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.trajectory.update(cx, |trajectory, cx| {
                        trajectory.select_adjacent(delta, cx)
                    });
                }))
        };
        // The kind leads, in the ledger's icon and color, so the header
        // says what the record is before what it says.
        let hue = record.kind.kind_hue().color(theme);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("record-kind")
                    .test_support()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .text_color(hue)
                    .child(Icon::new(record.kind.kind_icon()).xsmall())
                    // A request's title already names it ("Request #12 ·
                    // …"); its icon alone marks it.
                    .when(
                        !matches!(
                            record.kind,
                            TrajectoryRecordKind::Request(_)
                        ),
                        |this| {
                            this.child(
                                Label::new(record.kind.kind_label())
                                    .text_xs()
                                    .text_color(hue),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(title).text_sm().font_semibold()),
            )
            .child(Label::new(status.into_owned()).text_xs().text_color(
                if failed {
                    theme.danger
                } else {
                    theme.muted_foreground
                },
            ))
            .child(step(
                "record-prev",
                IconName::ChevronUp,
                "Record.prev",
                -1,
                cx,
            ))
            .child(step(
                "record-next",
                IconName::ChevronDown,
                "Record.next",
                1,
                cx,
            ))
            .when(self.closable, |this| {
                this.child(
                    Button::new("record-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(rust_i18n::t!("Record.close").into_owned())
                        .on_click(
                            cx.listener(|_, _, _, cx| cx.emit(DismissEvent)),
                        ),
                )
            })
    }

    /// The segmented switch between the record's sections.
    fn render_sections(
        &self,
        sections: &'static [RecordSection],
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let selected = sections
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        div().flex_none().px_3().py_2().child(
            TabBar::new("record-sections")
                .segmented()
                .small()
                .selected_index(selected)
                .children(
                    sections
                        .iter()
                        .map(|section| Tab::new().label(section.label())),
                )
                .on_click(cx.listener(move |this, index: &usize, _, cx| {
                    if let Some(section) = sections.get(*index) {
                        this.section = *section;
                        cx.notify();
                    }
                })),
        )
    }
}

impl Render for RecordView {
    /// The header, the section switch, and the section, or a hint without a selection.
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let record =
            self.trajectory.read(cx).model().selected_record().cloned();
        let Some(record) = record else {
            return div()
                .id("record-empty")
                .test_support()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .child(
                    Label::new(rust_i18n::t!("Record.hint").into_owned())
                        .text_color(cx.theme().muted_foreground),
                )
                .into_any_element();
        };
        let recorded = {
            let trajectory = self.trajectory.read(cx);
            RequestPair::of(trajectory.model(), &record)
                .request
                .is_some()
        };
        let sections = RecordSection::for_record(&record, recorded);
        if !sections.contains(&self.section) {
            self.section = RecordSection::Summary;
        }
        let body: AnyElement = self.render_section(&record, window, cx);
        div()
            .id("record-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_header(&record, cx))
            .child(self.render_sections(sections, cx))
            .child(
                div()
                    .id("record-section")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .child(body),
            )
            .into_any_element()
    }
}

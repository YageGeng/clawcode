//! The trajectory ledger: ledger rows turned into display lines, drawn by
//! gpui-kit's virtual table with one uniform row height.

use std::collections::HashMap;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme,
};
use gpui_kit::{
    App, Context, Div, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div,
    prelude::FluentBuilder as _, rems,
};
use protocol::{
    SettingChange, TrajectoryRecord, TrajectoryRecordKind, UserOrigin,
};
use typed_builder::TypedBuilder;

use super::{RequestTitle as _, TrajectoryView};
use crate::trajectory::{
    InFlight, LedgerRow, RecordText as _, TrajectoryModel,
};
use crate::ui::{Elapsed, TokenCount};

/// Columns: type, content, input, output, thinking, time.
const COLUMNS: usize = 6;

/// How a line's text is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    /// Body text.
    Normal,
    /// Body text a little heavier: what the user said.
    Strong,
    /// Secondary text.
    Muted,
    /// A failure.
    Danger,
    /// A group title.
    Heading,
}

/// Which kind of work a line shows, colored as the time overview colors it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::ui) enum Hue {
    /// Events that are not drawn as bars: settings, commands, and the like.
    #[default]
    Neutral,
    /// A model request and its answer.
    Request,
    /// A tool call.
    Tool,
    /// What the user said.
    User,
}

impl Hue {
    /// The theme's base hue for this kind, shared with the time overview's
    /// bars; the base colors keep their hue in every theme, where the chart
    /// colors of some themes are shades of gray.
    pub(in crate::ui) fn color(self, theme: &Theme) -> Hsla {
        match self {
            Hue::Neutral => theme.muted_foreground,
            Hue::Request => theme.blue,
            Hue::Tool => theme.yellow,
            Hue::User => theme.magenta,
        }
    }
}

/// How a record's kind reads wherever it is named: the ledger's type column
/// and the record detail's header.
pub(in crate::ui) trait KindLook {
    /// The kind's name.
    fn kind_label(&self) -> SharedString;
    /// The kind's icon.
    fn kind_icon(&self) -> LucideIcon;
    /// The kind's color.
    fn kind_hue(&self) -> Hue;
}

impl KindLook for TrajectoryRecordKind {
    /// The translated name of the kind.
    fn kind_label(&self) -> SharedString {
        text(match self {
            Self::User(_) => "Trajectory.kind_user",
            Self::Command(_) => "Trajectory.kind_command",
            Self::Request(_) => "Trajectory.kind_request",
            Self::Assistant(_) => "Trajectory.kind_assistant",
            Self::Tool(_) => "Trajectory.kind_tool",
            Self::Compaction { .. } => "Trajectory.kind_compaction",
            Self::BranchSummary { .. } => "Trajectory.kind_branch",
            Self::Setting(_) => "Trajectory.kind_setting",
            _ => "Trajectory.kind_extension",
        })
    }

    /// One Lucide icon per kind.
    fn kind_icon(&self) -> LucideIcon {
        match self {
            Self::User(_) => LucideIcon::User,
            Self::Command(_) => LucideIcon::SquareSlash,
            Self::Request(_) => LucideIcon::Send,
            Self::Assistant(_) => LucideIcon::Bot,
            Self::Tool(_) => LucideIcon::Wrench,
            Self::Compaction { .. } => LucideIcon::Minimize2,
            Self::BranchSummary { .. } => LucideIcon::GitBranch,
            Self::Setting(_) => LucideIcon::Settings2,
            _ => LucideIcon::Puzzle,
        }
    }

    /// Requests and their answers, tools, and the user have hues; other
    /// events stay neutral.
    fn kind_hue(&self) -> Hue {
        match self {
            Self::Request(_) | Self::Assistant(_) => Hue::Request,
            Self::Tool(_) => Hue::Tool,
            Self::User(_) => Hue::User,
            _ => Hue::Neutral,
        }
    }
}

/// What one ledger line shows, already formatted.
#[derive(Debug, Clone, TypedBuilder)]
pub(super) struct LedgerLine {
    /// The ledger row it draws.
    pub(super) row: LedgerRow,
    /// Element id of the table row.
    pub(super) element_id: SharedString,
    /// Type column: a kind label or a group title.
    pub(super) kind: SharedString,
    /// The icon before the kind label, so kinds read apart at a glance.
    #[builder(default, setter(strip_option))]
    pub(super) icon: Option<LucideIcon>,
    /// The color of the icon and kind label.
    #[builder(default)]
    pub(super) hue: Hue,
    /// Nesting below the run title: 0 for titles, 1 for what the run did,
    /// 2 for the tool calls a request made.
    #[builder(default)]
    pub(super) depth: u8,
    /// Content column.
    #[builder(default)]
    pub(super) content: SharedString,
    /// Input, output, and thinking token columns.
    #[builder(default)]
    pub(super) tokens: [SharedString; 3],
    /// Time column; in-flight work counts up when drawn (see `duration_now`).
    #[builder(default)]
    pub(super) duration: SharedString,
    /// When a loaded call that is still executing started; its time counts
    /// up like an in-flight row's.
    #[builder(default, setter(strip_option))]
    pub(super) running_since: Option<u64>,
    /// Text color.
    #[builder(default = Tone::Normal)]
    pub(super) tone: Tone,
}

impl LedgerLine {
    /// The time column now: work still in flight counts up to this moment.
    fn duration_now(&self) -> SharedString {
        match &self.row {
            LedgerRow::InFlight(work) => {
                Elapsed(crate::now_ms().saturating_sub(work.started_at_ms()))
                    .precise()
            }
            _ => match self.running_since {
                Some(since) => {
                    Elapsed(crate::now_ms().saturating_sub(since)).precise()
                }
                None => self.duration.clone(),
            },
        }
    }
}

/// Formats ledger rows with the record lookups they need.
pub(super) struct LineBuilder<'a> {
    records: HashMap<&'a str, &'a TrajectoryRecord>,
    /// First-token latency of each request number.
    latencies: HashMap<u32, u64>,
    /// Start of each loaded call still executing, by record id.
    executing: HashMap<String, u64>,
}

impl<'a> LineBuilder<'a> {
    /// Indexes the loaded records of `model` and the calls of `in_flight`.
    pub(super) fn new(
        model: &'a TrajectoryModel,
        in_flight: &[InFlight],
    ) -> Self {
        let mut latencies = HashMap::new();
        for record in model.records() {
            if let TrajectoryRecordKind::Assistant(assistant) = &record.kind {
                latencies.insert(
                    assistant.request_index,
                    assistant
                        .first_token_at_ms
                        .get()
                        .saturating_sub(assistant.requested_at_ms.get()),
                );
            }
        }
        Self {
            records: model
                .records()
                .iter()
                .map(|record| (record.id.as_str(), record))
                .collect(),
            latencies,
            executing: in_flight
                .iter()
                .filter_map(|work| {
                    Some((work.record_id()?, work.started_at_ms()))
                })
                .collect(),
        }
    }

    /// The display line of `row`.
    pub(super) fn line(&self, row: &LedgerRow) -> LedgerLine {
        let builder = LedgerLine::builder().row(row.clone());
        match row {
            LedgerRow::LoadEarlier => builder
                .element_id("trajectory-load-earlier".into())
                .kind(SharedString::default())
                .content(text("Trajectory.load_earlier"))
                .tone(Tone::Muted)
                .build(),
            LedgerRow::RunHeader {
                key,
                index,
                duration_ms,
                tokens,
                ..
            } => {
                let (element_id, title) = match index {
                    Some(index) => (
                        format!("run-header:{index}"),
                        rust_i18n::t!("Trajectory.run", index = index)
                            .into_owned(),
                    ),
                    None => (
                        format!(
                            "run-header:between:{}",
                            match key {
                                crate::trajectory::RunKey::Between(seq) => *seq,
                                crate::trajectory::RunKey::Run(_) => 0,
                            }
                        ),
                        text("Trajectory.between").to_string(),
                    ),
                };
                builder
                    .element_id(element_id.into())
                    .kind(title.into())
                    .content(
                        format!("{} tok", TokenCount(*tokens).label()).into(),
                    )
                    .duration(
                        duration_ms
                            .map(|ms| Elapsed(ms).label())
                            .unwrap_or_default(),
                    )
                    .tone(Tone::Heading)
                    .build()
            }
            LedgerRow::Record { id } => match self.records.get(id.as_str()) {
                Some(record) => self.record_line(row, record),
                None => builder
                    .element_id(format!("ledger-row:{id}").into())
                    .kind(SharedString::default())
                    .build(),
            },
            LedgerRow::CallGroup {
                request_index,
                count,
                duration_ms,
                failures,
            } => builder
                .element_id(format!("call-group:{request_index}").into())
                .kind(text("Trajectory.kind_calls"))
                .icon(LucideIcon::Wrench)
                .hue(Hue::Tool)
                .depth(2)
                .content(
                    rust_i18n::t!(
                        "Trajectory.call_group",
                        count = count,
                        time = Elapsed(*duration_ms).precise().as_ref(),
                        failures = failures
                    )
                    .into_owned()
                    .into(),
                )
                .duration(Elapsed(*duration_ms).precise())
                .tone(if *failures > 0 {
                    Tone::Danger
                } else {
                    Tone::Muted
                })
                .build(),
            LedgerRow::InFlight(work) => {
                let content = match work {
                    InFlight::Request { .. } => {
                        text("Trajectory.running_request").to_string()
                    }
                    InFlight::Tool { name, .. } => {
                        rust_i18n::t!("Trajectory.running_tool", name = name)
                            .into_owned()
                    }
                };
                builder
                    // One request runs at a time; calls are told apart by id.
                    .element_id(
                        match work {
                            InFlight::Request { .. } => {
                                "in-flight:request".to_string()
                            }
                            InFlight::Tool { call_id, .. } => {
                                format!("in-flight:tool:{call_id}")
                            }
                        }
                        .into(),
                    )
                    .kind(text("Trajectory.kind_running"))
                    .depth(match work {
                        InFlight::Request { .. } => 1,
                        InFlight::Tool { .. } => 2,
                    })
                    .content(content.into())
                    .tone(Tone::Muted)
                    .build()
            }
        }
    }

    /// The line of one record.
    fn record_line(
        &self,
        row: &LedgerRow,
        record: &TrajectoryRecord,
    ) -> LedgerLine {
        let duration = record
            .started_at_ms
            .zip(record.ended_at_ms)
            .map(|(start, end)| {
                Elapsed(end.get().saturating_sub(start.get())).precise()
            })
            .unwrap_or_default();
        let builder = LedgerLine::builder()
            .row(row.clone())
            .element_id(format!("ledger-row:{}", record.id).into())
            .icon(record.kind.kind_icon())
            .hue(record.kind.kind_hue())
            .depth(match record.kind {
                TrajectoryRecordKind::Tool(_) => 2,
                _ => 1,
            });
        match &record.kind {
            TrajectoryRecordKind::Request(request) => {
                let title = request.title();
                // The first-token latency leads the content; the type column is narrow.
                let latency =
                    self.latencies.get(&request.index).map(|latency| {
                        rust_i18n::t!(
                            "Trajectory.first_token",
                            time = Elapsed(*latency).precise().as_ref()
                        )
                        .into_owned()
                    });
                let change = if request.first {
                    text("Trajectory.initial_prompt").to_string()
                } else {
                    let mut parts = Vec::new();
                    if request.prompt_changed {
                        parts.push(
                            text("Trajectory.prompt_changed").to_string(),
                        );
                    }
                    if !request.tools_added.is_empty()
                        || !request.tools_removed.is_empty()
                    {
                        parts.push(
                            rust_i18n::t!(
                                "Trajectory.tools_changed",
                                added = request.tools_added.len(),
                                removed = request.tools_removed.len()
                            )
                            .into_owned(),
                        );
                    }
                    parts.join(" · ")
                };
                let content = latency
                    .into_iter()
                    .chain((!change.is_empty()).then_some(change))
                    .collect::<Vec<_>>()
                    .join(" · ");
                builder
                    .kind(title.into())
                    .content(content.into())
                    .tone(Tone::Heading)
                    .build()
            }
            TrajectoryRecordKind::Assistant(assistant) => {
                let usage = assistant.usage.as_ref();
                let count = |value: Option<u64>| {
                    value.map(|n| TokenCount(n).label()).unwrap_or_default()
                };
                let mut content = record.summary();
                if content.is_empty() && !assistant.tool_call_ids.is_empty() {
                    content = text("Trajectory.tool_calls_only").to_string();
                }
                let failed = assistant.error.is_some() || assistant.cancelled;
                if let Some(error) = &assistant.error {
                    content =
                        format!("{} · {error}", text("Trajectory.failed"));
                } else if assistant.cancelled {
                    content =
                        format!("{} · {content}", text("Trajectory.cancelled"));
                }
                builder
                    .kind(text("Trajectory.kind_assistant"))
                    .content(content.into())
                    .tokens([
                        count(usage.input_tokens),
                        count(usage.output_tokens),
                        count(usage.reasoning_tokens),
                    ])
                    .duration(duration)
                    .tone(if failed { Tone::Danger } else { Tone::Normal })
                    .build()
            }
            // Tool calls sit under the request that made them and stay quiet
            // beside the conversation, unless they failed.
            TrajectoryRecordKind::Tool(tool) => {
                let builder = builder
                    .kind(text("Trajectory.kind_tool"))
                    .content(record.summary().into())
                    .duration(duration)
                    .tone(if tool.is_error {
                        Tone::Danger
                    } else {
                        Tone::Muted
                    });
                match self.executing.get(&record.id) {
                    Some(since) => builder.running_since(*since).build(),
                    None => builder.build(),
                }
            }
            TrajectoryRecordKind::User(user) => {
                let mut tags = Vec::new();
                match user.origin {
                    UserOrigin::Input => {}
                    UserOrigin::Steering => {
                        tags.push(text("Trajectory.origin_steering"));
                    }
                    UserOrigin::FollowUp => {
                        tags.push(text("Trajectory.origin_follow_up"));
                    }
                    UserOrigin::Skill => {
                        tags.push(text("Trajectory.origin_skill"));
                    }
                }
                let attachments = user.image_count + user.file_count;
                if attachments > 0 {
                    tags.push(
                        rust_i18n::t!(
                            "Trajectory.attachments",
                            count = attachments
                        )
                        .into_owned()
                        .into(),
                    );
                }
                let mut content = tags
                    .iter()
                    .map(|tag| format!("[{tag}] "))
                    .collect::<String>();
                content.push_str(&record.summary());
                builder
                    .kind(text("Trajectory.kind_user"))
                    .content(content.into())
                    .tone(Tone::Strong)
                    .build()
            }
            TrajectoryRecordKind::Setting(change) => {
                let content = match change {
                    SettingChange::Model { model, .. } => {
                        rust_i18n::t!("Trajectory.setting_model", model = model)
                    }
                    SettingChange::ThinkingLevel { .. } => rust_i18n::t!(
                        "Trajectory.setting_thinking",
                        level = record.summary()
                    ),
                    SettingChange::ActiveTools { .. } => rust_i18n::t!(
                        "Trajectory.setting_tools",
                        tools = record.summary()
                    ),
                };
                builder
                    .kind(text("Trajectory.kind_setting"))
                    .content(content.into_owned().into())
                    .tone(Tone::Muted)
                    .build()
            }
            kind => builder
                .kind(kind.kind_label())
                .content(record.summary().into())
                .tone(Tone::Muted)
                .build(),
        }
    }
}

/// A translated string.
fn text(key: &str) -> SharedString {
    rust_i18n::t!(key).into_owned().into()
}

/// Draws the ledger lines and reports clicks on the view.
pub struct LedgerTable {
    pub(super) lines: Vec<LedgerLine>,
    /// Column widths: type, content, then the narrow columns.
    widths: [Pixels; COLUMNS],
    view: WeakEntity<TrajectoryView>,
}

impl LedgerTable {
    /// An empty ledger whose clicks go to `view`; `rem` sizes the columns.
    pub(super) fn new(view: WeakEntity<TrajectoryView>, rem: Pixels) -> Self {
        Self {
            lines: Vec::new(),
            // Thinking is the widest header of the narrow columns.
            widths: [16.0, 24.0, 5.0, 5.0, 6.5, 5.0].map(|rems| rem * rems),
            view,
        }
    }

    /// The color of `tone`.
    fn color(tone: Tone, cx: &App) -> Hsla {
        let theme = cx.theme();
        match tone {
            Tone::Normal | Tone::Strong | Tone::Heading => theme.foreground,
            Tone::Muted => theme.muted_foreground,
            Tone::Danger => theme.danger,
        }
    }
}

impl TableDelegate for LedgerTable {
    /// Six columns.
    fn columns_count(&self, _: &App) -> usize {
        COLUMNS
    }

    /// One row per line.
    fn rows_count(&self, _: &App) -> usize {
        self.lines.len()
    }

    /// The column definitions; numbers align right.
    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (key, name) = match col_ix {
            0 => ("kind", "Trajectory.col_kind"),
            1 => ("content", "Trajectory.col_content"),
            2 => ("input", "Trajectory.col_input"),
            3 => ("output", "Trajectory.col_output"),
            4 => ("reasoning", "Trajectory.col_reasoning"),
            _ => ("duration", "Trajectory.col_duration"),
        };
        let column = Column::new(key, text(name))
            .width(self.widths.get(col_ix).copied().unwrap_or_default());
        if col_ix >= 2 {
            column.text_right()
        } else {
            column
        }
    }

    /// A column title, centered over its column.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<'_, TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .w_full()
            .text_center()
            .child(self.column(col_ix, cx).name)
    }

    /// Names each row by its line so tests and screen readers find it; group
    /// titles, folds, and load-earlier act on click. A run title is shaded
    /// and ruled off above, so each run reads as one block.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<'_, TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(line) = self.lines.get(row_ix) else {
            return div().id(("row", row_ix));
        };
        let row = line.row.clone();
        let view = self.view.clone();
        let theme = cx.theme();
        div()
            .id(line.element_id.clone())
            .when(matches!(row, LedgerRow::RunHeader { .. }), |this| {
                this.bg(theme.primary.opacity(0.08))
                    .when(row_ix > 0, |this| {
                        this.border_t_1().border_color(theme.border)
                    })
            })
            .when(!matches!(row, LedgerRow::Record { .. }), |this| {
                this.cursor_pointer().on_click(move |_, _, cx| {
                    let _ = view.update(cx, |view, cx| view.activate(&row, cx));
                })
            })
    }

    /// One cell: titles in the type column, a truncated summary, and
    /// tabular numbers.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<'_, TableState<Self>>,
    ) -> impl IntoElement {
        let Some(line) = self.lines.get(row_ix) else {
            return div();
        };
        let cell = match col_ix {
            0 => line.kind.clone(),
            1 => line.content.clone(),
            2..=4 => line.tokens.get(col_ix - 2).cloned().unwrap_or_default(),
            _ => line.duration_now(),
        };
        let theme = cx.theme();
        // The kind label takes its line's hue unless the line failed.
        let hue = match line.tone {
            Tone::Danger => theme.danger,
            _ => line.hue.color(theme),
        };
        let color = match (line.tone, col_ix) {
            (Tone::Heading, 1..) => theme.muted_foreground,
            (Tone::Danger, _) => theme.danger,
            (_, 0) if line.hue != Hue::Neutral => hue,
            (tone, _) => Self::color(tone, cx),
        };
        let text = div()
            .min_w_0()
            .truncate()
            .when(line.tone == Tone::Heading && col_ix == 0, |this| {
                this.font_semibold()
            })
            .when(line.tone == Tone::Strong && col_ix <= 1, |this| {
                this.font_medium()
            })
            .child(cell);
        div()
            .w_full()
            .min_w_0()
            .text_sm()
            .text_color(color)
            .when(col_ix >= 2, |this| {
                this.truncate().font_family(theme.mono_font_family.clone())
            })
            .map(|this| match col_ix {
                // The type column steps in per level and leads with the
                // kind's icon.
                0 => this
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .pl(rems(0.875 * f32::from(line.depth)))
                    .when_some(line.icon, |this, icon| {
                        this.child(
                            Icon::new(icon)
                                .xsmall()
                                .flex_none()
                                .text_color(hue),
                        )
                    })
                    .child(text),
                _ => this.child(text),
            })
    }

    /// The cell's text, for screen readers and tests.
    fn cell_label(
        &self,
        row_ix: usize,
        col_ix: usize,
        _: &App,
    ) -> Option<SharedString> {
        let line = self.lines.get(row_ix)?;
        Some(match col_ix {
            0 => line.kind.clone(),
            1 => line.content.clone(),
            2..=4 => line.tokens.get(col_ix - 2)?.clone(),
            _ => line.duration_now(),
        })
    }

    /// The cell's text, for copying.
    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        self.cell_label(row_ix, col_ix, cx)
            .map(|label| label.to_string())
            .unwrap_or_default()
    }
}

//! Transcript row rendering: messages, thinking, tool groups and details, and run summaries.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::Diff;
use gpui_kit::component::label::Label;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    TestSupportExt as _, Window, div, img, prelude::FluentBuilder as _,
    relative, rems,
};

use super::{MarkdownPart, SessionView};
use crate::projection::{
    CommandKind, CommandMeta, CommandStatus, CompactionReason, CompactionRow,
    ErrorRow, MailRow, MessageOrigin, MessageRow, Role, RowKey, ToolCategory,
    ToolRow, ToolStatus, TranscriptRow,
};
use crate::transcript::{GroupPosition, LayoutRow, RunSummary, RunUsage};
use crate::ui::format::{Elapsed, TokenCount, single_line};

/// What a summary card shows: its pill and the Markdown it expands to.
#[derive(typed_builder::TypedBuilder)]
struct Card<'a> {
    /// Row whose card this is; also keys its open state.
    key: RowKey,
    /// Pill icon.
    icon: IconName,
    /// Pill text.
    title: String,
    /// A muted line above the Markdown when expanded.
    #[builder(default)]
    detail: Option<String>,
    /// The expanded body.
    markdown: &'a str,
}

/// Execute output lines shown before "Full output" is chosen.
const OUTPUT_TAIL_LINES: usize = 20;

impl ToolCategory {
    /// Lucide icon of the category (spec §7.2).
    fn icon(self) -> IconName {
        match self {
            ToolCategory::Read => IconName::FileText,
            ToolCategory::Edit => IconName::Pencil,
            // Lucide 1.43 renamed `trash-2` to `trash`.
            ToolCategory::Delete => IconName::Trash,
            ToolCategory::Move => IconName::ArrowRightLeft,
            ToolCategory::Search => IconName::Search,
            ToolCategory::Execute => IconName::SquareTerminal,
            ToolCategory::Fetch => IconName::Globe,
            ToolCategory::Think => IconName::Brain,
            ToolCategory::Other => IconName::Wrench,
        }
    }
}

impl SessionView {
    /// Renders layout row `ix`, reading its projection index from the current layout.
    pub(in crate::ui) fn render_row(
        &self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let Some(row) = self.layout.row(ix) else {
            return div().into_any_element();
        };
        let last = ix + 1 == self.layout.len();
        let id = row.element_id();
        let (content, joins_next) = match row {
            LayoutRow::RunSummary {
                anchor,
                summary,
                expanded,
            } => (
                self.render_run_summary(anchor, summary, *expanded, cx),
                false,
            ),
            LayoutRow::Row { index, group, .. } => {
                let Some(handle) = self.handle() else {
                    return div().into_any_element();
                };
                match handle.read(cx).projection().row(*index) {
                    Some(TranscriptRow::Message(message)) => {
                        (self.render_message(message, cx), false)
                    }
                    Some(TranscriptRow::Tool(tool)) => (
                        self.render_tool(tool, *group, cx),
                        matches!(
                            group,
                            Some(GroupPosition::First | GroupPosition::Middle)
                        ),
                    ),
                    Some(TranscriptRow::Compaction(row)) => {
                        (self.render_compaction(row, cx), false)
                    }
                    Some(TranscriptRow::Error(row)) => {
                        (Self::render_error(row, cx), false)
                    }
                    Some(TranscriptRow::Mail(row)) => {
                        (Self::render_mail(row, cx), false)
                    }
                    None => return div().into_any_element(),
                }
            }
            LayoutRow::Usage { usage, .. } => {
                (self.render_usage(usage, cx), false)
            }
        };
        div()
            .id(id)
            .test_support()
            .w_full()
            .flex()
            .justify_center()
            // Tool rows inside one group touch, so the group reads as one box.
            .when(!joins_next && !last, |this| this.pb_3())
            .child(div().w_full().max_w(rems(42.5)).child(content))
            .into_any_element()
    }

    /// One message: a user bubble, a user-run command, or the assistant's answer.
    fn render_message(
        &self,
        message: &MessageRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        if let Some(command) = &message.command {
            return self.render_command_card(message, command, cx);
        }
        match (message.role, message.origin) {
            (_, MessageOrigin::BranchSummary) => self.render_card(
                Card::builder()
                    .key(RowKey::Message(message.message_id.clone()))
                    .icon(IconName::GitBranch)
                    .title(
                        rust_i18n::t!("Transcript.branch_summary").into_owned(),
                    )
                    .markdown(&message.text)
                    .build(),
                cx,
            ),
            (_, MessageOrigin::UserBash) => self.render_command(message, cx),
            (Role::User, _) => self.render_user(message, cx),
            (Role::Assistant, _) => self.render_assistant(message, cx),
        }
    }

    /// Right-aligned muted bubble with image thumbnails above the text.
    fn render_user(
        &self,
        message: &MessageRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = &message.message_id;
        let thumbnails: Vec<AnyElement> = (0..message.images.len())
            .filter_map(|n| {
                let image = self.images.get(&(id.clone(), n))?;
                Some(self.render_thumbnail(
                    SharedString::from(format!("image:{id}:{n}")),
                    image,
                    cx,
                ))
            })
            .collect();
        h_flex()
            .w_full()
            .justify_end()
            .child(
                v_flex()
                    .max_w(relative(0.78))
                    .gap_2()
                    .px_3p5()
                    .py_2()
                    .bg(theme.muted)
                    .rounded(theme.radius_lg)
                    .when(!thumbnails.is_empty(), |this| {
                        this.child(
                            h_flex().flex_wrap().gap_2().children(thumbnails),
                        )
                    })
                    .when_some(
                        self.markdown.get(id, MarkdownPart::Body),
                        |this, state| this.child(TextView::new(state)),
                    ),
            )
            .into_any_element()
    }

    /// A clickable image thumbnail that opens the full image in a dialog.
    fn render_thumbnail(
        &self,
        id: SharedString,
        image: &Arc<gpui_kit::Image>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let radius = cx.theme().radius;
        let full = Arc::clone(image);
        div()
            .id(id)
            .test_support()
            .cursor_pointer()
            .child(
                img(Arc::clone(image))
                    .size(rems(6.))
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover),
            )
            .on_click(move |_: &ClickEvent, window, cx| {
                let full = Arc::clone(&full);
                window.open_dialog(cx, move |dialog, _, _| {
                    dialog.child(
                        img(Arc::clone(&full))
                            .w_full()
                            .max_h(rems(36.))
                            .object_fit(ObjectFit::Contain),
                    )
                });
            })
            .into_any_element()
    }

    /// A command the user ran directly, drawn like a one-row execute tool.
    fn render_command(
        &self,
        message: &MessageRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = message.message_id.clone();
        let open = self.open_commands.contains(&id);
        let first_line = single_line(&message.text).into_owned();
        v_flex()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .overflow_hidden()
            .child(
                h_flex()
                    .id("header")
                    .h(rems(2.125))
                    .px_3()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Label::new(
                            rust_i18n::t!("Transcript.you_ran").into_owned(),
                        )
                        .flex_none(),
                    )
                    .child(
                        Label::new(first_line)
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .font_weight(
                                crate::theme::FontWeights::get(cx).code,
                            )
                            .text_xs(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.toggle_command(id.clone(), cx)
                        },
                    )),
            )
            .when(open, |this| {
                this.child(
                    Label::new(message.text.clone())
                        .border_t_1()
                        .border_color(theme.border)
                        .px_3()
                        .py_2()
                        .font_family(theme.mono_font_family.clone())
                        .font_weight(crate::theme::FontWeights::get(cx).code)
                        .text_xs(),
                )
            })
            .into_any_element()
    }

    /// The assistant's answer: a thinking fold, the Markdown body, and a streaming cursor.
    fn render_assistant(
        &self,
        message: &MessageRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = &message.message_id;
        let thinking_open = self.open_thinking.contains(id);
        let header = (!message.reasoning.is_empty()).then(|| {
            let label = match message.duration_ms {
                Some(ms) => rust_i18n::t!(
                    "Transcript.thinking_for",
                    elapsed = Elapsed(ms).label().as_ref()
                ),
                None => rust_i18n::t!("Transcript.thinking"),
            }
            .into_owned();
            let toggle = id.clone();
            h_flex()
                .id(SharedString::from(format!("thinking:{id}")))
                .test_support()
                .gap_1p5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .child(
                    Icon::new(if thinking_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .small(),
                )
                .child(Icon::new(IconName::Brain).small())
                // Still thinking: nothing answered yet, so the label shimmers.
                .map(|this| {
                    if message.streaming && message.text.is_empty() {
                        this.child(ShimmerText::new(
                            rust_i18n::t!("Transcript.thinking_now")
                                .into_owned(),
                        ))
                    } else {
                        // `Label` paints foreground unless told otherwise.
                        this.child(
                            Label::new(label)
                                .text_color(theme.muted_foreground),
                        )
                    }
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_thinking(toggle.clone(), cx)
                }))
        });
        v_flex()
            .w_full()
            .gap_2()
            .children(header)
            .when_some(
                self.markdown
                    .get(id, MarkdownPart::Thinking)
                    .filter(|_| thinking_open),
                |this, state| {
                    this.child(
                        TextView::new(state)
                            .text_sm()
                            .text_color(theme.muted_foreground),
                    )
                },
            )
            .when_some(
                self.markdown.get(id, MarkdownPart::Body),
                |this, state| this.child(TextView::new(state)),
            )
            .when(message.streaming, |this| {
                this.child(Label::new("▍").text_color(theme.muted_foreground))
            })
            .when_some(
                message
                    .assistant
                    .as_ref()
                    .and_then(|assistant| assistant.error.as_deref()),
                |this, error| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .bg(theme.danger.opacity(0.08))
                            .text_color(theme.danger)
                            .child(
                                Label::new(
                                    rust_i18n::t!("Transcript.model_failed")
                                        .into_owned(),
                                )
                                .text_sm()
                                .font_semibold()
                                .text_color(theme.danger),
                            )
                            .child(
                                Label::new(error.to_owned())
                                    .text_sm()
                                    .text_color(theme.danger),
                            ),
                    )
                },
            )
            .into_any_element()
    }

    /// One tool row of a group box; the group position picks its corners and borders.
    fn render_tool(
        &self,
        tool: &ToolRow,
        group: Option<GroupPosition>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = tool.tool_call_id.clone();
        let open = self.open_tools.contains(&id);
        let category = tool.category();
        // The row has a fixed height, so a multi-line command shows its first line.
        let target =
            single_line(tool.target().unwrap_or(&tool.title)).into_owned();
        let radius = theme.radius;
        let header = h_flex()
            .id("header")
            .h(rems(2.125))
            .px_3()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .child(
                Icon::new(category.icon())
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(Label::new(category.verb()).flex_none())
            .child(
                Label::new(target)
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .font_weight(crate::theme::FontWeights::get(cx).code)
                    .text_xs(),
            )
            .child(self.render_tool_status(tool, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_tool(id.clone(), cx)
            }));
        // Every row draws its sides and top edge; past the first row that top edge is the hairline.
        v_flex()
            .border_l_1()
            .border_r_1()
            .border_t_1()
            .border_color(theme.border)
            .overflow_hidden()
            .map(|this| match group.unwrap_or(GroupPosition::Only) {
                GroupPosition::Only => this.border_b_1().rounded(radius),
                GroupPosition::First => {
                    this.rounded_tl(radius).rounded_tr(radius)
                }
                GroupPosition::Middle => this,
                GroupPosition::Last => {
                    this.border_b_1().rounded_bl(radius).rounded_br(radius)
                }
            })
            .child(header)
            .when(open, |this| {
                this.child(
                    div()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(self.render_tool_detail(tool, cx)),
                )
            })
            .into_any_element()
    }

    /// The trailing status of a tool row.
    fn render_tool_status(
        &self,
        tool: &ToolRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let status = h_flex().flex_none().gap_1p5().text_xs().text_color(muted);
        let check = || {
            Icon::new(IconName::Check)
                .xsmall()
                .text_color(theme.success)
        };
        match tool.status {
            ToolStatus::Running => status
                .child(Spinner::new().xsmall())
                .when_some(tool.timestamp_ms, |this, started| {
                    let now = crate::now_ms();
                    this.child(
                        Label::new(
                            Elapsed(now.saturating_sub(started)).label(),
                        )
                        .text_color(muted),
                    )
                }),
            ToolStatus::Completed => {
                match (tool.category(), tool.diff_stats()) {
                    (ToolCategory::Edit, Some((added, removed))) => status
                        .child(
                            Label::new(format!("+{added}"))
                                .text_color(theme.success),
                        )
                        .child(
                            Label::new(format!("\u{2212}{removed}"))
                                .text_color(theme.danger),
                        ),
                    (ToolCategory::Read, _) => status.child(check()).child(
                        Label::new(
                            rust_i18n::t!(
                                "Tool.lines",
                                count = tool.line_count()
                            )
                            .into_owned(),
                        )
                        .text_color(muted),
                    ),
                    _ => status.child(check()),
                }
            }
            ToolStatus::Failed => status
                .text_color(theme.danger)
                .child(Icon::new(IconName::CircleX).xsmall())
                .child(
                    Label::new(rust_i18n::t!("Tool.failed").into_owned())
                        .text_color(theme.danger),
                ),
            ToolStatus::Cancelled => status.child(
                Label::new(rust_i18n::t!("Tool.cancelled").into_owned())
                    .text_color(muted),
            ),
            ToolStatus::Blocked => {
                status.child(Icon::new(IconName::Ban).xsmall()).child(
                    Tag::danger()
                        .xsmall()
                        .child(rust_i18n::t!("Tool.blocked").into_owned()),
                )
            }
            ToolStatus::Pending => status.child(
                Label::new(rust_i18n::t!("Tool.pending").into_owned())
                    .text_color(muted),
            ),
        }
        .into_any_element()
    }

    /// Expanded tool details: the diff of an edit, the output of a command, or arguments and output.
    fn render_tool_detail(
        &self,
        tool: &ToolRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = &tool.tool_call_id;
        let mono = || {
            v_flex()
                .px_3()
                .py_2()
                .font_family(theme.mono_font_family.clone())
                .font_weight(crate::theme::FontWeights::get(cx).code)
                .text_xs()
        };
        // One element per line, so long output never shapes as one huge text run.
        let line = |text: &str| Label::new(text.to_string());
        match tool.category() {
            ToolCategory::Edit => match self.diffs.get(id) {
                Some(state) => Diff::new(state)
                    .h(self.diff_height(tool, cx))
                    .into_any_element(),
                None => mono()
                    .child(Label::new(
                        tool.patch
                            .clone()
                            .unwrap_or_else(|| tool.output.clone()),
                    ))
                    .into_any_element(),
            },
            ToolCategory::Execute => {
                let total = tool.line_count();
                let full = self.full_output.contains(id);
                let skip = if full {
                    0
                } else {
                    total.saturating_sub(OUTPUT_TAIL_LINES)
                };
                let toggle = id.clone();
                // The row shows one line of a multi-line command; the details show all of it.
                let command = tool
                    .target()
                    .filter(|command| command.trim().lines().count() > 1);
                mono()
                    .when_some(command, |this, command| {
                        this.child(v_flex().pb_2().children(
                            command.trim().lines().map(|text| {
                                line(text).text_color(theme.muted_foreground)
                            }),
                        ))
                    })
                    .children(tool.output.lines().skip(skip).map(line))
                    // Where the untruncated output was saved, under the output.
                    .when_some(tool.footer.as_deref(), |this, footer| {
                        this.child(div().pt_1().child(
                            line(footer).text_color(theme.muted_foreground),
                        ))
                    })
                    .when(total > OUTPUT_TAIL_LINES, |this| {
                        this.child(
                            div().pt_1().child(
                                Button::new(SharedString::from(format!(
                                    "full-output:{id}"
                                )))
                                .ghost()
                                .small()
                                .label(
                                    if full {
                                        rust_i18n::t!("Tool.show_less")
                                    } else {
                                        rust_i18n::t!("Tool.full_output")
                                    }
                                    .into_owned(),
                                )
                                .on_click(
                                    cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            this.toggle_full_output(
                                                toggle.clone(),
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                            ),
                        )
                    })
                    .into_any_element()
            }
            _ => mono()
                .gap_2()
                .when_some(tool.raw_input.as_ref(), |this, input| {
                    this.child(Label::new(
                        serde_json::to_string_pretty(input).unwrap_or_default(),
                    ))
                })
                .when(!tool.output.is_empty(), |this| {
                    this.child(v_flex().children(
                        tool.output.lines().take(OUTPUT_TAIL_LINES).map(line),
                    ))
                })
                .into_any_element(),
        }
    }

    /// Height of an edit's diff: its rows at the theme's monospace size, at most 20 rem.
    ///
    /// The diff lays its rows out in a virtual list that fills its parent, so it
    /// needs a definite height inside the auto-height transcript row.
    /// ponytail: counts patch lines rather than presented rows; ask `DiffState`
    /// for its row count if gpui-kit exposes one.
    fn diff_height(
        &self,
        tool: &ToolRow,
        cx: &Context<'_, Self>,
    ) -> gpui_kit::Pixels {
        let theme = cx.theme();
        // `---`/`+++` are not drawn as rows; the file header row is about two rows tall.
        let rows = tool.patch.as_deref().map_or(0, |patch| {
            patch
                .lines()
                .filter(|line| {
                    !line.starts_with("--- ") && !line.starts_with("+++ ")
                })
                .count()
        }) + 2;
        // Rows are 1.6 × the monospace size; the theme font size is the rem.
        let row = f32::from(theme.mono_font_size) * 1.6;
        gpui_kit::px((row * rows as f32).min(f32::from(theme.font_size) * 20.))
    }

    /// A finished run's summary line; clicking folds or unfolds its tools.
    fn render_run_summary(
        &self,
        anchor: &RowKey,
        summary: &RunSummary,
        expanded: bool,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let anchor = anchor.clone();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id("summary")
            .gap_1p5()
            .text_sm()
            .text_color(muted)
            .cursor_pointer()
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small(),
            )
            .child(Label::new(summary.label()).text_sm().text_color(muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_run(anchor.clone(), cx)
            }))
            .into_any_element()
    }

    /// A centered pill that expands to Markdown: compaction and branch summaries.
    fn render_card(
        &self,
        card: Card<'_>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let Card {
            key,
            icon,
            title,
            detail,
            markdown,
        } = card;
        let theme = cx.theme();
        let open = self.open_cards.contains(&key);
        let toggle = key.clone();
        v_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(
                h_flex()
                    .id(SharedString::from(format!("card:{key}")))
                    .gap_1p5()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(Icon::new(icon).small())
                    .child(
                        Label::new(title)
                            .text_sm()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .small(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.toggle_card(toggle.clone(), cx)
                        },
                    )),
            )
            .when(open, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .px_4()
                        .py_3()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .when_some(detail, |this, detail| {
                            this.child(
                                Label::new(detail)
                                    .text_sm()
                                    .text_color(theme.muted_foreground),
                            )
                        })
                        .child(TextView::markdown(
                            SharedString::from(format!("card-body:{key}")),
                            SharedString::from(markdown.to_owned()),
                        )),
                )
            })
            .into_any_element()
    }

    /// A completed compaction: `Context compacted · 182k`, expanding to its summary.
    fn render_compaction(
        &self,
        row: &CompactionRow,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let title = match row.tokens_before {
            Some(tokens) => rust_i18n::t!(
                "Transcript.compacted",
                tokens = TokenCount(tokens).label().as_ref()
            ),
            None => rust_i18n::t!("Transcript.compacted_plain"),
        }
        .into_owned();
        let reason = match row.reason {
            CompactionReason::Manual => {
                rust_i18n::t!("Transcript.compaction_reason_manual")
            }
            CompactionReason::Threshold => {
                rust_i18n::t!("Transcript.compaction_reason_threshold")
            }
            CompactionReason::Overflow => {
                rust_i18n::t!("Transcript.compaction_reason_overflow")
            }
        }
        .into_owned();
        let detail = match row.elapsed_ms {
            Some(ms) => format!("{reason} · {}", Elapsed(ms).label()),
            None => reason,
        };
        self.render_card(
            Card::builder()
                .key(RowKey::Compaction(row.entry_id.clone()))
                .icon(IconName::GitCommitHorizontal)
                .title(title)
                .detail(Some(detail))
                .markdown(&row.summary)
                .build(),
            cx,
        )
    }

    /// A slash command message: `/name`, its source, kind, status, and body.
    fn render_command_card(
        &self,
        message: &MessageRow,
        command: &CommandMeta,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let source = command.source.label().into_owned();
        let kind = match command.kind {
            CommandKind::Invocation => rust_i18n::t!("Command.kind_invocation"),
            CommandKind::Output => rust_i18n::t!("Command.kind_output"),
            CommandKind::Expansion => rust_i18n::t!("Command.kind_expansion"),
        }
        .into_owned();
        let status = command.status.map(|status| match status {
            CommandStatus::Succeeded => {
                (rust_i18n::t!("Command.status_succeeded"), theme.success)
            }
            CommandStatus::Failed => {
                (rust_i18n::t!("Command.status_failed"), theme.danger)
            }
            CommandStatus::Cancelled => (
                rust_i18n::t!("Command.status_cancelled"),
                theme.muted_foreground,
            ),
        });
        // An invocation's text repeats `/name`; only its arguments are new.
        let prefix = format!("/{}", command.name);
        let body = match command.kind {
            CommandKind::Output => message.text.as_str(),
            _ => message
                .text
                .strip_prefix(prefix.as_str())
                .unwrap_or(&message.text)
                .trim(),
        };
        let align_end = message.role == Role::User;
        v_flex()
            .w_full()
            .when(align_end, |this| this.items_end())
            .child(
                v_flex()
                    .max_w(relative(0.78))
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_sm()
                            .child(
                                Icon::new(IconName::SquareSlash)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                Label::new(prefix.clone())
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .font_weight(
                                        crate::theme::FontWeights::get(cx).code,
                                    ),
                            )
                            .child(
                                Tag::secondary()
                                    .small()
                                    .child(Label::new(source).text_xs()),
                            )
                            .child(
                                Label::new(kind)
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            )
                            .when_some(status, |this, (status, color)| {
                                this.child(
                                    Label::new(status.into_owned())
                                        .text_xs()
                                        .text_color(color),
                                )
                            }),
                    )
                    .when(!body.is_empty(), |this| {
                        this.child(TextView::markdown(
                            SharedString::from(format!(
                                "command-body:{}",
                                message.message_id
                            )),
                            SharedString::from(body.to_owned()),
                        ))
                    }),
            )
            .into_any_element()
    }

    /// The muted usage line under a finished run's last reply.
    fn render_usage(
        &self,
        usage: &RunUsage,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        // The model option names models `provider/model`; its choice carries the display name.
        let name = usage.model.as_ref().and_then(|(provider, model)| {
            let handle = self.handle()?.read(cx);
            let option = handle.projection().controls().config("model")?;
            let value = format!("{provider}/{model}");
            option
                .choices
                .iter()
                .find(|choice| choice.value == value)
                .map(|choice| choice.name.clone())
        });
        let muted = cx.theme().muted_foreground;
        Label::new(usage.label(name.as_deref()))
            .text_xs()
            .text_color(muted)
            .into_any_element()
    }

    /// A message between agents: a compact card with the sender and what it sent.
    fn render_mail(row: &MailRow, cx: &Context<'_, Self>) -> AnyElement {
        let theme = cx.theme();
        let kind = match row.kind.as_str() {
            "new_task" => rust_i18n::t!("Transcript.mail_task"),
            "result" => rust_i18n::t!("Transcript.mail_result"),
            _ => rust_i18n::t!("Transcript.mail_message"),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Bot).small())
                    .child(Label::new(row.author.clone()).text_xs())
                    .child(Label::new(kind.into_owned()).text_xs()),
            )
            .child(Label::new(row.payload.clone()).text_sm())
            .into_any_element()
    }

    /// An inline danger note for a failed extension handler.
    fn render_error(row: &ErrorRow, cx: &Context<'_, Self>) -> AnyElement {
        let danger = cx.theme().danger;
        h_flex()
            .gap_1p5()
            .text_sm()
            .text_color(danger)
            .child(Icon::new(IconName::CircleAlert).small())
            .when(!row.source.is_empty(), |this| {
                this.child(
                    Label::new(row.source.clone())
                        .text_sm()
                        .font_semibold()
                        .text_color(danger),
                )
            })
            .child(Label::new(row.message.clone()).text_sm().text_color(danger))
            .into_any_element()
    }
}

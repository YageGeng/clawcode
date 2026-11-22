//! The sections of the record detail: summary, timing, usage, the request's
//! prompt, tools, and options, a tool's arguments and result, and raw JSON.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::label::Label;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, Theme,
};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, rems,
};
use protocol::{
    AssistantRecord, ContentHash, ModelUsage, RequestContent, RequestRecord,
    ToolDefinition, ToolRecord, TrajectoryRecord, TrajectoryRecordKind,
};

use super::record::{PromptMode, RecordSection, RecordView};
use crate::trajectory::{RecordText as _, TrajectoryModel};
use crate::ui::{ContentState, Elapsed, TokenCount};

/// The request and its output, whichever of the two is selected.
pub(super) struct RequestPair<'a> {
    pub(super) request: Option<&'a RequestRecord>,
    assistant: Option<&'a AssistantRecord>,
}

impl<'a> RequestPair<'a> {
    /// Finds the other half of a request or assistant record.
    pub(super) fn of(
        model: &'a TrajectoryModel,
        record: &'a TrajectoryRecord,
    ) -> Self {
        let other_id = match &record.kind {
            TrajectoryRecordKind::Request(_) => {
                record.id.strip_prefix("request:").map(str::to_owned)
            }
            TrajectoryRecordKind::Assistant(_) => {
                Some(format!("request:{}", record.id))
            }
            _ => None,
        };
        let other = other_id.and_then(|id| model.record(&id));
        let mut pair = Self {
            request: None,
            assistant: None,
        };
        for half in [Some(record), other].into_iter().flatten() {
            match &half.kind {
                TrajectoryRecordKind::Request(request) => {
                    pair.request = Some(request)
                }
                TrajectoryRecordKind::Assistant(assistant) => {
                    pair.assistant = Some(assistant)
                }
                _ => {}
            }
        }
        pair
    }
}

/// Result lines the summary shows before pointing to the Result section.
const RESULT_PREVIEW_LINES: usize = 12;

/// Monospace text in a quiet box, line breaks kept.
fn code_block(text: String, theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .child(text)
        .into_any_element()
}

/// One argument of a tool call: its name, then a short value on the same
/// line or a long or multi-line one in a code box below.
fn argument(key: &str, value: &serde_json::Value, theme: &Theme) -> AnyElement {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let name = Label::new(key.to_owned())
        .text_xs()
        .text_color(theme.muted_foreground)
        .font_family(theme.mono_font_family.clone());
    if text.contains('\n') || text.chars().count() > INLINE_ARGUMENT_CHARS {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(name)
            .child(code_block(text, theme))
            .into_any_element()
    } else {
        div()
            .flex()
            .items_baseline()
            .gap_2()
            .child(name)
            .child(
                Label::new(text)
                    .text_sm()
                    .font_family(theme.mono_font_family.clone()),
            )
            .into_any_element()
    }
}

/// Longest argument value kept on its name's line.
const INLINE_ARGUMENT_CHARS: usize = 48;

/// One `label  value` line whose value is an element.
fn fact_with(
    label: &str,
    value: impl IntoElement,
    muted: gpui_kit::Hsla,
) -> AnyElement {
    div()
        .flex()
        .gap_3()
        .py_0p5()
        .child(
            div().w(rems(9.)).flex_none().child(
                Label::new(rust_i18n::t!(label).into_owned())
                    .text_sm()
                    .text_color(muted),
            ),
        )
        .child(value)
        .into_any_element()
}

/// One `label  value` line.
fn fact(
    label: &str,
    value: impl Into<SharedString>,
    muted: gpui_kit::Hsla,
) -> AnyElement {
    div()
        .flex()
        .gap_3()
        .py_0p5()
        .child(
            div().w(rems(9.)).flex_none().child(
                Label::new(rust_i18n::t!(label).into_owned())
                    .text_sm()
                    .text_color(muted),
            ),
        )
        .child(Label::new(value.into()).text_sm())
        .into_any_element()
}

impl RecordView {
    /// The body of the current section.
    pub(super) fn render_section(
        &mut self,
        record: &TrajectoryRecord,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        match self.section {
            RecordSection::Summary => self.render_summary(record, cx),
            RecordSection::Timing => self.render_timing(record, cx),
            RecordSection::Usage => self.render_usage(record, cx),
            RecordSection::SystemPrompt => {
                self.render_prompt(record, window, cx)
            }
            RecordSection::Tools => self.render_tools(record, cx),
            RecordSection::Options => {
                let options = {
                    let trajectory = self.trajectory.read(cx);
                    RequestPair::of(trajectory.model(), record)
                        .request
                        .map(|request| request.snapshot.options.clone())
                };
                match options {
                    Some(options) => self.render_code(
                        record,
                        "json",
                        || pretty(&options),
                        window,
                        cx,
                    ),
                    None => div().into_any_element(),
                }
            }
            RecordSection::Arguments | RecordSection::Result => {
                let TrajectoryRecordKind::Tool(tool) = &record.kind else {
                    return div().into_any_element();
                };
                match (self.section, &tool.result_text) {
                    (RecordSection::Arguments, _) => self.render_code(
                        record,
                        "json",
                        || pretty(&tool.arguments),
                        window,
                        cx,
                    ),
                    (_, Some(result)) => self.render_code(
                        record,
                        "text",
                        || result.clone(),
                        window,
                        cx,
                    ),
                    (_, None) => Label::new(
                        rust_i18n::t!("Record.no_result").into_owned(),
                    )
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
                }
            }
            RecordSection::Raw => {
                self.render_code(record, "json", || pretty(record), window, cx)
            }
        }
    }

    /// What the record says, laid out by its kind: a tool's call and
    /// result, a request's thinking and answer, or a message as Markdown.
    fn render_summary(
        &self,
        record: &TrajectoryRecord,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let markdown = |text: &str| {
            TextView::markdown(
                SharedString::from(format!("record-md:{}", record.id)),
                SharedString::from(text.to_owned()),
            )
            .into_any_element()
        };
        match &record.kind {
            TrajectoryRecordKind::Tool(tool) => {
                self.render_tool_summary(record, tool, cx)
            }
            TrajectoryRecordKind::Request(_)
            | TrajectoryRecordKind::Assistant(_) => {
                self.render_answer_summary(record, cx)
            }
            TrajectoryRecordKind::User(user) => markdown(&user.text),
            TrajectoryRecordKind::Compaction { summary, .. }
            | TrajectoryRecordKind::BranchSummary { summary } => {
                markdown(summary)
            }
            TrajectoryRecordKind::Command(command) => {
                code_block(command.text.clone(), &theme)
            }
            _ => Label::new(record.text()).text_sm().into_any_element(),
        }
    }

    /// A small muted title above `body`; a click on the title folds the
    /// part away or unfolds it.
    fn titled(
        &self,
        title: &'static str,
        body: impl IntoElement,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let open = !self.folded.contains(title);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(format!("record-part:{title}")))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_color(muted)
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(
                        Label::new(rust_i18n::t!(title).into_owned())
                            .text_xs()
                            .text_color(muted),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.folded.remove(title) {
                            this.folded.insert(title);
                        }
                        cx.notify();
                    })),
            )
            .when(open, |this| this.child(body))
            .into_any_element()
    }

    /// A tool call: name, status, and duration; its arguments one per line,
    /// long values in a code box; and the first lines of its result.
    fn render_tool_summary(
        &self,
        record: &TrajectoryRecord,
        tool: &ToolRecord,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = &cx.theme().clone();
        let muted = theme.muted_foreground;
        let (status, color) =
            match (tool.is_error, tool.cancelled, &tool.result_text) {
                (true, _, _) => ("Record.status_failed", theme.danger),
                (_, true, _) => ("Record.status_cancelled", muted),
                (_, _, None) => ("Record.status_running", muted),
                _ => ("Record.status_done", theme.success),
            };
        let duration =
            record
                .started_at_ms
                .zip(record.ended_at_ms)
                .map(|(start, end)| {
                    Elapsed(end.get().saturating_sub(start.get())).precise()
                });
        let facts = div()
            .flex()
            .flex_col()
            .child(fact_with(
                "Record.tool",
                Label::new(tool.name.clone())
                    .text_sm()
                    .font_family(theme.mono_font_family.clone()),
                muted,
            ))
            .child(fact_with(
                "Record.status",
                Label::new(rust_i18n::t!(status).into_owned())
                    .text_sm()
                    .text_color(color),
                muted,
            ))
            .children(
                duration
                    .map(|duration| fact("Record.duration", duration, muted)),
            );
        // An object's fields read one per line; anything else as JSON.
        let arguments: Vec<AnyElement> = match &tool.arguments {
            serde_json::Value::Object(fields) if !fields.is_empty() => fields
                .iter()
                .map(|(key, value)| argument(key, value, theme))
                .collect(),
            serde_json::Value::Object(_) | serde_json::Value::Null => {
                Vec::new()
            }
            other => vec![code_block(pretty(other), theme)],
        };
        let result = match &tool.result_text {
            Some(text) if !text.trim().is_empty() => {
                let lines: Vec<&str> = text.lines().collect();
                let shown = lines
                    .iter()
                    .take(RESULT_PREVIEW_LINES)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                let more = lines.len().saturating_sub(RESULT_PREVIEW_LINES);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(code_block(shown, theme))
                    .when(more > 0, |this| {
                        this.child(
                            Label::new(
                                rust_i18n::t!(
                                    "Record.result_more",
                                    count = more
                                )
                                .into_owned(),
                            )
                            .text_xs()
                            .text_color(muted),
                        )
                    })
                    .into_any_element()
            }
            Some(_) => {
                Label::new(rust_i18n::t!("Record.no_output").into_owned())
                    .text_sm()
                    .text_color(muted)
                    .into_any_element()
            }
            None => div().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(facts)
            .when(!arguments.is_empty(), |this| {
                this.child(self.titled(
                    "Record.section_arguments",
                    div().flex().flex_col().gap_1().children(arguments),
                    cx,
                ))
            })
            .when(tool.result_text.is_some(), |this| {
                this.child(self.titled("Record.section_result", result, cx))
            })
            .into_any_element()
    }

    /// A request's answer as Markdown, the tool calls it made (each selects
    /// its call), and its thinking, quoted and muted.
    fn render_answer_summary(
        &self,
        record: &TrajectoryRecord,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let trajectory = self.trajectory.read(cx);
        let pair = RequestPair::of(trajectory.model(), record);
        let Some(assistant) = pair.assistant else {
            return Label::new(record.text()).text_sm().into_any_element();
        };
        let calls: Vec<AnyElement> = assistant
            .tool_call_ids
            .iter()
            .map(|call| {
                let id = format!("tool:{call}");
                let target = id.clone();
                let summary = trajectory
                    .model()
                    .record(&id)
                    .map(|candidate| candidate.summary())
                    .unwrap_or_else(|| call.to_string());
                // A row rather than a Button: a Button's label cannot
                // shrink, so a long summary would overflow the panel
                // instead of ending in an ellipsis.
                div()
                    .id(SharedString::from(format!("record-call:{call}")))
                    .flex()
                    .items_center()
                    .gap_1()
                    .w_full()
                    .px_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .child(
                        Icon::new(LucideIcon::Wrench)
                            .xsmall()
                            .text_color(theme.yellow),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(Label::new(summary).text_sm()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.trajectory.update(cx, |trajectory, cx| {
                            trajectory.select(&target, cx)
                        });
                    }))
                    .into_any_element()
            })
            .collect();
        let (reasoning, text) =
            (assistant.reasoning.clone(), assistant.text.clone());
        let thinking =
            (!reasoning.is_empty()).then(|| {
                self.titled(
                    "Record.thinking",
                    div().pl_3().border_l_2().border_color(theme.border).child(
                        Label::new(reasoning).text_sm().text_color(muted),
                    ),
                    cx,
                )
            });
        let answer = (!text.is_empty()).then(|| {
            self.titled(
                "Record.answer",
                TextView::markdown(
                    SharedString::from(format!("record-md:{}", record.id)),
                    SharedString::from(text),
                ),
                cx,
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            // The answer and the calls lead; the thinking behind them,
            // often much longer, follows.
            .children(answer)
            .when(!calls.is_empty(), |this| {
                this.child(self.titled(
                    "Record.tool_calls",
                    div().flex().flex_col().items_start().children(calls),
                    cx,
                ))
            })
            .children(thinking)
            .into_any_element()
    }

    /// Send, first token, and last token times, with latency, generation,
    /// and throughput; a tool's start and duration.
    fn render_timing(
        &self,
        record: &TrajectoryRecord,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let trajectory = self.trajectory.read(cx);
        let pair = RequestPair::of(trajectory.model(), record);
        let mut facts = Vec::new();
        if let Some(assistant) = pair.assistant {
            let sent = assistant.requested_at_ms.get();
            let first = assistant.first_token_at_ms.get();
            let last = assistant.last_token_at_ms.get();
            let since = |at: u64| {
                format!("+{}", Elapsed(at.saturating_sub(sent)).precise())
            };
            let generation = last.saturating_sub(first);
            facts.push(fact("Record.sent", since(sent), muted));
            facts.push(fact("Record.first_token", since(first), muted));
            facts.push(fact("Record.last_token", since(last), muted));
            facts.push(fact(
                "Record.latency",
                Elapsed(first.saturating_sub(sent)).precise(),
                muted,
            ));
            facts.push(fact(
                "Record.generation",
                Elapsed(generation).precise(),
                muted,
            ));
            let throughput =
                match (assistant.usage.as_ref().output_tokens, generation) {
                    (Some(output), 1..) => rust_i18n::t!(
                        "Record.throughput_value",
                        value = format!(
                            "{:.1}",
                            output as f64 * 1000.0 / generation as f64
                        )
                    )
                    .into_owned(),
                    _ => rust_i18n::t!("Record.too_short").into_owned(),
                };
            facts.push(fact("Record.throughput", throughput, muted));
        } else if let Some(start) = record.started_at_ms {
            facts.push(fact("Record.started", format!("{start}"), muted));
            if let Some(end) = record.ended_at_ms {
                let duration =
                    Elapsed(end.get().saturating_sub(start.get())).precise();
                facts.push(fact("Record.duration", duration, muted));
            }
        }
        div().flex().flex_col().children(facts).into_any_element()
    }

    /// This request's tokens beside the session's so far.
    fn render_usage(
        &self,
        record: &TrajectoryRecord,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let trajectory = self.trajectory.read(cx);
        let model = trajectory.model();
        let pair = RequestPair::of(model, record);
        let request = pair
            .assistant
            .map(|assistant| assistant.usage)
            .unwrap_or_default();
        let total = pair
            .assistant
            .map(|assistant| model.usage_through(assistant.request_index))
            .unwrap_or(*model.usage_before());
        let column = |title: &str, usage: &ModelUsage| {
            let usage = usage.as_ref();
            let count = |value: Option<u64>| {
                value
                    .map(|n| TokenCount(n).label())
                    .unwrap_or_else(|| "—".into())
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .child(
                    Label::new(rust_i18n::t!(title).into_owned())
                        .text_xs()
                        .text_color(muted),
                )
                .child(fact("Record.input", count(usage.input_tokens), muted))
                .child(fact(
                    "Record.cache_read",
                    count(usage.cached_input_tokens),
                    muted,
                ))
                .child(fact("Record.output", count(usage.output_tokens), muted))
                .child(fact(
                    "Record.reasoning",
                    count(usage.reasoning_tokens),
                    muted,
                ))
        };
        div()
            .flex()
            .gap_4()
            .child(column("Record.this_request", &request))
            .child(column("Record.session_total", &total))
            .into_any_element()
    }

    /// The prompt in full, or as a diff against the previous request when it changed.
    fn render_prompt(
        &mut self,
        record: &TrajectoryRecord,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hash, changed, previous) = {
            let trajectory = self.trajectory.read(cx);
            let model = trajectory.model();
            let Some(request) = RequestPair::of(model, record).request else {
                return div().into_any_element();
            };
            let previous = model
                .previous_request(request.index)
                .map(|previous| previous.snapshot.system_prompt.clone());
            (
                request.snapshot.system_prompt.clone(),
                request.prompt_changed,
                previous,
            )
        };
        // A diff needs the previous prompt; when its request is not loaded,
        // only the full prompt is offered, not a "missing content" notice.
        let changed = changed && previous.is_some();
        if !changed {
            self.prompt_mode = PromptMode::Full;
        }
        let mode_switch = changed.then(|| {
            let toggle = |id: &'static str,
                          mode: PromptMode,
                          label: &str,
                          cx: &mut Context<'_, Self>| {
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .selected(self.prompt_mode == mode)
                    .label(rust_i18n::t!(label).to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.prompt_mode = mode;
                        cx.notify();
                    }))
            };
            div()
                .flex()
                .gap_1()
                .pb_2()
                .child(toggle(
                    "prompt-mode:full",
                    PromptMode::Full,
                    "Record.prompt_full",
                    cx,
                ))
                .child(toggle(
                    "prompt-mode:diff",
                    PromptMode::Diff,
                    "Record.prompt_diff",
                    cx,
                ))
        });
        let current = self.content(&hash, cx);
        let body = match (self.prompt_mode, current) {
            (PromptMode::Full, ContentState::Ready(content)) => self
                .render_code(
                    record,
                    // Shown as written: Markdown would hide its syntax.
                    "text",
                    || content.content.clone(),
                    window,
                    cx,
                ),
            (PromptMode::Diff, ContentState::Ready(content)) => {
                match previous.map(|previous| self.content(&previous, cx)) {
                    Some(ContentState::Ready(old)) => self.render_code(
                        record,
                        "diff",
                        || prompt_diff(&old, &content),
                        window,
                        cx,
                    ),
                    Some(state) => self.render_content_state(state, cx),
                    None => {
                        self.render_content_state(ContentState::Missing, cx)
                    }
                }
            }
            (_, state) => self.render_content_state(state, cx),
        };
        div()
            .flex()
            .flex_col()
            .children(mode_switch)
            .child(body)
            .into_any_element()
    }

    /// The tools of the request with what changed since the previous one;
    /// each tool shows its description and parameter schema.
    fn render_tools(
        &mut self,
        record: &TrajectoryRecord,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some((hash, added, removed)) = ({
            let trajectory = self.trajectory.read(cx);
            RequestPair::of(trajectory.model(), record)
                .request
                .map(|request| {
                    (
                        request.snapshot.tools.clone(),
                        request.tools_added.clone(),
                        request.tools_removed.clone(),
                    )
                })
        }) else {
            return div().into_any_element();
        };
        let changes = [
            ("Record.tools_added", added),
            ("Record.tools_removed", removed),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(key, names)| {
            Label::new(
                rust_i18n::t!(key, names = names.join(", ")).into_owned(),
            )
            .text_sm()
            .text_color(muted)
            .into_any_element()
        })
        .collect::<Vec<_>>();
        let tools = match self.content(&hash, cx) {
            ContentState::Ready(content) => {
                match serde_json::from_str::<Vec<ToolDefinition>>(
                    &content.content,
                ) {
                    Ok(tools) => tools
                        .into_iter()
                        .map(|tool| self.render_tool(tool, cx))
                        .collect::<Vec<_>>(),
                    Err(error) => vec![self.render_content_state(
                        ContentState::Failed(error.to_string()),
                        cx,
                    )],
                }
            }
            state => vec![self.render_content_state(state, cx)],
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(changes)
            .children(tools)
            .into_any_element()
    }

    /// One tool: its name opens or closes its description and schema.
    fn render_tool(
        &self,
        tool: ToolDefinition,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let open = self.open_tools.contains(&tool.name);
        let name = tool.name.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!(
                    "record-tool:{}",
                    tool.name
                )))
                .ghost()
                .xsmall()
                .selected(open)
                .label(tool.name.clone())
                .on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !this.open_tools.remove(&name) {
                            this.open_tools.insert(name.clone());
                        }
                        cx.notify();
                    },
                )),
            )
            .when(open, |this| {
                this.child(
                    Label::new(tool.description)
                        .text_sm()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(pretty(&tool.parameters)),
                )
            })
            .into_any_element()
    }

    /// Reads one content through the trajectory's shared cache.
    fn content(
        &self,
        hash: &ContentHash,
        cx: &mut Context<'_, Self>,
    ) -> ContentState {
        self.trajectory
            .update(cx, |trajectory, cx| trajectory.request_content(hash, cx))
    }

    /// A content still loading, missing, or failed.
    fn render_content_state(
        &self,
        state: ContentState,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let notice = |text: String, color| {
            Label::new(text)
                .text_sm()
                .text_color(color)
                .into_any_element()
        };
        match state {
            ContentState::Loading | ContentState::Ready(_) => {
                Skeleton::new().h_24().w_full().into_any_element()
            }
            ContentState::Missing => {
                notice(Self::missing_text(), theme.muted_foreground)
            }
            ContentState::Failed(error) => notice(
                rust_i18n::t!("Record.content_failed", error = error)
                    .into_owned(),
                theme.danger,
            ),
        }
    }

    /// Read-only text at its full height with a copy button, as a
    /// `language` code block. The text is built only when the record,
    /// section, or record end change.
    fn render_code(
        &mut self,
        record: &TrajectoryRecord,
        language: &str,
        text: impl FnOnce() -> String,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> AnyElement {
        // A record changes only when its tool result arrives, which ends it.
        let key = format!(
            "{}:{:?}:{:?}:{}",
            record.id, self.section, record.ended_at_ms, language
        );
        let (raw, markdown) = match &self.code {
            Some((shown, raw, markdown)) if *shown == key => {
                (raw.clone(), markdown.clone())
            }
            _ => {
                let raw = SharedString::from(text());
                let markdown = SharedString::from(fenced(&raw, language));
                self.code = Some((key, raw.clone(), markdown.clone()));
                (raw, markdown)
            }
        };
        // Copy sits on its own row: over the text it would hide a code
        // block's language label.
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(Clipboard::new("record-copy").value(raw)),
            )
            .child(TextView::markdown("record-code", markdown).selectable(true))
            .into_any_element()
    }
}

/// The unified diff from the `old` prompt to the `new` one, with three
/// lines of context around each change.
fn prompt_diff(old: &RequestContent, new: &RequestContent) -> String {
    similar::TextDiff::from_lines(&old.content, &new.content)
        .unified_diff()
        .context_radius(3)
        .header("a/system_prompt", "b/system_prompt")
        .to_string()
}

/// `text` as a Markdown code block in `language`, fenced with more
/// backticks than any run inside it so the text cannot close the block.
fn fenced(text: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}")
}

/// Pretty JSON of `value`.
fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

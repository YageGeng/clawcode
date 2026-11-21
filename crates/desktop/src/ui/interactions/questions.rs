//! The docked question panel: the agent's open questions, one at a time,
//! answered or skipped above the composer (spec §7.3).

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::Radio;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _,
};
use typed_builder::TypedBuilder;

use crate::backend::{BackendError, QuestionKey};
use crate::projection::AskedQuestion;
use crate::ui::session::SessionView;

/// The text sent for one question: picked options in display order, then free text.
#[derive(TypedBuilder)]
pub struct QuestionAnswer<'a> {
    /// The question's options.
    options: &'a [String],
    /// Indexes of the picked options, in any order.
    picks: &'a [usize],
    /// Free text typed under "Other", if chosen.
    #[builder(default)]
    other: Option<&'a str>,
    /// Whether several answers form a Markdown list.
    multi_select: bool,
}

impl QuestionAnswer<'_> {
    /// `"- a\n- b"` for several answers, the single answer otherwise; empty when blank.
    pub fn text(&self) -> String {
        let mut picks = self.picks.to_vec();
        picks.sort_unstable();
        let mut answers: Vec<&str> = picks
            .iter()
            .filter_map(|index| self.options.get(*index).map(String::as_str))
            .collect();
        if let Some(other) =
            self.other.map(str::trim).filter(|other| !other.is_empty())
        {
            answers.push(other);
        }
        if self.multi_select {
            answers
                .iter()
                .map(|answer| format!("- {answer}"))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            answers
                .first()
                .map(|answer| (*answer).to_owned())
                .unwrap_or_default()
        }
    }
}

/// What the user has chosen so far, per question, while the panel is open.
#[derive(typed_builder::TypedBuilder)]
pub struct QuestionDraft {
    /// Position of the shown question among the open ones.
    #[builder(default)]
    index: usize,
    /// Picked options, per question; absent means "first option".
    #[builder(default)]
    picks: HashMap<QuestionKey, Vec<usize>>,
    /// Questions whose "Other" choice is selected.
    #[builder(default)]
    other: HashSet<QuestionKey>,
    /// Free text typed under "Other", per question, for questions not shown now.
    #[builder(default)]
    text: HashMap<QuestionKey, String>,
    /// The question whose text the shared input holds now.
    #[builder(default)]
    input_key: Option<QuestionKey>,
}

impl QuestionDraft {
    /// Whether the typed text is part of the answer: "Other" is chosen, or the
    /// question offers no options and free text is the only answer.
    fn takes_text(&self, question: &AskedQuestion) -> bool {
        question.options.is_empty() || self.other.contains(&question.key)
    }

    /// Picks of a question; the first option is preselected but nothing is sent until "Send".
    fn picks(&self, question: &AskedQuestion) -> Vec<usize> {
        match self.picks.get(&question.key) {
            Some(picks) => picks.clone(),
            None if !question.options.is_empty()
                && !self.other.contains(&question.key) =>
            {
                vec![0]
            }
            None => Vec::new(),
        }
    }
}

impl SessionView {
    /// The question shown now and its position, clamped to the open ones.
    ///
    /// Only the shown question is cloned; this runs on every frame.
    fn shown_question(
        &self,
        cx: &Context<'_, Self>,
    ) -> Option<(usize, usize, AskedQuestion)> {
        let open = self.handle()?.read(cx).projection().open_questions();
        let count = open.len();
        let index = self.question.index.min(count.checked_sub(1)?);
        open.get(index)
            .map(|question| (index, count, (*question).clone()))
    }

    /// The answer the panel would send for `question` now.
    fn answer_text(
        &self,
        question: &AskedQuestion,
        cx: &Context<'_, Self>,
    ) -> String {
        let typed = self
            .question
            .takes_text(question)
            .then(|| self.question_input.read(cx).value().to_string());
        QuestionAnswer::builder()
            .options(&question.options)
            .picks(&self.question.picks(question))
            .other(typed.as_deref())
            .multi_select(question.multi_select)
            .build()
            .text()
    }

    /// Shows another question; the next frame swaps the typed text.
    fn show_question(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        self.question.index = index;
        cx.notify();
    }

    /// Gives the shared input to the question shown now, before it is drawn.
    ///
    /// The shown question changes on navigation, on skip and send, and when
    /// another client settles one; each time the text typed for the question
    /// left is kept under its key and the new question's own text is loaded.
    pub(in crate::ui) fn sync_question_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let shown =
            self.shown_question(cx).map(|(_, _, question)| question.key);
        if shown == self.question.input_key {
            return;
        }
        if let Some(left) = self.question.input_key.take() {
            let typed = self.question_input.read(cx).value().to_string();
            if typed.is_empty() {
                self.question.text.remove(&left);
            } else {
                self.question.text.insert(left, typed);
            }
        }
        let text = shown
            .as_ref()
            .and_then(|key| self.question.text.remove(key))
            .unwrap_or_default();
        self.question_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.question.input_key = shown;
    }

    /// Toggles option `index` (or replaces the pick of a single-choice question).
    fn pick(
        &mut self,
        question: &AskedQuestion,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let mut picks = self.question.picks(question);
        if question.multi_select {
            match picks.iter().position(|pick| *pick == index) {
                Some(position) => {
                    picks.remove(position);
                }
                None => picks.push(index),
            }
        } else {
            picks = vec![index];
            self.question.other.remove(&question.key);
        }
        self.question.picks.insert(question.key.clone(), picks);
        cx.notify();
    }

    /// Selects or clears the free-text "Other" choice; selecting it moves the cursor into its input.
    fn pick_other(
        &mut self,
        question: &AskedQuestion,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let key = question.key.clone();
        if question.multi_select {
            if !self.question.other.remove(&key) {
                self.question.other.insert(key.clone());
            }
            // An explicit choice replaces the preselected first option.
            self.question.picks.entry(key.clone()).or_default();
        } else {
            self.question.other.insert(key.clone());
            self.question.picks.insert(key.clone(), Vec::new());
        }
        if self.question.other.contains(&key) {
            self.question_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Sends the shown question's answer; it hides at once and returns if the send fails.
    fn send_answer(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((_, _, question)) = self.shown_question(cx) else {
            return;
        };
        let text = self.answer_text(&question, cx);
        let (Some(handle), Some(client)) =
            (self.handle().cloned(), self.client.clone())
        else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let key = question.key.clone();
        let session = handle.read(cx).id().clone();
        handle.update(cx, |handle, cx| {
            handle.controls_mut().mark_settling([key.clone()]);
            cx.notify();
        });
        // The answer is sent: an unsent copy must not come back with the question.
        self.question.text.remove(&key);
        self.question_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.spawn_in(window, async move |_, cx| {
            let result = client.answer_question(&session, &key, &text).await;
            let Err(error) = result else { return };
            // A queued answer withdrawn when the user stopped the run comes
            // back as cancelled: the question returns, without a warning.
            let message = match &error {
                BackendError::Cancelled => None,
                BackendError::QuestionClosed => {
                    Some(rust_i18n::t!("Question.closed"))
                }
                _ => Some(rust_i18n::t!("Question.send_failed")),
            };
            if message.is_some() {
                tracing::warn!(
                    "failed to answer question of session {}: {}",
                    session,
                    error
                );
            }
            let _ = cx.update(|window, cx| {
                handle.update(cx, |handle, cx| {
                    handle.controls_mut().unmark_settling(&[key]);
                    cx.notify();
                });
                let Some(message) = message else { return };
                window.push_notification(
                    Notification::warning(message.into_owned()),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Skips the shown question; it returns with a notice if the skip fails.
    fn skip_question(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((_, _, question)) = self.shown_question(cx) else {
            return;
        };
        let (Some(handle), Some(client)) =
            (self.handle().cloned(), self.client.clone())
        else {
            return;
        };
        let key = question.key;
        let session = handle.read(cx).id().clone();
        handle.update(cx, |handle, cx| {
            handle.controls_mut().mark_settling([key.clone()]);
            cx.notify();
        });
        cx.spawn_in(window, async move |_, cx| {
            let keys = [key];
            if let Err(error) = client.dismiss_questions(&session, &keys).await
            {
                tracing::warn!(
                    "failed to skip question of session {}: {}",
                    session,
                    error
                );
                let _ = cx.update(|window, cx| {
                    handle.update(cx, |handle, cx| {
                        handle.controls_mut().unmark_settling(&keys);
                        cx.notify();
                    });
                    window.push_notification(
                        Notification::warning(
                            rust_i18n::t!("Question.skip_failed").into_owned(),
                        ),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    /// The panel above the composer input; `None` without open questions.
    pub(in crate::ui) fn render_questions(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let (index, count, question) = self.shown_question(cx)?;
        let theme = cx.theme();
        let picks = self.question.picks(&question);
        let other = self.question.takes_text(&question);
        let sendable = !self.answer_text(&question, cx).is_empty();
        // The send button's text tone: gpui-kit's disabled tone when there is nothing to send.
        let send_text = if sendable {
            cx.theme().primary_foreground
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        };
        let view = cx.entity().downgrade();
        let options = question.options.iter().enumerate().map(|(n, option)| {
            let (view, asked) = (view.clone(), question.clone());
            let checked = picks.contains(&n);
            let id = SharedString::from(format!("question-option-{n}"));
            let click = move |_: &bool,
                              _: &mut Window,
                              cx: &mut gpui_kit::App| {
                let _ = view.update(cx, |this, cx| this.pick(&asked, n, cx));
            };
            if question.multi_select {
                Checkbox::new(id)
                    .label(option.clone())
                    .checked(checked)
                    .on_click(click)
                    .into_any_element()
            } else {
                Radio::new(id)
                    .label(option.clone())
                    .checked(checked)
                    .on_click(click)
                    .into_any_element()
            }
        });
        let other_choice = {
            let (view, asked) = (view.clone(), question.clone());
            let click = move |_: &bool,
                              window: &mut Window,
                              cx: &mut gpui_kit::App| {
                let _ = view
                    .update(cx, |this, cx| this.pick_other(&asked, window, cx));
            };
            let label = rust_i18n::t!("Question.other").into_owned();
            if question.multi_select {
                Checkbox::new("question-other")
                    .label(label)
                    .checked(other)
                    .on_click(click)
                    .into_any_element()
            } else {
                Radio::new("question-other")
                    .label(label)
                    .checked(other)
                    .on_click(click)
                    .into_any_element()
            }
        };
        Some(
            v_flex()
                .w_full()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Label::new(
                                rust_i18n::t!(
                                    "Question.header",
                                    i = index + 1,
                                    n = count
                                )
                                .into_owned(),
                            )
                            .text_xs()
                            .text_color(theme.muted_foreground),
                        )
                        .child(gpui_kit::div().flex_1())
                        .child(
                            Button::new("question-previous")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronLeft)
                                .disabled(index == 0)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.show_question(
                                            index.saturating_sub(1),
                                            cx,
                                        )
                                    },
                                )),
                        )
                        .child(
                            Button::new("question-next")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronRight)
                                .disabled(index + 1 >= count)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.show_question(index + 1, cx)
                                    },
                                )),
                        ),
                )
                .child(TextView::markdown(
                    SharedString::from(format!(
                        "question-title:{}:{}",
                        question.key.request_id, question.key.index
                    )),
                    SharedString::from(question.title.clone()),
                ))
                .when(question.multi_select, |this| {
                    this.child(
                        Label::new(
                            rust_i18n::t!("Question.multi").into_owned(),
                        )
                        .text_xs()
                        .text_color(theme.muted_foreground),
                    )
                })
                .children(options)
                .when(!question.options.is_empty(), |this| {
                    this.child(other_choice)
                })
                .when(other, |this| {
                    this.child(
                        div()
                            .id("question-input")
                            .test_support()
                            .child(Textarea::new(&self.question_input).small()),
                    )
                })
                .child(
                    h_flex()
                        .gap_1()
                        .justify_end()
                        .child(
                            Button::new("question-skip")
                                .ghost()
                                .small()
                                .child(
                                    Label::new(
                                        rust_i18n::t!("Question.skip")
                                            .into_owned(),
                                    )
                                    .text_xs(),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.skip_question(window, cx)
                                    },
                                )),
                        )
                        .child(
                            Button::new("question-send")
                                .primary()
                                .small()
                                .child(
                                    Label::new(
                                        rust_i18n::t!("Question.send")
                                            .into_owned(),
                                    )
                                    .text_xs()
                                    // `Label` would paint `foreground` on the
                                    // `primary` fill; follow the button's state.
                                    .text_color(send_text),
                                )
                                .disabled(!sendable)
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.send_answer(window, cx)
                                    },
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
}

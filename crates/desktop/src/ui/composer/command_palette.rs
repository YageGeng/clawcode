//! The slash command list above the composer input (spec §7.4): prefix
//! filtering, keyboard selection, and completion into the input.

use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, ClickEvent, Context, EntityInputHandler as _,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    prelude::FluentBuilder as _,
};

use super::Composer;
use crate::projection::{CommandSource, SlashCommand};

/// Commands whose name starts with `query`, ignoring case, in the agent's order.
pub fn filter_commands<'a>(
    commands: &'a [SlashCommand],
    query: &str,
) -> Vec<&'a SlashCommand> {
    let query = query.to_lowercase();
    commands
        .iter()
        .filter(|command| command.name.to_lowercase().starts_with(&query))
        .collect()
}

/// Palette state of one composer.
#[derive(Debug, Default)]
pub struct Palette {
    /// Commands the session offers.
    pub(in crate::ui) commands: Vec<SlashCommand>,
    /// Highlighted row among the matches.
    selected: usize,
    /// Text at which Escape closed the list; it stays closed until the text changes.
    dismissed_at: Option<String>,
}

impl Composer {
    /// The query when the list is open: text starting with `/` and no whitespace.
    fn palette_query(&self, cx: &Context<'_, Self>) -> Option<String> {
        let text = self.input().read(cx).value().to_string();
        let query = text.strip_prefix('/')?;
        if query.chars().any(char::is_whitespace)
            || self.palette.dismissed_at.as_deref() == Some(text.as_str())
        {
            return None;
        }
        Some(query.to_owned())
    }

    /// Replaces the offered commands.
    pub fn set_commands(
        &mut self,
        commands: Vec<SlashCommand>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.palette.commands != commands {
            self.palette.commands = commands;
            cx.notify();
        }
    }

    /// Writes the chosen command into the input without sending it.
    fn complete(
        &mut self,
        name: &str,
        hint: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let text = if hint {
            format!("/{name} ")
        } else {
            format!("/{name}")
        };
        self.input()
            .clone()
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        // Completed text is final until edited, so the list stays closed.
        self.palette.dismissed_at = Some(text);
        self.palette.selected = 0;
        cx.notify();
    }

    /// Matches of the open list as `(name, takes_input)`; empty while closed.
    fn palette_matches(&self, cx: &Context<'_, Self>) -> Vec<(String, bool)> {
        self.palette_query(cx)
            .map(|query| {
                filter_commands(&self.palette.commands, &query)
                    .into_iter()
                    .map(|command| {
                        (command.name.clone(), command.hint.is_some())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Moves the highlight by one row, wrapping around.
    pub(in crate::ui) fn palette_move(
        &mut self,
        down: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let count = self.palette_matches(cx).len();
        if count == 0 {
            return;
        }
        self.palette.selected = if down {
            (self.palette.selected + 1) % count
        } else {
            (self.palette.selected + count - 1) % count
        };
        cx.notify();
    }

    /// Completes the highlighted command; during an IME composition it only confirms the candidate.
    pub(in crate::ui) fn palette_accept(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let composing = self.input().clone().update(cx, |input, cx| {
            let composing = input.marked_text_range(window, cx).is_some();
            if composing {
                input.unmark_text(window, cx);
            }
            composing
        });
        if composing {
            return;
        }
        let matches = self.palette_matches(cx);
        let index = self.palette.selected.min(matches.len().saturating_sub(1));
        if let Some((name, hint)) = matches.get(index).cloned() {
            self.complete(&name, hint, window, cx);
        }
    }

    /// Closes the list until the text changes; the run keeps going.
    pub(in crate::ui) fn palette_dismiss(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) {
        self.palette.dismissed_at =
            Some(self.input().read(cx).value().to_string());
        cx.notify();
    }

    /// Forgets a dismissal once the text changed.
    pub(in crate::ui) fn on_palette_edit(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) {
        let text = self.input().read(cx).value().to_string();
        if self
            .palette
            .dismissed_at
            .as_ref()
            .is_some_and(|dismissed| *dismissed != text)
        {
            self.palette.dismissed_at = None;
        }
        self.palette.selected = 0;
        cx.notify();
    }

    /// The list above the input; `None` while closed.
    ///
    /// Also returns whether the list has a command to pick, which decides
    /// the input's key context: the picking keys apply only then. The query
    /// and the filter run once per frame here.
    pub(in crate::ui) fn render_palette(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<(AnyElement, bool)> {
        let query = self.palette_query(cx)?;
        let theme = cx.theme();
        let matches = filter_commands(&self.palette.commands, &query);
        let selected =
            self.palette.selected.min(matches.len().saturating_sub(1));
        let rows: Vec<AnyElement> = matches
            .into_iter()
            .enumerate()
            .map(|(n, command)| {
                let (name, hint) =
                    (command.name.clone(), command.hint.is_some());
                let source = command.source.map(CommandSource::label);
                v_flex()
                    .id(SharedString::from(format!("command-row-{n}")))
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(n == selected, |this| this.bg(theme.accent))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(format!("/{}", command.name))
                                    .text_sm(),
                            )
                            .when_some(command.hint.clone(), |this, hint| {
                                this.child(
                                    Label::new(hint)
                                        .text_xs()
                                        .font_family(
                                            theme.mono_font_family.clone(),
                                        )
                                        .font_weight(
                                            crate::theme::FontWeights::get(cx)
                                                .code,
                                        )
                                        .text_color(theme.muted_foreground),
                                )
                            })
                            .when_some(source, |this, source| {
                                this.child(
                                    Label::new(source.into_owned())
                                        .text_xs()
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .child(
                        Label::new(command.description.clone())
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .when_some(
                        command.qualified_name.clone(),
                        |this, qualified| {
                            this.child(
                                Label::new(
                                    rust_i18n::t!(
                                        "Command.full_name",
                                        name = qualified
                                    )
                                    .into_owned(),
                                )
                                .text_xs()
                                .text_color(theme.muted_foreground),
                            )
                        },
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.complete(&name, hint, window, cx);
                            this.focus(window, cx);
                        },
                    ))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let list = v_flex()
            .id("command-palette")
            .test_support()
            .mx_2()
            .mt_1()
            .p_1()
            .max_h(gpui_kit::rems(16.))
            .overflow_y_scroll()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .children(rows)
            .when(empty, |this| {
                this.child(
                    Label::new(rust_i18n::t!("Command.no_match").into_owned())
                        .text_sm()
                        .text_color(theme.muted_foreground),
                )
            })
            .into_any_element();
        Some((list, !empty))
    }
}

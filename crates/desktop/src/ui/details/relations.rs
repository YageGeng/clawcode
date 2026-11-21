//! Related sessions in the details card: the parent a session was forked
//! from and its subagents, and opening a subagent beside its parent or as a tab.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};

use super::details_row;
use crate::backend::SessionId;
use crate::catalog::SessionSummary;
use crate::session::SessionStatus;
use crate::state::{PanelTabKey, SubagentOpenTarget};
use crate::ui::{AppView, TabEntry};

/// A subagent of the active session, as the relations group lists it.
pub(super) struct Agent {
    /// The subagent's session.
    summary: SessionSummary,
    /// Its status, as far as the desktop knows it.
    status: SessionStatus,
}

/// The active session's parent, running subagents, and previous subagents.
#[derive(Default)]
pub(super) struct Relations {
    /// The session this one was forked from.
    pub(super) parent: Option<SessionSummary>,
    /// Subagents running now or during the parent's current run.
    pub(super) running: Vec<Agent>,
    /// Subagents of earlier runs.
    pub(super) previous: Vec<Agent>,
}

impl AppView {
    /// The relations of the active tab; nothing for a draft.
    pub(super) fn relations(
        &self,
        entry: &TabEntry,
        cx: &gpui_kit::App,
    ) -> Relations {
        let TabEntry::Session(id) = entry else {
            return Relations::default();
        };
        let catalog = &self.host.catalog;
        let parent = catalog
            .get(id)
            .and_then(|summary| summary.parent_session_id.as_ref())
            .and_then(|parent| catalog.get(parent))
            .cloned();
        // The list carries no run state: a subagent counts as running when the
        // desktop sees it run, or when it changed during its parent's current run.
        let run_started = self
            .session_handle(id, cx)
            .and_then(|handle| handle.read(cx).projection().run_started_ms());
        let (running, previous) = catalog
            .children(id)
            .into_iter()
            .map(|summary| Agent {
                status: self.session_status(&summary.session_id, cx),
                summary: summary.clone(),
            })
            .partition(|agent| {
                matches!(
                    agent.status,
                    SessionStatus::Running | SessionStatus::Waiting
                ) || agent.summary.changed_since(run_started)
            });
        Relations {
            parent,
            running,
            previous,
        }
    }

    /// The parent session; a click opens it.
    pub(super) fn render_parent_row(
        &self,
        parent: &SessionSummary,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let id = parent.session_id.clone();
        details_row("details-parent", &theme)
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open_session(&id, cx)
            }))
            .child(Icon::new(LucideIcon::CornerLeftUp).small())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(parent.display_title()).text_sm()),
            )
            .into_any_element()
    }

    /// The fold over `count` previous subagents; a click opens or closes it.
    pub(super) fn render_previous_row(
        &self,
        count: usize,
        open: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        details_row("details-previous", &theme)
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.details.previous_open = !this.details.previous_open;
                cx.notify();
            }))
            .child(
                Icon::new(if open {
                    LucideIcon::ChevronDown
                } else {
                    LucideIcon::ChevronRight
                })
                .small(),
            )
            .child(
                Label::new(
                    rust_i18n::t!("Details.previous", count = count)
                        .into_owned(),
                )
                .text_sm()
                .text_color(theme.muted_foreground),
            )
            .into_any_element()
    }

    /// One subagent: its title and status; a click opens it.
    pub(super) fn render_agent_row(
        &self,
        agent: &Agent,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let id: SessionId = agent.summary.session_id.clone();
        details_row(&format!("details-agent-{id}"), &theme)
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .on_click(cx.listener(
                move |this, event: &ClickEvent, window, cx| {
                    // Ctrl (⌘ on macOS) opens it in the other place.
                    let alternate = event.modifiers().secondary(cx);
                    this.open_subagent(id.clone(), alternate, window, cx)
                },
            ))
            .child(Icon::new(LucideIcon::Bot).small())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(agent.summary.display_title()).text_sm()),
            )
            .child(crate::ui::StatusIcon::new(agent.status))
            .into_any_element()
    }

    /// Opens a subagent where the settings say, or in the other place for a
    /// modified click: beside its parent in the panel, or as a main card tab.
    pub(in crate::ui) fn open_subagent(
        &mut self,
        id: SessionId,
        alternate: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let in_panel = matches!(
            self.state.subagent_open_target,
            SubagentOpenTarget::Panel
        );
        if in_panel != alternate {
            self.open_panel_tab(
                PanelTabKey::Subagent {
                    session: id.to_string(),
                },
                window,
                cx,
            );
        } else {
            self.open_session(&id, cx);
        }
    }
}

//! The floating content card: tab strip, session status, and the active tab's body.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::base::{ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_resizable,
    resizable_panel, resize_handle_appearance,
};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px, rems,
};

use crate::session::SessionStatus;
use crate::state::LayoutState;
use crate::ui::format::Elapsed;
use crate::ui::session::loading_skeleton;
use crate::ui::{AppView, BackendPhase, StatusIcon, TabEntry};

/// A resize handle that draws nothing at rest, so the chrome stays one undivided surface;
/// it shows gpui-kit's indicator while hovered or dragged.
fn quiet_resize_handle() -> ResizeHandleRenderer {
    let indicator = resize_handle_appearance();
    std::rc::Rc::new(move |handle, window, cx| {
        if handle.state() == ResizeHandleState::Idle {
            // An empty element, not `None`: declining makes gpui-base draw its default hairline.
            Some(div().into_any_element())
        } else {
            indicator(handle, window, cx)
        }
    })
}

/// A tab being dragged to a new position; it also draws its drag preview.
#[derive(Clone)]
struct DraggedTab {
    /// The dragged tab; its position is looked up on drop, as tabs may have moved meanwhile.
    entry: TabEntry,
    /// Title shown in the preview.
    title: SharedString,
}

impl Render for DraggedTab {
    /// A small translucent chip with the tab's title, following the pointer.
    fn render(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h(rems(1.75))
            .max_w(rems(13.75))
            .flex()
            .items_center()
            .px_2p5()
            .truncate()
            .text_sm()
            .text_color(theme.foreground)
            .bg(theme.muted)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .opacity(0.85)
            .child(self.title.clone())
    }
}

impl AppView {
    /// Title of a tab: the session title, or "New session" for drafts.
    fn tab_title(&self, tab: &TabEntry) -> SharedString {
        match tab {
            TabEntry::Session(id) => self
                .host
                .catalog
                .get(id)
                .map(|summary| summary.display_title())
                .unwrap_or_else(|| {
                    rust_i18n::t!("Sidebar.untitled").into_owned().into()
                }),
            TabEntry::Draft { .. } => {
                rust_i18n::t!("Sidebar.new_session").into_owned().into()
            }
        }
    }

    /// Status of a tab's session; drafts are idle.
    fn tab_status(&self, tab: &TabEntry, cx: &gpui_kit::App) -> SessionStatus {
        match tab {
            TabEntry::Session(id) => self.session_status(id, cx),
            TabEntry::Draft { .. } => SessionStatus::Idle,
        }
    }

    /// Renders the session card, the main card, and the right panel side by
    /// side; the session card and the panel are resizable.
    pub(in crate::ui) fn render_body(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let layout = &self.state.layout;
        let panel = self.render_inline_panel(window, cx);
        let maximized = panel.is_some()
            && self.panel().is_some_and(|panel| panel.maximized);
        // Pane widths are persisted pixels, the one place the shell sizes in `px`.
        let mut split = h_resizable("shell-split")
            .with_state(&self.sidebar_split)
            .with_handle_appearance(quiet_resize_handle())
            .child(
                resizable_panel()
                    .size(px(layout.sidebar_width()))
                    .size_range(
                        px(LayoutState::MIN_SIDEBAR_WIDTH)
                            ..px(LayoutState::MAX_SIDEBAR_WIDTH),
                    )
                    .visible(layout.sidebar_open)
                    .child(self.render_session_card(window, cx)),
            )
            .child(
                resizable_panel().visible(!maximized).child(
                    div()
                        .size_full()
                        .pl_2()
                        .pr_2()
                        .pb_2()
                        .child(self.render_card(window, cx)),
                ),
            );
        if let Some(panel) = panel {
            // A maximized panel takes the main card's room; otherwise it keeps its width.
            let column = if maximized {
                resizable_panel()
            } else {
                resizable_panel()
                    .size(px(self.panel_width(window)))
                    .size_range(px(LayoutState::MIN_PANEL_WIDTH)..Pixels::MAX)
            };
            split = split.child(column.child(panel));
        }
        split
    }

    /// Renders the card: header strip and body.
    fn render_card(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.is_dark();
        div()
            .id("content-card")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .when(!dark, |this| this.shadow_sm())
            // No tabs, no strip: the card shows only its empty state.
            .when(!self.tabs.is_empty(), |this| {
                this.child(self.render_card_header(window, cx))
            })
            .when(self.backend_stopped(), |this| {
                this.child(self.render_stopped_banner(cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    // The details card floats over the body's top right corner.
                    .relative()
                    .child(self.render_card_body(window, cx))
                    .children(self.render_inline_details(window, cx)),
            )
    }

    /// Tabs on the leading side; status and session actions on the trailing side.
    fn render_card_header(
        &self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (selected_bg, hover_bg, foreground, muted, radius) = (
            theme.muted,
            theme.accent,
            theme.foreground,
            theme.muted_foreground,
            theme.radius,
        );
        // Spec §5.5 and the mockup: the selected tab is a muted fill with
        // foreground text, the rest are muted text; gpui-kit's pill tabs fill
        // the selection with `primary`, so the strip is drawn here.
        let tabs: Vec<AnyElement> = self
            .tabs
            .entries()
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                // Identity comes from the entry, not the slot: tabs reorder,
                // and listeners resolve their tab's position when they run.
                let key = tab.key();
                let selected = self.tabs.active() == Some(index);
                let title = self.tab_title(tab);
                let dragged = DraggedTab {
                    entry: tab.clone(),
                    title: title.clone(),
                };
                let (close, click, target) =
                    (tab.clone(), tab.clone(), tab.clone());
                div()
                    .id(SharedString::from(format!("tab:{key}")))
                    .test_support()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .h(rems(1.75))
                    .max_w(rems(13.75))
                    .min_w_0()
                    .pl_2p5()
                    .pr_0p5()
                    .rounded(radius)
                    .text_sm()
                    .cursor_pointer()
                    .map(|this| {
                        if selected {
                            this.bg(selected_bg).text_color(foreground)
                        } else {
                            this.text_color(muted)
                                .hover(|style| style.bg(hover_bg))
                        }
                    })
                    .child(StatusIcon::new(self.tab_status(tab, cx)))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(
                        Button::new(SharedString::from(format!(
                            "close-tab:{key}"
                        )))
                        .ghost()
                        .small()
                        .icon(LucideIcon::X)
                        .tooltip(rust_i18n::t!("Menu.close_tab").to_string())
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.close_entry(&close, cx);
                            },
                        )),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            if let Some(index) = this.tab_index(&click) {
                                this.activate(index, cx)
                            }
                        },
                    ))
                    // Spec §5.5: tabs reorder by dragging one onto another.
                    .on_drag(dragged, |dragged, _, _, cx| {
                        cx.new(|_| dragged.clone())
                    })
                    .drag_over::<DraggedTab>(move |style, _, _, cx| {
                        style.bg(cx.theme().drag_border.opacity(0.2))
                    })
                    .on_drop(cx.listener(
                        move |this, dragged: &DraggedTab, _, cx| {
                            if let (Some(from), Some(to)) = (
                                this.tab_index(&dragged.entry),
                                this.tab_index(&target),
                            ) {
                                this.move_tab(from, to, cx)
                            }
                        },
                    ))
                    .into_any_element()
            })
            .collect();
        let status = self
            .active_entry()
            .map(|tab| self.tab_status(&tab, cx))
            .filter(|_| {
                matches!(self.active_entry(), Some(TabEntry::Session(_)))
            });
        let border = cx.theme().border;
        div()
            .flex()
            .items_center()
            .flex_none()
            .h_10()
            .px_1p5()
            .gap_1()
            .border_b_1()
            .border_color(border)
            .child(
                // A zero basis keeps the tabs' width out of the card's minimum
                // width; extra tabs scroll instead of widening the card.
                div()
                    .id("tabs")
                    .flex_1()
                    .w_0()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(tabs),
            )
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .text_sm()
                        .text_color(muted)
                        .child(StatusIcon::new(status))
                        .child(
                            Label::new(self.header_status_label(status, cx))
                                .text_sm()
                                .text_color(muted),
                        ),
                )
                .child(div().w_px().h_4().bg(border))
                .child(self.render_session_menu(cx))
            })
            .children(self.render_header_tools(window, cx))
    }

    /// Status word for the card header; a running session also shows how long the run has lasted.
    fn header_status_label(
        &self,
        status: SessionStatus,
        cx: &gpui_kit::App,
    ) -> String {
        let started = match (status, self.active_entry()) {
            (SessionStatus::Running, Some(TabEntry::Session(id))) => {
                self.session_handle(&id, cx).and_then(|handle| {
                    handle.read(cx).projection().run_started_ms()
                })
            }
            _ => None,
        };
        let Some(started) = started else {
            return status.label();
        };
        let now = crate::now_ms();
        rust_i18n::t!(
            "Status.running_for",
            elapsed = Elapsed(now.saturating_sub(started)).label().as_ref()
        )
        .into_owned()
    }

    /// `⋯` menu of the active session: rename, close, delete.
    fn render_session_menu(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let Some(TabEntry::Session(id)) = self.active_entry() else {
            return div().into_any_element();
        };
        Button::new("session-menu")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip(rust_i18n::t!("Session.more").to_string())
            .dropdown_menu(move |menu, _, _| {
                let rename = (view.clone(), id.clone());
                // The menu outlives this frame: close the session's tab, wherever it is by then.
                let close = (view.clone(), TabEntry::Session(id.clone()));
                let delete = (view.clone(), id.clone());
                menu.item(
                    PopupMenuItem::new(
                        rust_i18n::t!("Session.rename").into_owned(),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = rename.0.update(cx, |this, cx| {
                            this.rename_session(&rename.1, window, cx)
                        });
                    }),
                )
                .item(
                    PopupMenuItem::new(
                        rust_i18n::t!("Session.close").into_owned(),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = close.0.update(cx, |this, cx| {
                            this.close_entry(&close.1, cx)
                        });
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(
                        rust_i18n::t!("Session.delete").into_owned(),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = delete.0.update(cx, |this, cx| {
                            this.delete_session(&delete.1, window, cx)
                        });
                    }),
                )
            })
            .into_any_element()
    }

    /// The active tab's content, or the state that replaces it.
    fn render_card_body(
        &self,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if let BackendPhase::Failed(message) = &self.host.phase {
            return self.render_startup_error(message, cx);
        }
        let centered = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_8()
                .child(Label::new(text).text_color(muted))
        };
        match self.active_entry() {
            None => centered(rust_i18n::t!("Card.no_tabs").into_owned())
                .into_any_element(),
            // The view exists once `ensure_active_view` found something to show;
            // until then the session is still attaching.
            Some(entry) => match self.views.get(&entry) {
                // Cached: shell-only redraws (sidebar, search, tab strip)
                // reuse the transcript; the view redraws when it is notified.
                Some(view) => view
                    .clone()
                    .cached(StyleRefinement::default().size_full())
                    .into_any_element(),
                None => loading_skeleton().into_any_element(),
            },
        }
    }

    /// Danger banner shown when the backend connection ended, with a restart action.
    fn render_stopped_banner(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.danger)
            .bg(theme.danger.opacity(0.08))
            .child(
                Label::new(rust_i18n::t!("Backend.stopped").into_owned())
                    .flex_1()
                    .text_sm()
                    .text_color(theme.danger),
            )
            .child(
                Button::new("restart-app")
                    .small()
                    .label(rust_i18n::t!("Backend.restart").into_owned())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.restart_app(cx)
                    })),
            )
    }

    /// Full-card explanation of a failed backend start, with the two recovery actions.
    fn render_startup_error(
        &self,
        message: &str,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                div()
                    .id("startup-error")
                    .test_support()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .max_w_96()
                    .child(
                        Label::new(
                            rust_i18n::t!("Startup.failed_title").into_owned(),
                        )
                        .text_lg()
                        .font_semibold(),
                    )
                    .child(
                        Label::new(message.to_string())
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .font_weight(
                                crate::theme::FontWeights::get(cx).code,
                            )
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("open-config")
                                    .outline()
                                    .label(
                                        rust_i18n::t!("Startup.open_config")
                                            .into_owned(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| {
                                            this.open_config(cx)
                                        },
                                    )),
                            )
                            .child(
                                Button::new("retry")
                                    .primary()
                                    .label(
                                        rust_i18n::t!("Startup.retry")
                                            .into_owned(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| {
                                            this.start_backend(cx)
                                        },
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }
}

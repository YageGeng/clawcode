//! Drawing the panel card, its tab strip, and the header tools that toggle it.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
    h_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _, px,
};

use super::{PanelPresentation, PanelView};

use crate::actions::{ShowChanges, ShowFiles, TogglePanel};
use crate::backend::SessionId;
use crate::catalog::SessionCatalog;
use crate::state::{PanelState, PanelTabKey};
use crate::ui::AppView;
use crate::ui::file_icon::FileIconPainter;
use crate::ui::session::loading_skeleton;

impl PanelTabKey {
    /// The tab's element id suffix: `changes`, `files`, `file:{path}`, or `agent:{session}`.
    pub fn id(&self) -> String {
        match self {
            PanelTabKey::Changes => "changes".to_string(),
            PanelTabKey::Files => "files".to_string(),
            PanelTabKey::File { path } => format!("file:{path}"),
            PanelTabKey::Subagent { session } => format!("agent:{session}"),
            PanelTabKey::Record => "record".to_string(),
            PanelTabKey::Unknown => "unknown".to_string(),
        }
    }

    /// The tab's icon and title; a subagent is titled from the catalog.
    fn label(
        &self,
        painter: &FileIconPainter,
        catalog: &SessionCatalog,
    ) -> (AnyElement, String) {
        let icon = |name| Icon::new(name).small().into_any_element();
        match self {
            PanelTabKey::Changes => (
                icon(LucideIcon::GitCompare),
                rust_i18n::t!("Panel.changes").into_owned(),
            ),
            PanelTabKey::Files => (
                icon(LucideIcon::FolderTree),
                rust_i18n::t!("Panel.files").into_owned(),
            ),
            PanelTabKey::File { path } => {
                let (_, name) = super::split_path(path);
                (painter.file(name), name.to_string())
            }
            PanelTabKey::Subagent { session } => (
                icon(LucideIcon::Bot),
                catalog
                    .get(&SessionId::from(session.clone()))
                    .map(|summary| summary.display_title().to_string())
                    .unwrap_or_default(),
            ),
            PanelTabKey::Record => (
                icon(LucideIcon::ScrollText),
                rust_i18n::t!("Record.title").into_owned(),
            ),
            PanelTabKey::Unknown => (div().into_any_element(), String::new()),
        }
    }
}

impl AppView {
    /// The panel card: its tab strip above the active tab's view.
    pub(super) fn render_panel_card(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let owner = self.panel_owner()?;
        let panel = self.state.panels.get(&owner.key())?;
        let active = panel.active.and_then(|index| panel.tabs.get(index));
        let body = active
            .and_then(|tab| {
                self.panels.runtimes.get(&owner.key())?.views.get(tab)
            })
            .map(PanelView::element)
            .unwrap_or_else(|| loading_skeleton().into_any_element());
        let theme = cx.theme();
        Some(
            div()
                .id("right-panel")
                .test_support()
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius_lg)
                .when(!theme.is_dark(), |this| this.shadow_sm())
                .child(self.render_panel_tabs(panel, cx))
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element(),
        )
    }

    /// The panel's tab strip with its maximize and close buttons.
    pub(super) fn render_panel_tabs(
        &self,
        panel: &PanelState,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let painter = self.file_icon_painter(cx);
        let tabs: Vec<AnyElement> = panel
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let active = panel.active == Some(index);
                self.render_panel_tab(index, tab, active, &painter, cx)
            })
            .collect();
        let maximized = panel.maximized;
        // A subagent shown here switches between its chat and its trajectory
        // like a session tab in the main card.
        let switch = panel
            .active
            .and_then(|index| panel.tabs.get(index))
            .zip(self.panel_owner())
            .and_then(|(tab, owner)| {
                match self.panels.runtimes.get(&owner.key())?.views.get(tab)? {
                    PanelView::Subagent(view) => Some(view.clone()),
                    _ => None,
                }
            })
            .map(|view| self.render_view_switch("panel-view-switch", view, cx));
        h_flex()
            .flex_none()
            .h_10()
            .px_1p5()
            .gap_0p5()
            .border_b_1()
            .border_color(border)
            .child(
                // A zero basis keeps many tabs from widening the panel; they scroll.
                div()
                    .id("panel-tabs")
                    .flex_1()
                    .w_0()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .children(switch)
            .child(
                Button::new("panel-maximize")
                    .ghost()
                    .small()
                    .icon(if maximized {
                        LucideIcon::Minimize2
                    } else {
                        LucideIcon::Maximize2
                    })
                    .selected(maximized)
                    .tooltip(rust_i18n::t!("Panel.maximize").into_owned())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_panel_maximized(cx)
                    })),
            )
            .child(
                Button::new("panel-close")
                    .ghost()
                    .small()
                    .icon(LucideIcon::PanelRightClose)
                    .tooltip_with_action(
                        rust_i18n::t!("Panel.close").into_owned(),
                        &TogglePanel,
                        Some(crate::actions::WORKSPACE_CONTEXT),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_panel(cx)
                    })),
            )
    }

    /// One tab of the strip: icon, title, and close button; a middle click closes it too.
    fn render_panel_tab(
        &self,
        index: usize,
        tab: &PanelTabKey,
        active: bool,
        painter: &FileIconPainter,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (selected, hover, muted, radius) = (
            theme.muted,
            theme.accent,
            theme.muted_foreground,
            theme.radius,
        );
        let (icon, title) = tab.label(painter, &self.host.catalog);
        let id = tab.id();
        div()
            .id(SharedString::from(format!("panel-tab:{id}")))
            .test_support()
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .max_w(gpui_kit::rems(12.))
            .h_7()
            .pl_2()
            .pr_0p5()
            .rounded(radius)
            .text_sm()
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(selected)
                } else {
                    this.text_color(muted).hover(|style| style.bg(hover))
                }
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_panel_tab(index, cx)
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    this.close_panel_tab(index, cx)
                }),
            )
            .child(icon)
            .child(div().min_w_0().truncate().child(title))
            .child(
                Button::new(SharedString::from(format!(
                    "panel-tab-close:{id}"
                )))
                .ghost()
                .xsmall()
                .icon(LucideIcon::X)
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.close_panel_tab(index, cx)
                    },
                )),
            )
            .into_any_element()
    }

    /// The main card header's panel buttons: Changes, Files, and the panel toggle.
    pub(in crate::ui) fn render_header_tools(
        &self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let details = self.render_details_button(window, cx);
        if self.panel_owner().is_none() {
            return details.into_iter().collect();
        }
        let not_git = self.current_workspace().is_some_and(|workspace| {
            self.is_repository(&workspace) == Some(false)
        });
        let open = self.panel().is_some_and(|panel| panel.open);
        let tooltip = |key: &str| rust_i18n::t!(key).into_owned();
        vec![
            Button::new("header-changes")
                .ghost()
                .small()
                .icon(LucideIcon::GitCompare)
                .disabled(not_git)
                .tooltip_with_action(
                    if not_git {
                        tooltip("Panel.not_repository")
                    } else {
                        tooltip("Panel.changes")
                    },
                    &ShowChanges,
                    Some(crate::actions::WORKSPACE_CONTEXT),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.open_panel_tab(PanelTabKey::Changes, window, cx)
                }))
                .into_any_element(),
            Button::new("header-files")
                .ghost()
                .small()
                .icon(LucideIcon::FolderTree)
                .tooltip_with_action(
                    tooltip("Panel.files"),
                    &ShowFiles,
                    Some(crate::actions::WORKSPACE_CONTEXT),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.open_panel_tab(PanelTabKey::Files, window, cx)
                }))
                .into_any_element(),
            details.unwrap_or_else(|| div().into_any_element()),
            Button::new("header-panel")
                .ghost()
                .small()
                .icon(LucideIcon::PanelRight)
                .selected(open)
                .tooltip_with_action(
                    tooltip("Panel.toggle"),
                    &TogglePanel,
                    Some(crate::actions::WORKSPACE_CONTEXT),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_panel(window, cx)
                }))
                .into_any_element(),
        ]
    }

    /// The panel beside the main card, inside its resizable column.
    pub(in crate::ui) fn render_inline_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if self.panel_presentation(window) != PanelPresentation::Inline {
            return None;
        }
        let card = self.render_panel_card(cx)?;
        // The column's padding sets the card's `sm` gaps to the title bar and
        // the window's right and bottom edges.
        Some(
            div()
                .size_full()
                .pt_2()
                .pr_2()
                .pb_2()
                .child(card)
                .into_any_element(),
        )
    }

    /// The panel as a sheet over the main card, with a scrim that closes it.
    pub(in crate::ui) fn render_panel_sheet(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if self.panel_presentation(window) != PanelPresentation::Sheet {
            return None;
        }
        let card = self.render_panel_card(cx)?;
        let width: Pixels = px(self.panel_width(window));
        Some(
            div()
                .id("panel-sheet")
                .test_support()
                .absolute()
                .inset_0()
                .key_context("PanelSheet")
                .track_focus(&self.panels.sheet_focus)
                .on_action(cx.listener(
                    |this, _: &crate::actions::ClosePanel, _, cx| {
                        this.close_panel(cx)
                    },
                ))
                .child(
                    div()
                        .id("panel-scrim")
                        .absolute()
                        .inset_0()
                        .bg(gpui_kit::black().opacity(0.3))
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.close_panel(cx),
                        )),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(width)
                        .p_2()
                        .occlude()
                        .child(card),
                )
                .into_any_element(),
        )
    }
}

//! The session details card (M3b spec §5): the active tab's workspace,
//! branch, changed lines, and related sessions, floating at the main card's
//! top right, or in a popover from the header when the card is too narrow.

mod open_in;
mod relations;

use std::path::Path;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{
    ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem,
};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, h_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt as _, WeakEntity, Window, div, prelude::FluentBuilder as _,
    px, rems,
};
use protocol::{WorkspaceDiffScope, WorkspaceGitStatus};

pub use open_in::EditorCatalog;
use relations::{Agent, Relations};

use crate::catalog::SessionSummary;
use crate::state::{DesktopState, DetailsItem, OpenIn, PanelTabKey};
use crate::ui::panel::PanelPresentation;
use crate::ui::{AppView, TabEntry};

/// The card's width in rems: 280px at the default 16px.
const CARD_REMS: f32 = 17.5;
/// The transcript column the card must not cover, in px at 16px rem.
const TRANSCRIPT_WIDTH: f32 = 680.0;
/// One row's height in rems, for fitting the card to the room it has.
const ROW_REMS: f32 = 2.0;
/// A separator between groups, in rems.
const SEPARATOR_REMS: f32 = 0.5;
/// What sits above the main card body: title bar, card header, and gaps, in rems.
const CHROME_REMS: f32 = 6.0;

/// How much of the card shows, from everything to the essentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsDensity {
    /// Every row the user keeps.
    Full,
    /// Without "Open in", previous agents, context, and usage.
    Compact,
    /// Branch and changes only.
    Essential,
}

impl DetailsDensity {
    /// The fullest density whose height fits `available`, from heights in
    /// Full, Compact, Essential order; `true` when even Essential overflows
    /// and the card scrolls.
    pub fn fit(available: Pixels, heights: [Pixels; 3]) -> (Self, bool) {
        let densities = [Self::Full, Self::Compact, Self::Essential];
        densities
            .into_iter()
            .zip(heights)
            .find(|(_, height)| *height <= available)
            .map_or((Self::Essential, true), |(density, _)| (density, false))
    }

    /// Whether an item shows at this density.
    fn keeps(self, item: DetailsItem) -> bool {
        match self {
            Self::Full => true,
            Self::Compact => !matches!(
                item,
                DetailsItem::OpenIn | DetailsItem::Context | DetailsItem::Usage
            ),
            Self::Essential => {
                matches!(item, DetailsItem::Branch | DetailsItem::Changes)
            }
        }
    }
}

/// The details card's shell state.
pub struct DetailsHost {
    /// External editors found on `PATH` at startup.
    pub(in crate::ui) editors: EditorCatalog,
    /// Whether the card lists previous subagents.
    pub(in crate::ui) previous_open: bool,
}

impl Default for DetailsHost {
    /// Editors detected from this process's `PATH`; previous subagents folded.
    fn default() -> Self {
        Self {
            editors: EditorCatalog::detect(std::env::var_os("PATH").as_deref()),
            previous_open: false,
        }
    }
}

/// Where the card is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsPresentation {
    /// Floating over the main card.
    Inline,
    /// In a popover from the header button.
    Popover,
}

/// One row the card may show. The card plans its rows once per density and
/// uses the same plan both to size itself and to draw.
enum DetailsRow<'a> {
    /// The workspace name.
    Workspace,
    /// The note that the workspace folder is gone.
    Missing,
    /// "Open in …" and its menu.
    OpenIn,
    /// The checked-out branch; `None` when detached.
    Branch(Option<&'a str>),
    /// Changed lines against the diff base.
    Changes {
        /// The diff base's name.
        base: &'a str,
        /// Lines added.
        insertions: u64,
        /// Lines removed.
        deletions: u64,
    },
    /// The session this one was forked from.
    Parent(&'a SessionSummary),
    /// One subagent.
    Agent(&'a Agent),
    /// The fold over previous subagents.
    Previous {
        /// How many previous subagents there are.
        count: usize,
        /// Whether they are listed below.
        open: bool,
    },
    /// The context window fill.
    Context,
    /// The last run's usage.
    Usage,
}

/// What the card describes, read once per frame, planned into rows per density.
struct DetailsPlan<'a> {
    /// Which rows the user keeps.
    state: &'a DesktopState,
    /// The workspace folder no longer exists.
    missing: bool,
    /// The workspace's last read Git status.
    git: Option<&'a WorkspaceGitStatus>,
    /// The active session's parent and subagents.
    relations: &'a Relations,
    /// Whether previous subagents are unfolded.
    previous_open: bool,
}

impl<'a> DetailsPlan<'a> {
    /// The row groups shown at `density`, in card order, empty groups left out.
    fn groups(&self, density: DetailsDensity) -> Vec<Vec<DetailsRow<'a>>> {
        let keep =
            |item| self.state.details_item_visible(item) && density.keeps(item);
        let mut workspace = vec![DetailsRow::Workspace];
        if self.missing {
            workspace.push(DetailsRow::Missing);
        } else if keep(DetailsItem::OpenIn) {
            workspace.push(DetailsRow::OpenIn);
        }
        let mut git = Vec::new();
        if let (
            false,
            Some(WorkspaceGitStatus::Repository {
                branch,
                base,
                insertions,
                deletions,
                ..
            }),
        ) = (self.missing, self.git)
        {
            if keep(DetailsItem::Branch) {
                git.push(DetailsRow::Branch(branch.as_deref()));
            }
            if keep(DetailsItem::Changes) {
                git.push(DetailsRow::Changes {
                    base,
                    insertions: *insertions,
                    deletions: *deletions,
                });
            }
        }
        let mut related = Vec::new();
        if keep(DetailsItem::Relations) {
            let relations = self.relations;
            related.extend(relations.parent.iter().map(DetailsRow::Parent));
            related.extend(relations.running.iter().map(DetailsRow::Agent));
            // Previous subagents fold away below full density.
            if !relations.previous.is_empty() && density == DetailsDensity::Full
            {
                related.push(DetailsRow::Previous {
                    count: relations.previous.len(),
                    open: self.previous_open,
                });
                if self.previous_open {
                    related.extend(
                        relations.previous.iter().map(DetailsRow::Agent),
                    );
                }
            }
        }
        let mut other = Vec::new();
        if keep(DetailsItem::Context) {
            other.push(DetailsRow::Context);
        }
        if keep(DetailsItem::Usage) {
            other.push(DetailsRow::Usage);
        }
        [workspace, git, related, other]
            .into_iter()
            .filter(|group| !group.is_empty())
            .collect()
    }
}

impl AppView {
    /// The session catalog, as last listed.
    pub fn catalog(&self) -> &crate::catalog::SessionCatalog {
        &self.host.catalog
    }

    /// Shows or hides one details row, saving only choices that differ from the default.
    pub fn set_details_item(
        &mut self,
        item: DetailsItem,
        visible: bool,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.set_details_item(item, visible);
        self.persist(cx);
        cx.notify();
    }

    /// Whether the card fits beside the transcript in the main card.
    fn details_fit_inline(&self, window: &Window) -> bool {
        let scale = f32::from(window.rem_size()) / 16.0;
        let panel = match self.panel_presentation(window) {
            PanelPresentation::Inline => self.panel_width(window) + 8.0 * scale,
            PanelPresentation::Sheet | PanelPresentation::Hidden => 0.0,
        };
        let sidebar = if self.state.layout.sidebar_open {
            self.state.layout.sidebar_width() + 8.0 * scale
        } else {
            0.0
        };
        // The main card's own `sm` gaps on both sides.
        let main = f32::from(window.viewport_size().width)
            - sidebar
            - panel
            - 16.0 * scale;
        main >= (TRANSCRIPT_WIDTH + 16.0) * scale
            + CARD_REMS * f32::from(window.rem_size())
    }

    /// Keeps the active session view clear of the inline card: the card's
    /// width plus its gap while it shows, nothing otherwise.
    pub(in crate::ui) fn sync_details_inset(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let shown =
            self.state.layout.details_open && self.details_fit_inline(window);
        let inset = if shown {
            window.rem_size() * CARD_REMS + px(16.)
        } else {
            px(0.)
        };
        if let Some(view) = self.active_view().cloned() {
            view.update(cx, |view, cx| view.set_right_inset(inset, cx));
        }
    }

    /// The inline card, absolutely placed in the main card body, when it fits and is open.
    pub(in crate::ui) fn render_inline_details(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if !self.state.layout.details_open || !self.details_fit_inline(window) {
            return None;
        }
        let body =
            self.render_details_body(DetailsPresentation::Inline, window, cx)?;
        Some(
            div()
                .absolute()
                .top_2()
                .right_2()
                .w(rems(CARD_REMS))
                .occlude()
                .child(body)
                .into_any_element(),
        )
    }

    /// The header's details button: a toggle of the inline card, or a popover trigger when it does not fit.
    pub(in crate::ui) fn render_details_button(
        &self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        self.active_entry()?;
        let tooltip = rust_i18n::t!("Details.toggle").into_owned();
        if self.details_fit_inline(window) {
            return Some(
                Button::new("header-details")
                    .ghost()
                    .small()
                    .icon(LucideIcon::Info)
                    .selected(self.state.layout.details_open)
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.layout.details_open =
                            !this.state.layout.details_open;
                        this.persist(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let view = cx.entity().downgrade();
        Some(
            Popover::new("details-popover")
                .trigger(
                    Button::new("header-details")
                        .ghost()
                        .small()
                        .icon(LucideIcon::Info)
                        .tooltip(tooltip),
                )
                .content(move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.render_details_body(
                            DetailsPresentation::Popover,
                            window,
                            cx,
                        )
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| div().into_any_element())
                })
                .into_any_element(),
        )
    }

    /// The card itself, for the active tab; `None` without an active tab.
    fn render_details_body(
        &self,
        presentation: DetailsPresentation,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let entry = self.active_entry()?;
        let theme = cx.theme().clone();
        let card = div()
            .id("details-card")
            .test_support()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_1()
            .text_sm()
            .when(presentation == DetailsPresentation::Inline, |this| {
                this.bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .when(!theme.is_dark(), |this| this.shadow_sm())
            });
        let Some(workspace) = self.active_workspace() else {
            return Some(
                card.children(self.render_no_workspace(cx))
                    .into_any_element(),
            );
        };
        let relations = self.relations(&entry, cx);
        let plan = DetailsPlan {
            state: &self.state,
            missing: self.missing.contains(&workspace),
            git: self.git_status(&workspace),
            relations: &relations,
            previous_open: self.details.previous_open,
        };
        // Fit the rows to the room under the card's top edge.
        let rem = window.rem_size();
        let available =
            window.viewport_size().height - rem * CHROME_REMS - px(16.);
        let height = |density| {
            let groups = plan.groups(density);
            let rows: usize = groups.iter().map(Vec::len).sum();
            let separators = groups.len().saturating_sub(1);
            rem * (rows as f32 * ROW_REMS + separators as f32 * SEPARATOR_REMS)
        };
        let (density, scrolls) = match presentation {
            DetailsPresentation::Inline => DetailsDensity::fit(
                available,
                [
                    DetailsDensity::Full,
                    DetailsDensity::Compact,
                    DetailsDensity::Essential,
                ]
                .map(height),
            ),
            DetailsPresentation::Popover => (DetailsDensity::Full, false),
        };
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, rows) in plan.groups(density).iter().enumerate() {
            if index > 0 {
                body.push(
                    div()
                        .mx_2()
                        .my_0p5()
                        .h_px()
                        .bg(theme.border)
                        .into_any_element(),
                );
            }
            body.extend(rows.iter().map(|row| {
                self.render_details_row(row, &entry, &workspace, cx)
            }));
        }
        let view = cx.entity().downgrade();
        Some(
            card.when(scrolls, |this| {
                this.max_h(available).overflow_y_scroll()
            })
            .children(body)
            .context_menu(move |menu, _, cx| {
                details_items_menu(menu, &view, cx)
            })
            .into_any_element(),
        )
    }

    /// What a draft without a workspace shows: a note and the way to add one.
    fn render_no_workspace(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> [AnyElement; 2] {
        let muted = cx.theme().muted_foreground;
        [
            div()
                .p_2()
                .child(
                    Label::new(
                        rust_i18n::t!("Details.no_workspace").into_owned(),
                    )
                    .text_sm()
                    .text_color(muted),
                )
                .into_any_element(),
            Button::new("details-add-workspace")
                .ghost()
                .small()
                .label(rust_i18n::t!("Details.add_workspace").into_owned())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.add_workspace(window, cx)
                }))
                .into_any_element(),
        ]
    }

    /// Draws one planned row.
    fn render_details_row(
        &self,
        row: &DetailsRow<'_>,
        entry: &TabEntry,
        workspace: &Path,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        match row {
            DetailsRow::Workspace => self.render_workspace_row(workspace, cx),
            DetailsRow::Missing => div()
                .px_2()
                .child(
                    Label::new(
                        rust_i18n::t!("Sidebar.missing_folder").into_owned(),
                    )
                    .text_xs()
                    .text_color(theme.muted_foreground),
                )
                .into_any_element(),
            DetailsRow::OpenIn => self.render_open_in(workspace, cx),
            DetailsRow::Branch(branch) => details_row("details-branch", &theme)
                .child(Icon::new(LucideIcon::GitBranch).small())
                .child(
                    Label::new(branch.unwrap_or("HEAD").to_string()).text_sm(),
                )
                .into_any_element(),
            DetailsRow::Changes {
                base,
                insertions,
                deletions,
            } => self.render_changes_row(
                workspace,
                base,
                (*insertions, *deletions),
                cx,
            ),
            DetailsRow::Parent(parent) => self.render_parent_row(parent, cx),
            DetailsRow::Agent(agent) => self.render_agent_row(agent, cx),
            DetailsRow::Previous { count, open } => {
                self.render_previous_row(*count, *open, cx)
            }
            DetailsRow::Context => {
                let label = self
                    .details_context(entry, cx)
                    .unwrap_or_else(|| "--".to_string());
                details_row("details-context", &theme)
                    .child(Icon::new(LucideIcon::Gauge).small())
                    .child(
                        Label::new(
                            rust_i18n::t!("Details.context", value = label)
                                .into_owned(),
                        )
                        .text_sm(),
                    )
                    .into_any_element()
            }
            DetailsRow::Usage => {
                let label = self
                    .details_usage(entry, cx)
                    .unwrap_or_else(|| "--".to_string());
                details_row("details-usage", &theme)
                    .child(Icon::new(LucideIcon::ChartColumn).small())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(Label::new(label).text_xs()),
                    )
                    .into_any_element()
            }
        }
    }

    /// Changed lines against `base`; a click opens Changes, hovering lists the files.
    fn render_changes_row(
        &self,
        workspace: &Path,
        base: &str,
        (insertions, deletions): (u64, u64),
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let view = cx.entity().downgrade();
        let tip_workspace = workspace.to_path_buf();
        details_row("details-changes", &theme)
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .tooltip(move |window, cx| {
                // The breakdown is built only when the tooltip opens, not every frame.
                let breakdown = view
                    .upgrade()
                    .map(|view| view.read(cx).changes_breakdown(&tip_workspace))
                    .unwrap_or_default();
                gpui_kit::component::tooltip::Tooltip::new(breakdown)
                    .build(window, cx)
            })
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_changes_against_base(window, cx)
            }))
            .child(Icon::new(LucideIcon::FileDiff).small())
            .child(
                Label::new(rust_i18n::t!("Details.changes").into_owned())
                    .text_sm(),
            )
            .child(
                div().flex_1().min_w_0().truncate().child(
                    Label::new(
                        rust_i18n::t!("Changes.against", base = base)
                            .into_owned(),
                    )
                    .text_xs()
                    .text_color(theme.muted_foreground),
                ),
            )
            .child(
                Label::new(format!("+{insertions}"))
                    .text_xs()
                    .text_color(theme.success),
            )
            .child(
                Label::new(format!("−{deletions}"))
                    .text_xs()
                    .text_color(theme.danger),
            )
            .into_any_element()
    }

    /// The workspace name row.
    fn render_workspace_row(
        &self,
        workspace: &Path,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        details_row("details-workspace", theme)
            .child(Icon::new(LucideIcon::Folder).small())
            .child(
                Label::new(crate::workspace::display_name(workspace))
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM),
            )
            .into_any_element()
    }

    /// "Open in …" with the menu of other places to open the workspace.
    fn render_open_in(
        &self,
        workspace: &Path,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let target = self.state.open_in.resolve(&self.details.editors);
        let label = match &target {
            OpenIn::FileManager => {
                rust_i18n::t!("Details.open_in_file_manager").into_owned()
            }
            OpenIn::Editor(name) => {
                rust_i18n::t!("Details.open_in_editor", name = name)
                    .into_owned()
            }
        };
        let path = workspace.to_path_buf();
        let menu_path = workspace.to_path_buf();
        let editors = self.details.editors.editors().to_vec();
        let view = cx.entity().downgrade();
        details_row("details-open-in", &theme)
            .cursor_pointer()
            .hover(|style| style.bg(theme.accent))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let target = this.state.open_in.clone();
                this.open_workspace_in(&target, &path, cx)
            }))
            .child(Icon::new(LucideIcon::SquareArrowOutUpRight).small())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(Label::new(label).text_sm()),
            )
            .child(
                Button::new("details-open-in-menu")
                    .ghost()
                    .xsmall()
                    .icon(LucideIcon::ChevronDown)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        let choices = std::iter::once(OpenIn::FileManager)
                            .chain(editors.iter().cloned().map(OpenIn::Editor));
                        for choice in choices {
                            let label = match &choice {
                                OpenIn::FileManager => {
                                    rust_i18n::t!("Details.file_manager")
                                        .into_owned()
                                }
                                OpenIn::Editor(name) => name.clone(),
                            };
                            let (view, path) =
                                (view.clone(), menu_path.clone());
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.state.open_in = choice.clone();
                                            this.open_workspace_in(
                                                &choice, &path, cx,
                                            );
                                            this.persist(cx);
                                        });
                                    },
                                ));
                        }
                        menu
                    }),
            )
            .into_any_element()
    }

    /// Opens the panel's Changes tab against the diff base.
    fn open_changes_against_base(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_panel_tab(PanelTabKey::Changes, window, cx);
        self.set_changes_scope(WorkspaceDiffScope::Base, cx);
    }

    /// The first changed files of `workspace` with their line counts, one per line.
    fn changes_breakdown(&self, workspace: &Path) -> String {
        let Some(WorkspaceGitStatus::Repository { changes, .. }) =
            self.git_status(workspace)
        else {
            return String::new();
        };
        let lines: Vec<String> = changes
            .iter()
            .take(12)
            .map(|change| {
                format!(
                    "{}  +{} −{}",
                    change.path,
                    change.insertions.unwrap_or(0),
                    change.deletions.unwrap_or(0)
                )
            })
            .collect();
        lines.join("\n")
    }

    /// The active session's context fill, e.g. `12.3%`.
    fn details_context(
        &self,
        entry: &TabEntry,
        cx: &gpui_kit::App,
    ) -> Option<String> {
        let TabEntry::Session(id) = entry else {
            return None;
        };
        let handle = self.session_handle(id, cx)?;
        let tenths = handle
            .read(cx)
            .projection()
            .controls()
            .context()?
            .percent_tenths()?;
        Some(format!("{}.{}%", tenths / 10, tenths % 10))
    }

    /// The active session's last run usage line.
    fn details_usage(
        &self,
        entry: &TabEntry,
        cx: &gpui_kit::App,
    ) -> Option<String> {
        let view = self.views.get(entry)?;
        let usage = view.read(cx).last_run_usage()?;
        Some(usage.label(None).to_string())
    }
}

/// A details row: icon and text in one line, with the row's id.
fn details_row(
    id: &str,
    theme: &gpui_kit::component::Theme,
) -> impl StatefulInteractiveElement + ParentElement + Styled + IntoElement {
    h_flex()
        .id(SharedString::from(id.to_owned()))
        .test_support()
        .gap_2()
        .px_2()
        .h(rems(ROW_REMS))
        .rounded(theme.radius)
        .text_color(theme.foreground)
}

/// The "choose what to show" menu: one checkable item per details row.
fn details_items_menu(
    menu: PopupMenu,
    view: &WeakEntity<AppView>,
    cx: &gpui_kit::App,
) -> PopupMenu {
    let Some(state) = view.upgrade() else {
        return menu;
    };
    let state = &state.read(cx).state;
    DetailsItem::ALL.into_iter().fold(menu, |menu, item| {
        let visible = state.details_item_visible(item);
        let view = view.clone();
        let label = match item {
            DetailsItem::OpenIn => rust_i18n::t!("Details.item_open_in"),
            DetailsItem::Branch => rust_i18n::t!("Details.item_branch"),
            DetailsItem::Changes => rust_i18n::t!("Details.item_changes"),
            DetailsItem::Relations => rust_i18n::t!("Details.item_relations"),
            DetailsItem::Context => rust_i18n::t!("Details.item_context"),
            DetailsItem::Usage => rust_i18n::t!("Details.item_usage"),
            // Not in `ALL`: only a newer build's saved value reads as Unknown.
            DetailsItem::Unknown => return menu,
        };
        menu.item(
            PopupMenuItem::new(label.into_owned())
                .checked(visible)
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.set_details_item(item, !visible, cx)
                    });
                }),
        )
    })
}

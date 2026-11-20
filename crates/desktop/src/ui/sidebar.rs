//! Sessions sidebar: workspaces with their sessions, recent sessions, and search.
//!
//! [`AppView::sidebar_model`] decides what the sidebar lists; rendering maps
//! that model onto gpui-kit's `Sidebar` components.

use std::path::PathBuf;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem as _, SidebarMenu,
    SidebarMenuItem,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _,
};
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement,
    Keystroke, ParentElement as _, SharedString, StyleRefinement, Styled as _,
    Window, div, prelude::FluentBuilder as _, rems,
};

use super::{AppView, StatusIcon, TabEntry};
use crate::actions::{
    CloseSearch, FocusSearch, NewSession, SEARCH_CONTEXT, WORKSPACE_CONTEXT,
};
use crate::backend::SessionId;
use crate::catalog::{RelativeAge, SessionSort, SessionSummary};
use crate::session::SessionStatus;

/// Sessions listed per workspace before "Show all".
pub const SESSIONS_PER_WORKSPACE: usize = 5;
/// Sessions listed under Recent.
pub const RECENT_SESSIONS: usize = 10;

/// Everything the sidebar lists, decided before rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarModel {
    /// Groups in display order.
    pub groups: Vec<SidebarGroupModel>,
    /// A search is active and matched nothing.
    pub no_matches: bool,
    /// No workspace is registered yet.
    pub no_workspaces: bool,
}

/// One titled group of rows.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarGroupModel {
    /// Which group this is.
    pub kind: GroupKind,
    /// Localized group title.
    pub label: SharedString,
    /// Rows in display order.
    pub rows: Vec<SidebarRow>,
}

/// The sidebar's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// Registered workspaces and their sessions.
    Workspaces,
    /// Newest sessions across workspaces.
    Recent,
    /// Search results.
    Search,
}

/// One sidebar row.
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarRow {
    /// A workspace with its listed sessions.
    Workspace {
        /// Workspace directory.
        path: PathBuf,
        /// Display name.
        name: SharedString,
        /// Whether its sessions are shown.
        expanded: bool,
        /// The directory no longer exists.
        missing: bool,
        /// Session rows and an optional "Show all" row.
        children: Vec<SidebarRow>,
    },
    /// A session.
    Session {
        /// Session identifier.
        id: SessionId,
        /// Title, with the untitled placeholder applied.
        title: SharedString,
        /// Trailing text: relative age, or the workspace name in flat lists.
        trailing: SharedString,
        /// Run status.
        status: SessionStatus,
        /// Whether it is the active tab.
        active: bool,
    },
    /// Reveals the rest of a workspace's sessions.
    ShowAll {
        /// Workspace directory.
        path: PathBuf,
        /// Total number of sessions in the workspace.
        count: usize,
    },
}

/// What a session row shows at its trailing edge.
#[derive(Debug, Clone, Copy)]
enum Trailing {
    /// The relative age when idle.
    Age,
    /// The workspace the session belongs to, for cross-workspace lists.
    Workspace,
}

/// The shortcut shown beside "New session", matching its key binding.
fn new_session_keystroke() -> Keystroke {
    let keys = if cfg!(target_os = "macos") {
        "cmd-n"
    } else {
        "ctrl-n"
    };
    Keystroke::parse(keys).unwrap_or_default()
}

impl SessionSummary {
    /// Builds this session's row from its `status`, read once by the caller.
    fn sidebar_row(
        &self,
        status: SessionStatus,
        trailing: Trailing,
        now_ms: u64,
        active: bool,
    ) -> SidebarRow {
        let trailing = match trailing {
            Trailing::Workspace => {
                crate::workspace::display_name(&self.cwd).into()
            }
            // A status glyph replaces the age while something is happening.
            Trailing::Age if status == SessionStatus::Idle => self
                .modified_at_ms
                .map(|then| RelativeAge::between(now_ms, then).label())
                .unwrap_or_default(),
            Trailing::Age => SharedString::default(),
        };
        SidebarRow::Session {
            id: self.session_id.clone(),
            title: self.display_title(),
            trailing,
            status,
            active,
        }
    }

    /// Title shown in lists, with a placeholder for untitled sessions.
    pub(super) fn display_title(&self) -> SharedString {
        // Tabs and sidebar rows are one line high; agent-set titles may not be.
        match self.title.as_deref() {
            Some(title) => crate::transcript::single_line(title).into_owned(),
            None => rust_i18n::t!("Sidebar.untitled").into_owned(),
        }
        .into()
    }
}

impl AppView {
    /// Status of a session, `Idle` when it is not attached.
    pub fn session_status(&self, id: &SessionId, cx: &App) -> SessionStatus {
        self.session_handle(id, cx)
            .map_or(SessionStatus::Idle, |handle| handle.read(cx).status())
    }

    /// Whether the "running only" filter keeps a session with this status: running or waiting for the user.
    fn passes_filter(&self, status: SessionStatus) -> bool {
        !self.running_only
            || matches!(status, SessionStatus::Running | SessionStatus::Waiting)
    }

    /// The header's filter button: a dropdown with the sort order and the running-only switch.
    fn filter_button(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let (sort, running_only) = (self.sort, self.running_only);
        Button::new("filter-sessions")
            .ghost()
            .small()
            .icon(LucideIcon::ListFilter)
            .tooltip(rust_i18n::t!("Sidebar.filter").into_owned())
            .dropdown_menu(move |menu, _, _| {
                let item = |label: &str, checked: bool| {
                    PopupMenuItem::new(label.to_owned()).checked(checked)
                };
                let (modified, created, only) =
                    (view.clone(), view.clone(), view.clone());
                menu.item(
                    item(
                        &rust_i18n::t!("Sidebar.sort_modified"),
                        sort == SessionSort::Modified,
                    )
                    .on_click(move |_, _, cx| {
                        let _ = modified.update(cx, |this, cx| {
                            this.sort = SessionSort::Modified;
                            cx.notify();
                        });
                    }),
                )
                .item(
                    item(
                        &rust_i18n::t!("Sidebar.sort_created"),
                        sort == SessionSort::Created,
                    )
                    .on_click(move |_, _, cx| {
                        let _ = created.update(cx, |this, cx| {
                            this.sort = SessionSort::Created;
                            cx.notify();
                        });
                    }),
                )
                .separator()
                .item(
                    item(&rust_i18n::t!("Sidebar.running_only"), running_only)
                        .on_click(move |_, _, cx| {
                            let _ = only.update(cx, |this, cx| {
                                this.set_running_only(!this.running_only, cx)
                            });
                        }),
                )
            })
            .into_any_element()
    }

    /// Decides what the sidebar lists from the workspaces, the catalog, and the search query.
    pub fn sidebar_model(&self, cx: &App) -> SidebarModel {
        let now_ms = crate::now_ms();
        let active = match self.active_entry() {
            Some(TabEntry::Session(id)) => Some(id),
            _ => None,
        };
        let row = |summary: &SessionSummary,
                   status: SessionStatus,
                   trailing: Trailing| {
            let active = active.as_ref() == Some(&summary.session_id);
            summary.sidebar_row(status, trailing, now_ms, active)
        };
        let query = self.search.read(cx).value();
        let searching = self.search_open && !query.trim().is_empty();
        if searching {
            let rows: Vec<SidebarRow> = self
                .host
                .catalog
                .search(&query)
                .into_iter()
                .map(|summary| {
                    let status = self.session_status(&summary.session_id, cx);
                    row(summary, status, Trailing::Workspace)
                })
                .collect();
            return SidebarModel {
                no_matches: rows.is_empty(),
                no_workspaces: false,
                groups: vec![SidebarGroupModel {
                    kind: GroupKind::Search,
                    label: rust_i18n::t!("Sidebar.search").into_owned().into(),
                    rows,
                }],
            };
        }
        // Sort the catalog and read each session's status once; every list
        // below filters this one ordering. Filtering before Recent's
        // truncation lets "running only" fill it from older sessions.
        let listed: Vec<(&SessionSummary, SessionStatus)> = self
            .host
            .catalog
            .recent(usize::MAX, self.sort)
            .into_iter()
            .map(|summary| {
                (summary, self.session_status(&summary.session_id, cx))
            })
            .filter(|(_, status)| self.passes_filter(*status))
            .collect();
        let workspaces = self
            .state
            .workspaces
            .entries()
            .iter()
            .map(|entry| {
                // Registry paths and session directories are both normalized on the way in.
                let sessions: Vec<&(&SessionSummary, SessionStatus)> = listed
                    .iter()
                    .filter(|(summary, _)| summary.cwd == entry.path)
                    .collect();
                let limit = if self.show_all.contains(&entry.path) {
                    sessions.len()
                } else {
                    SESSIONS_PER_WORKSPACE
                };
                let mut children: Vec<SidebarRow> = sessions
                    .iter()
                    .take(limit)
                    .map(|(summary, status)| {
                        row(summary, *status, Trailing::Age)
                    })
                    .collect();
                if sessions.len() > limit {
                    children.push(SidebarRow::ShowAll {
                        path: entry.path.clone(),
                        count: sessions.len(),
                    });
                }
                SidebarRow::Workspace {
                    path: entry.path.clone(),
                    name: crate::workspace::display_name(&entry.path).into(),
                    expanded: entry.expanded,
                    missing: self.missing.contains(&entry.path),
                    children,
                }
            })
            .collect();
        let recent: Vec<SidebarRow> = listed
            .iter()
            .take(RECENT_SESSIONS)
            .map(|(summary, status)| row(summary, *status, Trailing::Workspace))
            .collect();
        let mut groups = vec![SidebarGroupModel {
            kind: GroupKind::Workspaces,
            label: rust_i18n::t!("Sidebar.workspaces").into_owned().into(),
            rows: workspaces,
        }];
        if !recent.is_empty() {
            groups.push(SidebarGroupModel {
                kind: GroupKind::Recent,
                label: rust_i18n::t!("Sidebar.recent").into_owned().into(),
                rows: recent,
            });
        }
        SidebarModel {
            groups,
            no_matches: false,
            no_workspaces: self.state.workspaces.entries().is_empty(),
        }
    }

    /// Maps one row model onto sidebar menu items: a workspace row is followed by
    /// its indented session rows while it is expanded.
    ///
    /// Workspaces are flat rows rather than gpui-kit submenus: a submenu keeps its
    /// own open flag, which drifted from the persisted `expanded` (spec §5.4).
    fn menu_items(
        &self,
        row: SidebarRow,
        cx: &mut Context<'_, Self>,
    ) -> Vec<SidebarMenuItem> {
        let item = match row {
            SidebarRow::Session {
                id,
                title,
                trailing,
                status,
                active,
            } => {
                let accessible: SharedString = match status {
                    SessionStatus::Idle => title.clone(),
                    other => format!("{title}, {}", other.label()).into(),
                };
                let view = cx.entity().downgrade();
                let menu_id = id.clone();
                SidebarMenuItem::new(title)
                    .accessibility_label(accessible)
                    .active(active)
                    .context_menu(move |menu, _, _| {
                        let rename = (view.clone(), menu_id.clone());
                        let delete = (view.clone(), menu_id.clone());
                        menu.item(
                            PopupMenuItem::new(
                                rust_i18n::t!("Session.rename").into_owned(),
                            )
                            .on_click(
                                move |_, window, cx| {
                                    let _ = rename.0.update(cx, |this, cx| {
                                        this.rename_session(
                                            &rename.1, window, cx,
                                        )
                                    });
                                },
                            ),
                        )
                        .item(
                            PopupMenuItem::new(
                                rust_i18n::t!("Session.delete").into_owned(),
                            )
                            .on_click(
                                move |_, window, cx| {
                                    let _ = delete.0.update(cx, |this, cx| {
                                        this.delete_session(
                                            &delete.1, window, cx,
                                        )
                                    });
                                },
                            ),
                        )
                    })
                    .suffix(move |_, cx| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(trailing.clone())
                            .child(StatusIcon::new(status))
                    })
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.open_session(&id, cx)
                        },
                    ))
            }
            SidebarRow::ShowAll { path, count } => SidebarMenuItem::new(
                rust_i18n::t!("Sidebar.show_all", count = count).into_owned(),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.show_all.insert(path.clone());
                cx.notify();
            })),
            SidebarRow::Workspace {
                path,
                name,
                expanded,
                missing,
                children,
            } => {
                // Sub-rows indent to the workspace name, past the folder icon (mockup `.row.sub`).
                let children: Vec<SidebarMenuItem> = if expanded {
                    children
                        .into_iter()
                        .flat_map(|child| self.menu_items(child, cx))
                        .map(|child| child.pl(rems(2.)))
                        .collect()
                } else {
                    Vec::new()
                };
                let view = cx.entity().downgrade();
                let menu_path = path.clone();
                let muted = cx.theme().muted_foreground;
                let row = SidebarMenuItem::new(name)
                    .icon(Icon::new(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    }))
                    .when(missing, |this| {
                        this.label_style(
                            StyleRefinement::default().text_color(muted),
                        )
                    })
                    // The chevron is part of the row, so it toggles through the row's click.
                    .suffix(move |_, cx| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(cx.theme().muted_foreground)
                            .when(missing, |this| {
                                this.child(
                                    Label::new(
                                        rust_i18n::t!("Sidebar.missing_folder")
                                            .into_owned(),
                                    )
                                    .text_xs(),
                                )
                            })
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size_4(),
                            )
                    })
                    .context_menu(move |menu, _, _| {
                        let draft = (view.clone(), menu_path.clone());
                        let reveal = (view.clone(), menu_path.clone());
                        let remove = (view.clone(), menu_path.clone());
                        let menu = menu.item(
                            PopupMenuItem::new(
                                rust_i18n::t!("Sidebar.new_session")
                                    .into_owned(),
                            )
                            .on_click(
                                move |_, _, cx| {
                                    let _ = draft.0.update(cx, |this, cx| {
                                        this.open_draft(
                                            Some(draft.1.clone()),
                                            cx,
                                        )
                                    });
                                },
                            ),
                        );
                        // Revealing needs the folder on this machine.
                        let menu =
                            if missing {
                                menu
                            } else {
                                menu.item(
                                    PopupMenuItem::new(
                                        rust_i18n::t!("Workspace.reveal")
                                            .into_owned(),
                                    )
                                    .on_click(move |_, _, cx| {
                                        let _ =
                                            reveal.0.update(cx, |this, cx| {
                                                this.reveal_workspace(
                                                    &reveal.1, cx,
                                                )
                                            });
                                    }),
                                )
                            };
                        menu.separator().item(
                            PopupMenuItem::new(
                                rust_i18n::t!("Workspace.remove").into_owned(),
                            )
                            .on_click(
                                move |_, _, cx| {
                                    let _ = remove.0.update(cx, |this, cx| {
                                        this.remove_workspace(&remove.1, cx)
                                    });
                                },
                            ),
                        )
                    })
                    // `expanded` in desktop.json is the only open state; icon, chevron
                    // and sub-rows are all derived from it on the next render.
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            // Read the live flag: two clicks can land before the next render.
                            let open =
                                this.state.workspaces.entries().iter().any(
                                    |entry| {
                                        entry.path == path && entry.expanded
                                    },
                                );
                            this.state.workspaces.set_expanded(&path, !open);
                            this.persist(cx);
                            cx.notify();
                        },
                    ));
                return std::iter::once(row).chain(children).collect();
            }
        };
        vec![item]
    }

    /// The sidebar header: title, search, "New session", and empty-state notes.
    fn sidebar_header(
        &self,
        no_matches: bool,
        no_workspaces: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .px_1()
                    .child(
                        Label::new(rust_i18n::t!("Rail.sessions").into_owned())
                            .flex_1()
                            .text_base()
                            .font_semibold(),
                    )
                    .child(self.filter_button(cx))
                    .child(
                        Button::new("search-sessions")
                            .ghost()
                            .small()
                            .icon(IconName::Search)
                            .selected(self.search_open)
                            // A real toggle now, so assistive tech hears its pressed state.
                            .toggled(self.search_open)
                            .tooltip_with_action(
                                rust_i18n::t!("Sidebar.search").into_owned(),
                                &FocusSearch,
                                Some(WORKSPACE_CONTEXT),
                            )
                            // The button toggles; Ctrl+K only ever opens or focuses.
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, window, cx| {
                                    if this.search_open {
                                        this.on_close_search(
                                            &CloseSearch,
                                            window,
                                            cx,
                                        )
                                    } else {
                                        this.on_focus_search(
                                            &FocusSearch,
                                            window,
                                            cx,
                                        )
                                    }
                                },
                            )),
                    ),
            )
            .when(self.search_open, |this| {
                // The key context lets Escape in the field close the search.
                this.child(
                    div().key_context(SEARCH_CONTEXT).child(
                        Input::new(&self.search).small().cleanable(true),
                    ),
                )
            })
            // A menu row, so "New session" lines up with the session rows below it.
            .child(
                SidebarMenu::new()
                    .child(
                        SidebarMenuItem::new(
                            rust_i18n::t!("Sidebar.new_session").into_owned(),
                        )
                        .icon(Icon::new(LucideIcon::SquarePen))
                        .suffix(|_, cx| {
                            Kbd::new(new_session_keystroke())
                                .text_color(cx.theme().muted_foreground)
                        })
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewSession), cx)
                        }),
                    )
                    .render("new-session-menu", window, cx),
            )
            .when(no_matches, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .child(
                            Label::new(
                                rust_i18n::t!("Sidebar.no_matches")
                                    .into_owned(),
                            )
                            .text_sm()
                            .text_color(muted),
                        )
                        .child(
                            Button::new("clear-search")
                                .ghost()
                                .small()
                                .label(
                                    rust_i18n::t!("Sidebar.clear_search")
                                        .into_owned(),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.search.update(cx, |search, cx| {
                                            search.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    },
                                )),
                        ),
                )
            })
            .when(no_workspaces, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .child(
                            Label::new(
                                rust_i18n::t!("Sidebar.no_workspaces")
                                    .into_owned(),
                            )
                            .text_sm()
                            .text_color(muted),
                        )
                        .child(
                            Button::new("add-first-workspace")
                                .small()
                                .label(
                                    rust_i18n::t!("Sidebar.add_workspace")
                                        .into_owned(),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.add_workspace(window, cx)
                                    },
                                )),
                        ),
                )
            })
            .into_any_element()
    }

    /// Renders the sidebar on the chrome surface.
    pub(super) fn render_sidebar(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let model = self.sidebar_model(cx);
        let header = self.sidebar_header(
            model.no_matches,
            model.no_workspaces,
            window,
            cx,
        );
        let groups: Vec<SidebarGroup<SidebarMenu>> = model
            .groups
            .into_iter()
            .map(|group| {
                let items: Vec<SidebarMenuItem> = group
                    .rows
                    .into_iter()
                    .flat_map(|row| self.menu_items(row, cx))
                    .collect();
                let group_view = SidebarGroup::new(group.label);
                let group_view = if group.kind == GroupKind::Workspaces {
                    let view = cx.entity().downgrade();
                    group_view.suffix(move |_, _| {
                        let view = view.clone();
                        Button::new("add-workspace")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip(
                                rust_i18n::t!("Sidebar.add_workspace")
                                    .into_owned(),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.add_workspace(window, cx)
                                });
                            })
                    })
                } else {
                    group_view
                };
                group_view.child(SidebarMenu::new().children(items))
            })
            .collect();
        Sidebar::new("sessions-sidebar")
            .w_full()
            .h_full()
            // The chrome surface: one tone for title bar and sidebar (spec §6.1).
            .bg(cx.theme().title_bar)
            .border_r_0()
            .collapsible(SidebarCollapsible::None)
            .header(header)
            .children(groups)
    }
}

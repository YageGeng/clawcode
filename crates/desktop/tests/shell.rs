//! Workspace shell: sidebar grouping, tabs, keyboard commands, and startup states.

mod support;

use std::path::{Path, PathBuf};

use desktop::backend::BackendError;
use desktop::catalog::SessionCatalog;
use desktop::state::DesktopState;
use desktop::ui::{GroupKind, SessionRow, SidebarRow, TabEntry, WorkspaceRow};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::shell::{
    id, model, open, session, shows, titles, with_workspaces,
};

#[gpui_kit::test]
fn sidebar_groups_sessions_by_workspace(cx: &mut TestAppContext) {
    let (_handle, view, _dir) = open(cx, with_workspaces(&["/w/a", "/w/b"]));
    let mut sessions: Vec<_> = (0..7)
        .map(|n| {
            session(
                &format!("a{n}"),
                "/w/a",
                Some(&format!("task {n}")),
                100 + n,
            )
        })
        .collect();
    sessions.push(session("b0", "/w/b", Some("other task"), 50));
    let mut child = session("c0", "/w/a", Some("child agent"), 999);
    child.parent_session_id = Some(id("a6"));
    sessions.push(child);
    view.update(cx, |view, cx| {
        view.replace_catalog(sessions.into_iter().collect(), cx)
    });

    let model = model(cx, &view);

    let workspaces = &model.groups[0];
    assert_eq!(workspaces.kind, GroupKind::Workspaces);
    let SidebarRow::Workspace(WorkspaceRow { path, children, .. }) =
        &workspaces.rows[0]
    else {
        panic!("first row is a workspace");
    };
    assert_eq!(path, Path::new("/w/a"));
    assert_eq!(
        titles(children),
        ["task 6", "task 5", "task 4", "task 3", "task 2"]
    );
    assert!(children.contains(&SidebarRow::ShowAll {
        path: PathBuf::from("/w/a"),
        count: 7
    }));
    let SidebarRow::Workspace(WorkspaceRow {
        children: other, ..
    }) = &workspaces.rows[1]
    else {
        panic!("second row is a workspace");
    };
    assert_eq!(titles(other), ["other task"]);
    let recent = &model.groups[1];
    assert_eq!(recent.kind, GroupKind::Recent);
    assert_eq!(recent.rows.len(), 8);
    assert!(!titles(&recent.rows).contains(&"child agent".to_string()));
}

#[gpui_kit::test]
fn show_all_lists_every_session_of_the_workspace(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    let sessions: Vec<_> = (0..7)
        .map(|n| {
            session(
                &format!("a{n}"),
                "/w/a",
                Some(&format!("task {n}")),
                100 + n,
            )
        })
        .collect();
    view.update(cx, |view, cx| {
        view.replace_catalog(sessions.into_iter().collect(), cx)
    });

    // Row 0 is the workspace; rows 1-5 its sessions; row 6 is "Show all 7".
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("0-0-6", cx);
    })
    .expect("window");

    let model = model(cx, &view);
    let SidebarRow::Workspace(WorkspaceRow { children, .. }) =
        &model.groups[0].rows[0]
    else {
        panic!("first row is a workspace");
    };
    assert_eq!(titles(children).len(), 7);
}

#[gpui_kit::test]
fn clicking_a_session_opens_one_tab_and_reuses_it(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("s1", "/w/a", Some("refactor kernel"), 1)]
                .into_iter()
                .collect(),
            cx,
        )
    });

    // "0-0-1" is the first session row, right below the first workspace row.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("0-0-1", cx);
        window.render_frame(cx);
        window.click("0-0-1", cx);
    })
    .expect("window");

    cx.update(|cx| {
        assert_eq!(view.read(cx).tabs(), &[TabEntry::Session(id("s1"))]);
        assert_eq!(view.read(cx).active_tab(), Some(0));
    });
}

#[gpui_kit::test]
fn close_tab_activates_the_right_neighbor(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        let catalog: SessionCatalog = ["s1", "s2", "s3"]
            .into_iter()
            .map(|name| session(name, "/w/a", Some(name), 1))
            .collect();
        view.replace_catalog(catalog, cx);
        for name in ["s1", "s2", "s3"] {
            view.open_session(&id(name), cx);
        }
        view.open_session(&id("s2"), cx);
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-w", cx);
    })
    .expect("window");

    cx.update(|cx| {
        assert_eq!(
            view.read(cx).tabs(),
            &[TabEntry::Session(id("s1")), TabEntry::Session(id("s3"))]
        );
        assert_eq!(view.read(cx).active_tab(), Some(1));
    });
}

#[gpui_kit::test]
fn secondary_b_toggles_the_sidebar(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-b", cx);
    })
    .expect("window");
    assert!(!cx.update(|cx| view.read(cx).state().layout.sidebar_open));

    cx.update_window(handle, |_, window, cx| window.press("ctrl-b", cx))
        .expect("window");
    assert!(cx.update(|cx| view.read(cx).state().layout.sidebar_open));
}

#[gpui_kit::test]
fn new_session_opens_a_draft_for_the_selected_workspace(
    cx: &mut TestAppContext,
) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/clawcode"]));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-n", cx);
        window.render_frame(cx);
        assert!(shows(window, "What should we build in clawcode?"));
    })
    .expect("window");

    cx.update(|cx| {
        assert_eq!(
            view.read(cx).tabs().last(),
            Some(&TabEntry::Draft {
                workspace: Some(PathBuf::from("/w/clawcode"))
            })
        );
    });
}

#[gpui_kit::test]
fn startup_failure_shows_the_error_card(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, DesktopState::default());

    view.update(cx, |view, cx| {
        view.set_backend(
            Err(BackendError::Startup("config missing".to_string())),
            cx,
        )
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("startup-error").visible());
        assert!(shows(window, "config missing"));
        assert!(window.find("retry").visible());
        assert!(window.find("open-config").visible());
    })
    .expect("window");
}

#[gpui_kit::test]
fn search_filters_sessions_and_reports_no_matches(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [
                session("k", "/w/a", Some("refactor kernel"), 2),
                session("u", "/w/a", Some("update docs"), 1),
            ]
            .into_iter()
            .collect(),
            cx,
        )
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-k", cx);
        window.input("KERNEL", cx);
    })
    .expect("window");
    let found = model(cx, &view);
    assert_eq!(found.groups[0].kind, GroupKind::Search);
    assert_eq!(titles(&found.groups[0].rows), ["refactor kernel"]);

    cx.update_window(handle, |_, window, cx| {
        window.input("zzz", cx);
        window.render_frame(cx);
        assert!(shows(window, "No matching sessions"));
    })
    .expect("window");
    assert!(model(cx, &view).no_matches);
}

#[gpui_kit::test]
fn blank_titles_render_as_untitled(cx: &mut TestAppContext) {
    let (_handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("n", "/w/a", None, 1)].into_iter().collect(),
            cx,
        )
    });

    let model = model(cx, &view);

    assert_eq!(titles(&model.groups[1].rows), ["Untitled session"]);
}

#[gpui_kit::test]
fn running_only_hides_idle_sessions(cx: &mut TestAppContext) {
    let live = support::shell::live(cx, DesktopState::default());
    let idle = live.open_new_session(cx);
    let running = live.open_new_session(cx);
    let client = live.backend.client.clone();
    let target = running.clone();
    let prompt = live.backend.runtime.spawn(async move {
        client.prompt(&target, vec!["slow".into()]).await
    });
    support::shell::eventually(cx, "run started", |cx| {
        live.view.read(cx).session_status(&running, cx)
            == desktop::session::SessionStatus::Running
    });
    live.view
        .update(cx, |view, cx| view.set_running_only(true, cx));

    let model = model(cx, &live.view);

    let SidebarRow::Workspace(WorkspaceRow { children, .. }) =
        &model.groups[0].rows[0]
    else {
        panic!("a workspace row");
    };
    assert_eq!(children.len(), 1, "only the running session remains");
    assert!(
        matches!(&children[0], SidebarRow::Session(SessionRow { id, .. }) if *id == running)
    );
    assert!(model.groups[1].rows.iter().all(
        |row| !matches!(row, SidebarRow::Session(SessionRow { id, .. }) if *id == idle)
    ));
    drop(prompt);
}

#[gpui_kit::test]
fn header_shows_running_elapsed(cx: &mut TestAppContext) {
    let live = support::shell::live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    let client = live.backend.client.clone();
    let _prompt = live
        .backend
        .runtime
        .spawn(async move { client.prompt(&id, vec!["slow".into()]).await });

    live.until(cx, "running header with elapsed", |window| {
        support::shell::painted(window)
            .iter()
            .any(|text| text.text().starts_with("Running · "))
    });
}

/// Opens `s1`, `s2`, `s3` as tabs with `s2` active.
fn three_tabs(
    cx: &mut TestAppContext,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<desktop::ui::AppView>,
    tempfile::TempDir,
) {
    let (handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        let catalog: SessionCatalog = ["s1", "s2", "s3"]
            .into_iter()
            .map(|name| session(name, "/w/a", Some(name), 1))
            .collect();
        view.replace_catalog(catalog, cx);
        for name in ["s1", "s2", "s3"] {
            view.open_session(&id(name), cx);
        }
        view.open_session(&id("s2"), cx);
    });
    (handle, view, dir)
}

#[gpui_kit::test]
fn move_tab_reorders_and_keeps_the_active_entry(cx: &mut TestAppContext) {
    let (_handle, view, _dir) = three_tabs(cx);
    view.update(cx, |view, cx| view.move_tab(0, 2, cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(
            view.tabs(),
            &[
                TabEntry::Session(id("s2")),
                TabEntry::Session(id("s3")),
                TabEntry::Session(id("s1"))
            ]
        );
        assert_eq!(view.active_tab(), Some(0), "s2 stays active");
        assert_eq!(view.state().tabs.open, ["s2", "s3", "s1"]);
        assert_eq!(view.state().tabs.active, Some(0));
    });
}

#[gpui_kit::test]
fn dragging_a_tab_onto_another_moves_it(cx: &mut TestAppContext) {
    let (handle, view, _dir) = three_tabs(cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to("tab:session:s3", "tab:session:s1", cx);
    })
    .expect("window");
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(
            view.tabs(),
            &[
                TabEntry::Session(id("s3")),
                TabEntry::Session(id("s1")),
                TabEntry::Session(id("s2"))
            ]
        );
        assert_eq!(view.active_tab(), Some(2), "s2 stays active");
    });
}

#[gpui_kit::test]
fn tab_buttons_act_on_their_own_tab_after_a_reorder(cx: &mut TestAppContext) {
    use gpui_kit::{
        InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent,
    };

    let (handle, view, _dir) = three_tabs(cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let position = window.find("close-tab:session:s3").bounds().center();
        // The strip changes before the next frame: the painted button is still s3's.
        view.update(cx, |view, cx| view.move_tab(2, 0, cx));
        // Raw events, so no frame is drawn between the reorder and the click.
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .expect("window");
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).tabs(),
            &[TabEntry::Session(id("s1")), TabEntry::Session(id("s2"))],
            "s3 closed, not the tab that moved into its slot"
        );
    });
}

#[gpui_kit::test]
fn a_config_loaded_at_launch_that_failed_shows_the_error_card(
    cx: &mut TestAppContext,
) {
    let (handle, view, _dir) = open(cx, DesktopState::default());

    view.update(cx, |view, cx| {
        view.start_backend_with(
            Err(config::ConfigError::NotFound("no config here".to_string())),
            cx,
        )
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("startup-error").visible());
        assert!(shows(window, "no config here"));
    })
    .expect("window");
}

#[gpui_kit::test]
fn the_ticker_redraws_only_a_running_transcript(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let live = support::shell::live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    // The first prompt names the session, which refreshes the shell's list;
    // run it first so only the ticker is left to redraw anything below.
    live.run(cx, &id, vec!["hello".into()]);
    support::shell::eventually(cx, "titled session listed", |cx| {
        live.view
            .read(cx)
            .sidebar_model(cx)
            .groups
            .iter()
            .any(|group| support::shell::titles(&group.rows) == ["hello"])
    });
    // Views are created by the first frame that shows them.
    live.window(cx, |_, _| ());
    let transcript = cx
        .update(|cx| live.view.read(cx).active_view().cloned())
        .expect("session view");
    // `[shell, transcript]` notifications.
    let counts = Rc::new(RefCell::new([0usize; 2]));
    let _observers = cx.update(|cx| {
        let (shell, rows) = (Rc::clone(&counts), Rc::clone(&counts));
        [
            cx.observe(&live.view, move |_, _| shell.borrow_mut()[0] += 1),
            cx.observe(&transcript, move |_, _| rows.borrow_mut()[1] += 1),
        ]
    });
    let tick = |cx: &mut TestAppContext| {
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
    };

    tick(cx);
    assert_eq!(
        *counts.borrow(),
        [0, 0],
        "an idle shell waits for the minute"
    );

    let client = live.backend.client.clone();
    let prompt_id = id.clone();
    let _prompt = live.backend.runtime.spawn(async move {
        client.prompt(&prompt_id, vec!["slower".into()]).await
    });
    support::shell::eventually(cx, "running", |cx| {
        live.view.read(cx).session_status(&id, cx)
            == desktop::session::SessionStatus::Running
    });
    *counts.borrow_mut() = [0, 0];
    tick(cx);
    let [shell, rows] = *counts.borrow();
    assert!(rows >= 1, "the running transcript redraws every second");
    // The header and sidebar repaint as the view's ancestors, without a notify.
    assert_eq!(shell, 0, "the shell itself is not notified");
}

#[gpui_kit::test]
fn activating_a_tab_scrolls_it_into_view(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    let names: Vec<String> = (0..16).map(|n| format!("s{n}")).collect();
    view.update(cx, |view, cx| {
        let catalog: SessionCatalog = names
            .iter()
            .map(|name| session(name, "/w/a", Some(name), 1))
            .collect();
        view.replace_catalog(catalog, cx);
        view.open_session(&id("s0"), cx);
    });
    // The strip measures itself in a first frame, as it has by the time a user opens tabs.
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    view.update(cx, |view, cx| {
        for name in &names {
            view.open_session(&id(name), cx);
        }
    });
    // The strip overflows; the newest, active tab is scrolled into view.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.find("tab:session:s15").visible(), "last tab visible");
        assert!(!window.find("tab:session:s0").visible(), "strip overflows");
    })
    .expect("window");
    view.update(cx, |view, cx| view.open_session(&id("s0"), cx));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.find("tab:session:s0").visible(), "first tab visible");
    })
    .expect("window");
}

/// Visible rows in the first sidebar group (the workspaces): workspace rows plus their session rows.
///
/// Menu rows register under positional ids `0-0-…`, whatever their nesting.
fn workspace_group_rows(window: &gpui_kit::Window) -> usize {
    gpui_kit::base::test_support::snapshots(window)
        .iter()
        .filter(|element| {
            element.visible()
                && element
                    .path()
                    .last()
                    .is_some_and(|id| id.to_string().starts_with("0-0-"))
        })
        .count()
}

#[gpui_kit::test]
fn search_button_toggles_the_field(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("k", "/w/a", Some("refactor kernel"), 1)]
                .into_iter()
                .collect(),
            cx,
        )
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("search-sessions", cx);
        window.input("kernel", cx);
        window.render_frame(cx);
        assert_eq!(window.find("search-sessions").checked(), Some(true));
    })
    .expect("window");
    assert_eq!(model(cx, &view).groups[0].kind, GroupKind::Search);

    // A second click closes the field and drops the query.
    cx.update_window(handle, |_, window, cx| {
        window.click("search-sessions", cx);
        window.render_frame(cx);
        assert_eq!(window.find("search-sessions").checked(), Some(false));
    })
    .expect("window");
    assert_eq!(model(cx, &view).groups[0].kind, GroupKind::Workspaces);

    // Ctrl+K only opens or focuses; it never closes. The reopened field starts empty.
    cx.update_window(handle, |_, window, cx| {
        window.press("ctrl-k", cx);
        window.press("ctrl-k", cx);
        window.render_frame(cx);
        assert_eq!(window.find("search-sessions").checked(), Some(true));
    })
    .expect("window");
    assert_eq!(model(cx, &view).groups[0].kind, GroupKind::Workspaces);
    cx.update_window(handle, |_, window, cx| window.input("kernel", cx))
        .expect("window");
    assert_eq!(model(cx, &view).groups[0].kind, GroupKind::Search);

    // Escape in the field closes it too.
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(window.find("search-sessions").checked(), Some(false));
    })
    .expect("window");
    assert_eq!(model(cx, &view).groups[0].kind, GroupKind::Workspaces);
}

#[gpui_kit::test]
fn clicking_a_workspace_row_toggles_its_sessions(cx: &mut TestAppContext) {
    let (handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("s1", "/w/a", Some("refactor kernel"), 1)]
                .into_iter()
                .collect(),
            cx,
        )
    });
    let expanded = |cx: &mut TestAppContext| {
        cx.update(|cx| view.read(cx).state().workspaces.entries()[0].expanded)
    };

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(workspace_group_rows(window), 2, "row and its session");
        // "0-0-0" is the first workspace row.
        window.click("0-0-0", cx);
        window.render_frame(cx);
        assert_eq!(workspace_group_rows(window), 1, "row only");
    })
    .expect("window");
    assert!(!expanded(cx));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    let saved = DesktopState::load(&dir.path().join("desktop.json"));
    assert!(!saved.workspaces.entries()[0].expanded, "persisted");

    cx.update_window(handle, |_, window, cx| {
        // "0-0-0" is the first workspace row.
        window.click("0-0-0", cx);
        window.render_frame(cx);
        assert_eq!(workspace_group_rows(window), 2, "row and its session");
    })
    .expect("window");
    assert!(expanded(cx));

    // The chevron at the row's trailing edge toggles the same state.
    cx.update_window(handle, |_, window, cx| {
        let row = window.find("0-0-0").bounds();
        let chevron = gpui_kit::point(
            row.size.width - gpui_kit::px(16.),
            gpui_kit::px(14.),
        );
        window.click_at("0-0-0", chevron, cx);
        window.render_frame(cx);
        assert_eq!(workspace_group_rows(window), 1, "row only");
    })
    .expect("window");
    assert!(!expanded(cx));
}

#[gpui_kit::test]
fn rail_is_an_inset_card_aligned_with_the_content_card(
    cx: &mut TestAppContext,
) {
    let (handle, _view, _dir) = open(cx, with_workspaces(&["/w/a"]));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let rail = window.find("rail").bounds();
        let card = window.find("content-card").bounds();
        let gap = gpui_kit::px(8.);
        assert_eq!(rail.left(), gap, "spacing sm from the window's left edge");
        assert_eq!(
            window.viewport_size().height - rail.bottom(),
            gap,
            "spacing sm from the window's bottom edge"
        );
        assert_eq!(rail.top(), card.top(), "tops line up");
        assert_eq!(rail.bottom(), card.bottom(), "bottoms line up");
    })
    .expect("window");
}

//! The right panel: per-session tabs for files, file previews, and changes,
//! shown beside the main card or as a sheet when the window is narrow.

mod support;

use std::path::PathBuf;

use desktop::state::{DesktopState, PanelTabKey};
use desktop::ui::{PanelPresentation, SessionViewEvent, TabEntry};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, SharedString, TestAppContext, px, size};
use support::shell::{
    Live, eventually, git, live, live_in_git_repo, row_texts, shows,
};

/// The fixture workspace of a live shell.
fn root(live: &Live) -> PathBuf {
    live.backend.workspace.path().to_path_buf()
}

/// A live shell with its fixture workspace registered and one session open.
fn with_session(live: Live, cx: &mut TestAppContext) -> Live {
    live.add_workspace(cx, &root(&live));
    live.open_new_session(cx);
    live
}

/// The active owner's panel tabs and active index.
fn panel(
    live: &Live,
    cx: &App,
) -> Option<(bool, Vec<PanelTabKey>, Option<usize>)> {
    live.view
        .read(cx)
        .panel()
        .map(|panel| (panel.open, panel.tabs.clone(), panel.active))
}

/// Whether the active owner's panel is open; an owner without saved state has a closed panel.
fn is_open(live: &Live, cx: &App) -> bool {
    live.view.read(cx).panel().is_some_and(|panel| panel.open)
}

/// The text a preview tab holds, once it is loaded as text.
fn preview_text(live: &Live, cx: &App, path: &str) -> Option<String> {
    let view = live.view.read(cx).panel_file_view(path)?;
    Some(view.read(cx).editor()?.read(cx).value().to_string())
}

/// Clicks an element by id in a fresh frame.
fn click(live: &Live, cx: &mut TestAppContext, id: &str) {
    let id = SharedString::from(id.to_string());
    live.window(cx, |window, cx| window.click(id, cx));
}

#[gpui_kit::test]
fn ctrl_shift_e_opens_files_and_a_file_preview(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    std::fs::create_dir(root(&live).join("src")).expect("dir");
    std::fs::write(root(&live).join("src/lib.rs"), "pub fn a() {}")
        .expect("file");
    let tabs = cx.update(|cx| live.view.read(cx).tabs().len());

    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((true, vec![PanelTabKey::Files], Some(0)))
        )
    });
    live.until(cx, "file tree", |window| {
        window.try_find("file-node-src").is_some()
    });
    click(&live, cx, "file-node-src");
    live.until(cx, "expanded", |window| {
        window.try_find("file-node-src/lib.rs").is_some()
    });
    click(&live, cx, "file-node-src/lib.rs");
    cx.update(|cx| {
        let (_, tabs_now, active) = panel(&live, cx).expect("panel");
        assert_eq!(
            tabs_now,
            [
                PanelTabKey::Files,
                PanelTabKey::File {
                    path: "src/lib.rs".to_string()
                }
            ]
        );
        assert_eq!(active, Some(1));
        assert_eq!(
            live.view.read(cx).tabs().len(),
            tabs,
            "main tabs unchanged"
        );
    });
    eventually(cx, "preview loaded", |cx| {
        preview_text(&live, cx, "src/lib.rs").as_deref()
            == Some("pub fn a() {}")
    });
}

#[gpui_kit::test]
fn binary_and_missing_files_show_notices(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    std::fs::write(root(&live).join("logo.png"), [0u8, 1, 2]).expect("binary");
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(
                PanelTabKey::File {
                    path: "logo.png".to_string(),
                },
                window,
                cx,
            )
        })
    });
    live.until(cx, "binary notice", |window| {
        shows(window, "Binary file — no preview")
    });
}

#[gpui_kit::test]
fn files_tab_shows_an_error_for_a_deleted_workspace(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let gone = tempfile::tempdir().expect("gone");
    let path = gone.path().to_path_buf();
    live.add_workspace(cx, &path);
    cx.update(|cx| {
        live.view
            .update(cx, |view, cx| view.open_draft(Some(path.clone()), cx))
    });
    drop(gone);

    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    live.until(cx, "error", |window| {
        window.try_find("files-retry").is_some()
    });
}

#[gpui_kit::test]
fn panel_state_is_per_session_and_survives_a_restart(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    let a = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    let b = live.open_new_session(cx);
    cx.update(|cx| assert!(!is_open(&live, cx)));
    live.open(cx, &a);
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((true, vec![PanelTabKey::Files], Some(0)))
        )
    });

    let state = cx.update(|cx| live.view.read(cx).state().clone());
    assert!(state.panels.contains_key(&format!("session:{a}")));
    assert!(
        !state
            .panels
            .get(&format!("session:{b}"))
            .is_some_and(|panel| panel.open)
    );
    let (_handle, view, _dir) = support::shell::open(cx, state);
    view.update(cx, |view, cx| view.open_session(&a, cx));
    cx.update(|cx| {
        let panel = view.read(cx).panel().expect("restored panel");
        assert!(panel.open);
        assert_eq!(panel.tabs, [PanelTabKey::Files]);
    });
}

#[gpui_kit::test]
fn ctrl_j_toggles_and_defaults_to_changes(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    live.window(cx, |window, cx| window.press("ctrl-j", cx));
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((true, vec![PanelTabKey::Changes], Some(0)))
        )
    });
    live.window(cx, |window, cx| window.press("ctrl-j", cx));
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((false, vec![PanelTabKey::Changes], Some(0)))
        )
    });
}

#[gpui_kit::test]
fn ctrl_j_opens_files_outside_git(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    eventually(cx, "status read", |cx| {
        live.view.read(cx).is_repository(&root(&live)) == Some(false)
    });
    live.window(cx, |window, cx| window.press("ctrl-j", cx));
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((true, vec![PanelTabKey::Files], Some(0)))
        )
    });
}

#[gpui_kit::test]
fn narrow_window_turns_the_panel_into_a_sheet_and_back(
    cx: &mut TestAppContext,
) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    let presentation = |live: &Live, cx: &mut TestAppContext| {
        live.window(cx, |window, cx| {
            live.view.read(cx).panel_presentation(window)
        })
    };
    assert_eq!(presentation(&live, cx), PanelPresentation::Inline);
    live.window(cx, |window, _| {
        assert!(window.try_find("right-panel").is_some())
    });

    cx.simulate_window_resize(live.handle, size(px(960.), px(640.)));
    assert_eq!(presentation(&live, cx), PanelPresentation::Sheet);
    live.window(cx, |window, _| {
        let sheet = window.find("panel-sheet").bounds();
        let panel = window.find("right-panel").bounds();
        // The sheet's card sits over the main card at the window's right edge.
        assert!(sheet.contains(&panel.origin));
        assert_eq!(panel.right(), sheet.right() - px(8.));
    });

    cx.simulate_window_resize(live.handle, size(px(1280.), px(820.)));
    assert_eq!(presentation(&live, cx), PanelPresentation::Inline);
    cx.update(|cx| {
        assert_eq!(
            panel(&live, cx),
            Some((true, vec![PanelTabKey::Files], Some(0)))
        )
    });
}

#[gpui_kit::test]
fn draft_panel_follows_its_workspace_and_moves_to_the_session(
    cx: &mut TestAppContext,
) {
    let live = live(cx, DesktopState::default());
    let w1 = root(&live);
    let other = tempfile::tempdir().expect("w2");
    let w2 = other.path().to_path_buf();
    live.add_workspace(cx, &w2);
    live.add_workspace(cx, &w1);
    cx.update(|cx| {
        live.view
            .update(cx, |view, cx| view.open_draft(Some(w1.clone()), cx))
    });
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    let select = |live: &Live, cx: &mut TestAppContext, workspace: &PathBuf| {
        let draft = cx.update(|cx| {
            live.view
                .read(cx)
                .active_view()
                .cloned()
                .expect("draft view")
        });
        let workspace = workspace.clone();
        draft.update(cx, |_, cx| {
            cx.emit(SessionViewEvent::SelectWorkspace(workspace))
        });
        live.window(cx, |_, _| ());
    };
    select(&live, cx, &w2);
    cx.update(|cx| assert!(!is_open(&live, cx)));
    select(&live, cx, &w1);
    cx.update(|cx| assert!(is_open(&live, cx)));

    // The first message turns the draft into a session that keeps the panel.
    live.window(cx, |window, cx| {
        let composer =
            live.view.read(cx).active_view().cloned().expect("draft");
        composer.update(cx, |view, cx| {
            view.composer()
                .update(cx, |composer, cx| composer.focus(window, cx))
        });
        window.input("hello", cx);
        window.press("enter", cx);
    });
    eventually(cx, "session created", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });
    cx.update(|cx| {
        let view = live.view.read(cx);
        let [TabEntry::Session(id)] = view.tabs() else {
            panic!("tabs")
        };
        let panels = &view.state().panels;
        assert!(
            panels
                .get(&format!("session:{id}"))
                .is_some_and(|panel| panel.open)
        );
        assert!(!panels.contains_key(&format!("draft:{}", w1.display())));
    });
}

#[gpui_kit::test]
fn deleted_sessions_lose_their_panel_state(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    let id = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    // An open, attached session keeps its catalog entry; a deleted one has no tab.
    cx.update(|cx| {
        live.view.update(cx, |view, cx| {
            view.close_tab(0, cx);
            view.replace_catalog(
                desktop::catalog::SessionCatalog::default(),
                cx,
            )
        })
    });
    cx.update(|cx| {
        let view = live.view.read(cx);
        assert!(!view.state().panels.contains_key(&format!("session:{id}")))
    });
}

#[gpui_kit::test]
fn maximize_hides_the_main_card(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    click(&live, cx, "panel-maximize");
    live.window(cx, |window, _| {
        assert!(window.try_find("content-card").is_none());
        assert!(window.try_find("right-panel").is_some());
    });
    click(&live, cx, "panel-maximize");
    live.window(cx, |window, _| {
        assert!(window.try_find("content-card").is_some())
    });
}

#[gpui_kit::test]
fn files_load_once_the_backend_starts_after_the_first_frame(
    cx: &mut TestAppContext,
) {
    // The app restarts with a draft's Files panel open and draws before its backend is ready.
    let mut backend = support::ScriptedBackend::start();
    std::fs::write(backend.workspace.path().join("early.rs"), "")
        .expect("file");
    let workspace = backend.workspace.path().to_path_buf();
    let state = support::shell::fixture_state(&backend);
    let (handle, view, dir) = support::shell::open(cx, state);
    view.update(cx, |view, cx| view.open_draft(Some(workspace.clone()), cx));
    gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.open_panel_tab(PanelTabKey::Files, window, cx)
        });
        window.render_frame(cx)
    })
    .expect("first frame");
    support::shell::connect(cx, &view, &mut backend);
    let live = Live::builder()
        .handle(handle)
        .view(view)
        .backend(backend)
        ._dir(dir)
        .build();

    live.until(cx, "file tree", |window| {
        window.try_find("file-node-early.rs").is_some()
    });
}

/// Renders frames for a while, failing as soon as `check` stops holding.
fn holds(
    live: &Live,
    cx: &mut TestAppContext,
    what: &str,
    check: impl Fn(&App) -> bool,
) {
    for _ in 0..30 {
        live.window(cx, |_, _| ());
        cx.update(|cx| assert!(check(cx), "{what} no longer holds"));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[gpui_kit::test]
fn returning_to_an_unchanged_preview_keeps_its_place(cx: &mut TestAppContext) {
    use gpui_kit::component::input::Position;

    let live = with_session(live(cx, DesktopState::default()), cx);
    let text: String = (0..60).map(|n| format!("line {n}\n")).collect();
    std::fs::write(root(&live).join("long.txt"), &text).expect("file");
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(
                PanelTabKey::File {
                    path: "long.txt".to_string(),
                },
                window,
                cx,
            )
        })
    });
    eventually(cx, "text loaded", |cx| {
        preview_text(&live, cx, "long.txt").as_deref() == Some(text.as_str())
    });
    let editor = cx.update(|cx| {
        live.view
            .read(cx)
            .panel_file_view("long.txt")
            .and_then(|view| view.read(cx).editor().cloned())
            .expect("editor")
    });
    live.window(cx, |window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_cursor_position(Position::new(40, 2), window, cx)
        })
    });

    // Away to the Files tab and back: the file did not change, so neither does the view.
    click(&live, cx, "panel-tab:files");
    click(&live, cx, "panel-tab:file:long.txt");
    holds(&live, cx, "cursor on line 40", |cx| {
        editor.read(cx).cursor_position().line == 40
    });
}

/// Whether rows of the `n`-th file of the Changes diff are in view; gpui-kit
/// marks each file's rows with the id `("diff-file", n)`.
fn diff_file_shown(window: &gpui_kit::Window, n: u64) -> bool {
    let file = gpui_kit::ElementId::NamedInteger("diff-file".into(), n);
    gpui_kit::base::test_support::snapshots(window)
        .iter()
        .any(|element| element.path().contains(&file))
}

#[gpui_kit::test]
fn changes_tab_lists_files_and_reveals_one(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    // Enough changed lines in a.rs that new.rs starts below the fold.
    let long: String = (0..200).map(|n| format!("changed {n}\n")).collect();
    std::fs::write(root(&live).join("a.rs"), long).expect("edit");
    std::fs::write(root(&live).join("new.rs"), "fresh\n").expect("new");

    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "change rows", |window| {
        window.try_find("changed-file-1").is_some()
    });
    live.window(cx, |window, _| {
        assert!(
            row_texts(window, "changed-file-0").contains(&"a.rs".to_string())
        );
        assert!(
            row_texts(window, "changed-file-1").contains(&"new.rs".to_string())
        );
    });
    // Both headers start as placeholders in view; once a.rs's long patch
    // loads, it pushes new.rs below the fold. gpui-kit marks the rows of the
    // n-th file in view as ("diff-file", n).
    live.until(cx, "a.rs loaded", |window| {
        diff_file_shown(window, 0) && !diff_file_shown(window, 1)
    });
    click(&live, cx, "changed-file-1");
    live.until(cx, "revealed", |window| diff_file_shown(window, 1));
}

#[gpui_kit::test]
fn scope_switch_shows_only_unstaged_lines(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    std::fs::write(root(&live).join("a.rs"), "staged\n").expect("edit");
    git(&root(&live), &["add", "a.rs"]);
    std::fs::write(root(&live).join("a.rs"), "staged\nloose\n").expect("edit");
    std::fs::write(root(&live).join("notes.txt"), "beta\n").expect("edit");
    git(&root(&live), &["add", "notes.txt"]);

    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "base scope", |window| {
        window.try_find("changed-file-1").is_some()
    });
    click(&live, cx, "changes-scope-unstaged");
    live.until(cx, "unstaged scope", |window| {
        window.try_find("changed-file-1").is_none()
            && window.try_find("changed-file-0").is_some()
    });
    live.window(cx, |window, _| {
        assert!(
            row_texts(window, "changed-file-0").contains(&"a.rs".to_string())
        );
        assert_eq!(row_texts(window, "changes-totals"), ["+1", "−0"]);
    });
}

#[gpui_kit::test]
fn not_a_repository_shows_a_notice(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(PanelTabKey::Changes, window, cx)
        })
    });
    live.until(cx, "notice", |window| {
        shows(window, "This workspace is not a Git repository")
    });
}

#[gpui_kit::test]
fn a_finished_run_refreshes_open_changes_tabs(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    live.add_workspace(cx, &root(&live));
    let id = live.open_new_session(cx);
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "clean", |window| shows(window, "No changes"));

    // The scripted `edit` tool rewrites notes.txt in the workspace.
    live.run(cx, &id, vec!["edit".into()]);
    live.until(cx, "refreshed", |window| {
        window.try_find("changed-file-0").is_some()
    });
}

#[gpui_kit::test]
fn a_diff_cut_at_the_size_limit_renders_with_a_note(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    let line =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    // 1.5 MiB: past the 1 MiB limit of one file's patch.
    std::fs::write(root(&live).join("big.txt"), line.repeat(24 * 1024))
        .expect("large file");
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "truncated note", |window| {
        shows(
            window,
            "This file is too large to show in full; the counts include every change.",
        )
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("changed-file-0").is_some())
    });
}

#[gpui_kit::test]
fn a_long_change_list_builds_only_the_visible_rows(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    for n in 0..300 {
        std::fs::write(root(&live).join(format!("new-{n:03}.rs")), "x\n")
            .expect("file");
    }
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "change row", |window| {
        window.try_find("changed-file-0").is_some()
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("changed-file-299").is_none());
        assert!(window.try_find("changed-file-200").is_none());
    });
}

#[gpui_kit::test]
fn deleting_the_parent_drops_its_panel_and_subagent_views(
    cx: &mut TestAppContext,
) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    let parent = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    let child = live.backend.new_session();
    live.view.update(cx, |view, cx| view.refresh_catalog(cx));
    eventually(cx, "child listed", |cx| {
        live.view.read(cx).catalog().get(&child).is_some()
    });
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(
                PanelTabKey::Subagent {
                    session: child.to_string(),
                },
                window,
                cx,
            )
        })
    });
    live.window(cx, |_, _| ());
    eventually(cx, "subagent attached", |cx| {
        live.view.read(cx).session_handle(&child, cx).is_some()
    });

    // The parent is deleted: its tab closes and the listing no longer has it.
    live.view.update(cx, |view, cx| {
        let mut catalog = view.catalog().clone();
        catalog.remove(&parent);
        view.close_tab(0, cx);
        view.replace_catalog(catalog, cx);
    });
    cx.update(|cx| {
        let view = live.view.read(cx);
        assert!(
            !view
                .state()
                .panels
                .contains_key(&format!("session:{parent}"))
        );
    });
    eventually(cx, "subagent released", |cx| {
        live.view.read(cx).session_handle(&child, cx).is_none()
    });
}

#[gpui_kit::test]
fn a_session_in_the_panel_keeps_its_composer_controls_inside(
    cx: &mut TestAppContext,
) {
    // The narrowest panel the layout allows.
    let mut state = DesktopState::default();
    state.layout.panel_width = 320.0;
    let live = with_session(live(cx, state), cx);
    let other = live.backend.new_session();
    live.view.update(cx, |view, cx| view.refresh_catalog(cx));
    eventually(cx, "listed", |cx| {
        live.view.read(cx).catalog().get(&other).is_some()
    });
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(
                PanelTabKey::Subagent {
                    session: other.to_string(),
                },
                window,
                cx,
            )
        })
    });
    live.until(cx, "two composers", |window| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .filter(|element| {
                element
                    .path()
                    .contains(&gpui_kit::ElementId::Name("right-panel".into()))
                    && element.path().last()
                        == Some(&gpui_kit::ElementId::Name(
                            "composer-model".into(),
                        ))
            })
            .count()
            == 1
    });
    live.window(cx, |window, _| {
        let panel = window.find("right-panel").bounds();
        let model = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|element| {
                element
                    .path()
                    .contains(&gpui_kit::ElementId::Name("right-panel".into()))
                    && element.path().last()
                        == Some(&gpui_kit::ElementId::Name(
                            "composer-model".into(),
                        ))
            })
            .expect("panel composer model")
            .bounds();
        assert!(
            model.right() <= panel.right(),
            "model chip {model:?} spills out of the panel {panel:?}"
        );
    });
}

/// Opens `other` as a subagent tab in the active panel and waits until it is attached.
fn show_in_panel(
    live: &Live,
    cx: &mut TestAppContext,
    other: &desktop::backend::SessionId,
) {
    live.view.update(cx, |view, cx| view.refresh_catalog(cx));
    eventually(cx, "listed", |cx| {
        live.view.read(cx).catalog().get(other).is_some()
    });
    let tab = PanelTabKey::Subagent {
        session: other.to_string(),
    };
    live.window(cx, |window, cx| {
        live.view
            .update(cx, |view, cx| view.open_panel_tab(tab, window, cx))
    });
    live.window(cx, |_, _| ());
    eventually(cx, "attached", |cx| {
        live.view.read(cx).session_handle(other, cx).is_some()
    });
}

#[gpui_kit::test]
fn closing_a_main_tab_keeps_a_session_the_panel_shows(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    let other = live.backend.new_session();
    show_in_panel(&live, cx, &other);
    // The same session also opens as a main tab, which is then closed.
    live.view
        .update(cx, |view, cx| view.open_session(&other, cx));
    live.window(cx, |_, _| ());
    live.view.update(cx, |view, cx| {
        let index = view
            .tabs()
            .iter()
            .position(|tab| *tab == TabEntry::Session(other.clone()))
            .expect("main tab");
        view.close_tab(index, cx);
    });
    live.window(cx, |_, _| ());
    cx.update(|cx| {
        assert!(
            live.view.read(cx).session_handle(&other, cx).is_some(),
            "the panel still shows the session"
        )
    });
}

#[gpui_kit::test]
fn closing_the_parent_tab_releases_its_panel_subagents(
    cx: &mut TestAppContext,
) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    let parent = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    let other = live.backend.new_session();
    show_in_panel(&live, cx, &other);
    live.view.update(cx, |view, cx| view.close_tab(0, cx));
    eventually(cx, "released", |cx| {
        live.view.read(cx).session_handle(&other, cx).is_none()
    });
    cx.update(|cx| {
        assert!(
            live.view
                .read(cx)
                .state()
                .panels
                .contains_key(&format!("session:{parent}")),
            "the parent's panel is still remembered"
        )
    });
}

#[gpui_kit::test]
fn a_restored_panel_lists_its_own_sessions_workspace(cx: &mut TestAppContext) {
    // W1 is registered first; the session runs in the fixture workspace.
    let mut backend = support::ScriptedBackend::start();
    std::fs::write(backend.workspace.path().join("mine.rs"), "").expect("file");
    let first = tempfile::tempdir().expect("w1");
    std::fs::write(first.path().join("other-only.rs"), "").expect("file");
    let session = backend.new_session();
    let mut state = support::shell::with_workspaces(&[
        first.path().to_str().expect("utf8"),
        backend.workspace.path().to_str().expect("utf8"),
    ]);
    state.tabs.open = vec![session.to_string()];
    state.tabs.active = Some(0);
    state.panels.insert(
        format!("session:{session}"),
        desktop::state::PanelState::builder()
            .open(true)
            .tabs(vec![PanelTabKey::Files])
            .active(Some(0))
            .build(),
    );
    let (handle, view, dir) = support::shell::open(cx, state);
    gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
        window.render_frame(cx)
    })
    .expect("first frame");
    support::shell::connect(cx, &view, &mut backend);
    let live = Live::builder()
        .handle(handle)
        .view(view)
        .backend(backend)
        ._dir(dir)
        .build();

    live.until(cx, "own workspace", |window| {
        window.try_find("file-node-mine.rs").is_some()
            || window.try_find("file-node-other-only.rs").is_some()
    });
    live.window(cx, |window, _| {
        assert!(
            window.try_find("file-node-other-only.rs").is_none(),
            "listed the first workspace"
        );
        assert!(window.try_find("file-node-mine.rs").is_some());
    });
}

#[gpui_kit::test]
fn escape_closes_a_sheet_that_appeared_by_resizing(cx: &mut TestAppContext) {
    let live = with_session(live(cx, DesktopState::default()), cx);
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    // Typing in the composer, then the window narrows under the open panel.
    live.window(cx, |window, cx| window.press("ctrl-l", cx));
    cx.simulate_window_resize(live.handle, size(px(960.), px(640.)));
    live.window(cx, |window, _| {
        assert!(window.try_find("panel-sheet").is_some())
    });
    live.window(cx, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert!(!is_open(&live, cx), "Esc closes the sheet"));
}

/// A folder the user folded in the Changes tree stays folded when the tab
/// reads the workspace again.
#[gpui_kit::test]
fn a_folded_changes_folder_stays_folded_on_refresh(cx: &mut TestAppContext) {
    let live = with_session(live_in_git_repo(cx), cx);
    std::fs::create_dir_all(root(&live).join("dir")).expect("dir");
    std::fs::write(root(&live).join("dir/x.rs"), "x\n").expect("new");

    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "the file in its folder", |window| {
        window.try_find("changed-file-0").is_some()
    });
    click(&live, cx, "change-folder-dir");
    live.until(cx, "folded", |window| {
        window.try_find("changed-file-0").is_none()
    });

    click(&live, cx, "changes-refresh");
    // The read lands within these frames; the folder must not open again.
    for _ in 0..30 {
        live.window(cx, |window, _| {
            assert!(window.try_find("changed-file-0").is_none(), "unfolded")
        });
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

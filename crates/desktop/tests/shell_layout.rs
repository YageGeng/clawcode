//! The shell's layout: the session card, the main card, and the right panel, without a rail.

mod support;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, ExternalPaths, FileDropEvent, PlatformInput,
    TestAppContext,
};
use support::shell::{live_in_git_repo, open, shows, with_workspaces};

#[gpui_kit::test]
fn no_rail_and_three_cards(cx: &mut TestAppContext) {
    let (handle, _view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("rail").is_none());
        assert!(window.try_find("session-card").is_some());
        assert!(window.try_find("content-card").is_some());
    })
    .expect("window");
}

#[gpui_kit::test]
fn session_card_is_an_inset_card_aligned_with_the_content_card(
    cx: &mut TestAppContext,
) {
    let (handle, _view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let sessions = window.find("session-card").bounds();
        let card = window.find("content-card").bounds();
        let gap = gpui_kit::px(8.);
        assert_eq!(
            sessions.left(),
            gap,
            "spacing sm from the window's left edge"
        );
        assert_eq!(
            window.viewport_size().height - sessions.bottom(),
            gap,
            "spacing sm from the window's bottom edge"
        );
        assert_eq!(sessions.top(), card.top(), "tops line up");
        assert_eq!(sessions.bottom(), card.bottom(), "bottoms line up");
        assert_eq!(
            card.left() - sessions.right(),
            gap,
            "spacing sm between cards"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn ctrl_b_hides_the_session_card(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    let press = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("ctrl-b", cx);
        })
        .expect("window");
    };
    press(cx);
    cx.update(|cx| assert!(!view.read(cx).state().layout.sidebar_open));
    press(cx);
    cx.update(|cx| assert!(view.read(cx).state().layout.sidebar_open));
}

#[gpui_kit::test]
fn settings_button_sits_in_the_session_card(cx: &mut TestAppContext) {
    let (handle, _view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let card = window.find("session-card").bounds();
        let button = window.find("open-settings").bounds();
        assert!(card.contains(&button.origin));
        assert!(card.contains(&button.bottom_right()));
    })
    .expect("window");
}

#[gpui_kit::test]
fn dropping_a_folder_on_the_session_card_adds_it(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&[]));
    let folder = tempfile::tempdir().expect("folder");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let card = window.find("session-card").bounds();
        let position = card.center();
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Entered {
                position,
                paths: ExternalPaths(vec![folder.path().to_path_buf()].into()),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::FileDrop(FileDropEvent::Submit { position }),
            cx,
        );
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(view.read(cx).state().workspaces.contains(folder.path()))
    });
}

#[gpui_kit::test]
fn workspace_rows_show_the_branch(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    let root = live.backend.workspace.path().to_path_buf();
    live.add_workspace(cx, &root);

    live.until(cx, "branch", |window| shows(window, "main"));
}

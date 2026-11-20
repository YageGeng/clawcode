//! Shell lifecycle: quit confirmation, debounced state saving, and a stopped backend.

mod support;

use std::time::Duration;

use desktop::actions::Quit;
use desktop::session::SessionStatus;
use desktop::state::DesktopState;
use futures::channel::mpsc::unbounded;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::ScriptedBackend;
use support::shell::{
    click_label, eventually, id, open, session, shows, with_workspaces,
};

#[gpui_kit::test]
fn quit_with_a_running_session_asks_first(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let session_id = backend.new_session();
    cx.executor().allow_parking();
    let workspace =
        backend.workspace.path().to_str().expect("utf8").to_string();
    let (handle, view, _dir) = open(cx, with_workspaces(&[&workspace]));
    let events = std::mem::replace(&mut backend.events, unbounded().1);
    view.update(cx, |view, cx| {
        view.set_backend(Ok((backend.client.clone(), events)), cx)
    });
    eventually(cx, "session listed", |cx| {
        view.read(cx).sidebar_model(cx).groups.len() == 2
    });
    view.update(cx, |view, cx| view.open_session(&session_id, cx));
    eventually(cx, "attached", |cx| {
        view.read(cx).sidebar_model(cx).groups.len() == 2
    });
    backend
        .block_on(backend.client.prompt(&session_id, vec!["slow".into()]))
        .expect("prompt");
    eventually(cx, "running", |cx| {
        view.read(cx).session_status(&session_id, cx) == SessionStatus::Running
    });

    cx.update_window(handle, |_, window, cx| {
        window.dispatch_action(Box::new(Quit), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "Stop the running session and quit?"));
        click_label(window, "Cancel", cx);
    })
    .expect("window");
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        assert!(!window.has_active_dialog(cx))
    })
    .expect("window");
}

#[gpui_kit::test]
fn tab_changes_are_persisted_after_debounce(cx: &mut TestAppContext) {
    let (_handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    let path = dir.path().join("desktop.json");
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("s1", "/w/a", Some("one"), 1)]
                .into_iter()
                .collect(),
            cx,
        );
        view.open_session(&id("s1"), cx);
    });

    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(!path.exists(), "saved before the debounce elapsed");

    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let saved = DesktopState::load(&path);
    assert_eq!(saved.tabs.open, ["s1"]);
    assert_eq!(saved.tabs.active, Some(0));
}

#[gpui_kit::test]
fn stopped_backend_offers_a_restart(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    cx.executor().allow_parking();
    let (handle, view, _dir) = open(cx, DesktopState::default());
    let events = std::mem::replace(&mut backend.events, unbounded().1);
    view.update(cx, |view, cx| {
        view.set_backend(Ok((backend.client.clone(), events)), cx)
    });

    let ScriptedBackend {
        connection,
        runtime,
        ..
    } = backend;
    runtime.block_on(connection.close());

    eventually(cx, "stopped banner", |cx| view.read(cx).backend_stopped());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "The backend stopped."));
        assert!(window.find("restart-app").visible());
    })
    .expect("window");
}

#[gpui_kit::test]
fn discovered_workspaces_are_persisted(cx: &mut TestAppContext) {
    let (_handle, view, dir) = open(cx, DesktopState::default());
    let path = dir.path().join("desktop.json");

    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("s1", "/w/discovered", Some("one"), 1)]
                .into_iter()
                .collect(),
            cx,
        )
    });
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();

    let saved = DesktopState::load(&path);
    assert!(
        saved
            .workspaces
            .contains(std::path::Path::new("/w/discovered"))
    );
}

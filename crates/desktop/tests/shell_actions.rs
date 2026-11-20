//! Sidebar and card commands: removing workspaces, missing folders, renaming and deleting sessions.

mod support;

use std::path::Path;

use desktop::ui::SidebarRow;
use futures::channel::mpsc::unbounded;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{ClickOptions, TestWindowExt as _};
use gpui_kit::{AppContext as _, TestAppContext};
use support::ScriptedBackend;
use support::shell::{
    Live, click_label, eventually, live, model, open, session, shows, titles,
    with_workspaces,
};

#[gpui_kit::test]
fn removing_a_workspace_survives_rediscovery(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a", "/w/b"]));
    let catalog = || {
        [session("s1", "/w/a", Some("one"), 1)]
            .into_iter()
            .collect()
    };
    view.update(cx, |view, cx| view.replace_catalog(catalog(), cx));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Right-click the first workspace row.
        window.click_with_options(
            "0-0-0",
            ClickOptions::new()
                .with_offset(gpui_kit::point(
                    gpui_kit::px(16.),
                    gpui_kit::px(12.),
                ))
                .with_button(gpui_kit::MouseButton::Right),
            cx,
        );
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        click_label(window, "Remove from list", cx);
    })
    .expect("window");
    view.update(cx, |view, cx| view.replace_catalog(catalog(), cx));

    cx.update(|cx| {
        assert!(!view.read(cx).state().workspaces.contains(Path::new("/w/a")))
    });
    let model = model(cx, &view);
    assert!(model.groups[0].rows.iter().all(|row| !matches!(row, SidebarRow::Workspace { path, .. } if path == Path::new("/w/a"))));
}

#[gpui_kit::test]
fn missing_workspace_folder_is_marked(cx: &mut TestAppContext) {
    let existing = tempfile::tempdir().expect("existing workspace");
    let existing_path = existing.path().to_str().expect("utf8").to_string();
    let (_handle, view, _dir) = open(
        cx,
        with_workspaces(&["/definitely/missing", &existing_path]),
    );

    let model = model(cx, &view);

    let missing: Vec<bool> = model.groups[0]
        .rows
        .iter()
        .filter_map(|row| match row {
            SidebarRow::Workspace { missing, .. } => Some(*missing),
            _ => None,
        })
        .collect();
    assert_eq!(missing, [true, false]);
}

/// Opens the shell connected to a scripted backend whose workspace is registered.
fn connected(
    cx: &mut TestAppContext,
    backend: &mut ScriptedBackend,
) -> (
    gpui_kit::AnyWindowHandle,
    gpui_kit::Entity<desktop::ui::AppView>,
    tempfile::TempDir,
) {
    cx.executor().allow_parking();
    let workspace =
        backend.workspace.path().to_str().expect("utf8").to_string();
    let (handle, view, dir) = open(cx, with_workspaces(&[&workspace]));
    let events = std::mem::replace(&mut backend.events, unbounded().1);
    let client = backend.client.clone();
    view.update(cx, |view, cx| view.set_backend(Ok((client, events)), cx));
    (handle, view, dir)
}

#[gpui_kit::test]
fn rename_dialog_updates_the_title(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let (handle, view, _dir) = connected(cx, &mut backend);
    eventually(cx, "session listed", |cx| {
        view.read(cx).sidebar_model(cx).groups.len() == 2
    });

    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.rename_session(&id, window, cx));
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.input("Renamed", cx);
        window.press("enter", cx);
    })
    .expect("window");

    eventually(cx, "renamed row", |cx| {
        titles(&view.read(cx).sidebar_model(cx).groups[1].rows) == ["Renamed"]
    });
    let sessions = backend
        .block_on(backend.client.list_sessions())
        .expect("list");
    let listed = sessions
        .iter()
        .find(|session| session.session_id == id)
        .expect("listed");
    assert_eq!(listed.title.as_deref(), Some("Renamed"));
}

#[gpui_kit::test]
fn delete_confirmation_names_the_session_and_removes_it(
    cx: &mut TestAppContext,
) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.set_title(&id, "hello"))
        .expect("title");
    let (handle, view, _dir) = connected(cx, &mut backend);
    eventually(cx, "session listed", |cx| {
        view.read(cx).sidebar_model(cx).groups.len() == 2
    });
    view.update(cx, |view, cx| view.open_session(&id, cx));

    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.delete_session(&id, window, cx));
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "Delete “hello”?"));
        click_label(window, "Delete", cx);
    })
    .expect("window");

    eventually(cx, "row removed", |cx| {
        view.read(cx).sidebar_model(cx).groups.len() == 1
    });
    cx.update(|cx| assert!(view.read(cx).tabs().is_empty()));
    let sessions = backend
        .block_on(backend.client.list_sessions())
        .expect("list");
    assert!(sessions.iter().all(|session| session.session_id != id));
}

#[gpui_kit::test]
fn escape_dismisses_the_rename_dialog_and_restores_focus(
    cx: &mut TestAppContext,
) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("s1", "/w/a", Some("one"), 1)]
                .into_iter()
                .collect(),
            cx,
        )
    });
    let id = support::shell::id("s1");

    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.rename_session(&id, window, cx));
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        // Shell shortcuts work again, so focus went back to the workspace.
        window.press("ctrl-n", cx);
    })
    .expect("window");

    cx.update(|cx| assert_eq!(view.read(cx).tabs().len(), 1));
}

#[gpui_kit::test]
fn failed_rename_pushes_a_notification(cx: &mut TestAppContext) {
    let live = live(cx, desktop::state::DesktopState::default());
    let id = live.open_new_session(cx);
    // Notification text is not painted by the headless window, so the count is the signal;
    // exactly one proves Enter's two commit paths send a single rename.
    let Live {
        handle,
        view,
        backend,
        _dir,
    } = live;
    backend.runtime.block_on(backend.connection.close());

    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.rename_session(&id, window, cx));
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.input("Renamed", cx);
        window.press("enter", cx);
    })
    .expect("window");

    // Poll in real time: the failing request runs on the backend's own runtime.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let count = cx
            .update_window(handle, |_, window, cx| {
                window.notifications(cx).len()
            })
            .expect("window");
        if count == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no error notification"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // A second commit would fail a moment later; give it the time to show up.
    std::thread::sleep(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    let (count, dialog) = cx
        .update_window(handle, |_, window, cx| {
            (window.notifications(cx).len(), window.has_active_dialog(cx))
        })
        .expect("window");
    assert_eq!(count, 1, "Enter commits the rename exactly once");
    assert!(!dialog, "the dialog closed on Enter");
}

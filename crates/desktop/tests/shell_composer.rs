//! Composer: sending, newlines, IME, the run queue, stopping, drafts, and focus.

mod support;

use std::time::Duration;

use desktop::backend::SessionId;
use desktop::session::SessionStatus;
use desktop::state::DesktopState;
use desktop::ui::{Composer, SessionRow, SidebarRow, TabEntry};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{TestInput, TestWindowExt as _};
use gpui_kit::{
    App, AppContext as _, ElementInputHandler, Entity, Focusable as _,
    SharedString, TestAppContext, Window,
};
use support::shell::{Live, eventually, live, model, open, shows};

/// The active tab's composer.
fn composer(view: &Entity<desktop::ui::AppView>, cx: &App) -> Entity<Composer> {
    view.read(cx)
        .active_view()
        .expect("active session view")
        .read(cx)
        .composer()
        .clone()
}

/// The composer's current text.
fn value(view: &Entity<desktop::ui::AppView>, cx: &App) -> String {
    composer(view, cx)
        .read(cx)
        .input()
        .read(cx)
        .value()
        .to_string()
}

/// Whether the active composer's input owns keyboard focus.
fn composer_focused(
    view: &Entity<desktop::ui::AppView>,
    window: &Window,
    cx: &App,
) -> bool {
    composer(view, cx)
        .read(cx)
        .input()
        .read(cx)
        .focus_handle(cx)
        .is_focused(window)
}

/// Focuses the active composer.
fn focus(live: &Live, cx: &mut TestAppContext) {
    live.window(cx, |window, cx| {
        let composer = composer(&live.view, cx);
        composer.update(cx, |composer, cx| composer.focus(window, cx));
    });
}

/// Types `text` into the focused composer and presses `key`.
fn type_and_press(live: &Live, cx: &mut TestAppContext, text: &str, key: &str) {
    live.window(cx, |window, cx| {
        window.input(text, cx);
        window.press(key, cx);
    });
}

/// Number of transcript rows of a session.
fn rows(live: &Live, id: &SessionId, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        live.session(id, cx)
            .expect("attached session")
            .read(cx)
            .projection()
            .len()
    })
}

/// Lets a wrongly sent prompt reach the projection before asserting nothing arrived.
fn settle(cx: &mut TestAppContext) {
    std::thread::sleep(Duration::from_millis(200));
    cx.run_until_parked();
}

/// Waits until the session's run is active.
fn wait_running(live: &Live, id: &SessionId, cx: &mut TestAppContext) {
    eventually(cx, "run started", |cx| {
        live.view.read(cx).session_status(id, cx) == SessionStatus::Running
    });
}

/// Registers the fixture workspace through a session discovered in the catalog, without opening a tab.
fn discover_fixture_workspace(live: &Live, cx: &mut TestAppContext) {
    live.backend.new_session();
    live.view.update(cx, |view, cx| view.refresh_catalog(cx));
    let workspace =
        desktop::workspace::normalize(live.backend.workspace.path());
    eventually(cx, "fixture workspace registered", |cx| {
        live.view.read(cx).state().workspaces.contains(&workspace)
    });
}

#[gpui_kit::test]
fn enter_sends_and_clears(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "hello", "enter");
    live.until(cx, "reply", |window| shows(window, "Hi there"));
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "");
}

#[gpui_kit::test]
fn shift_enter_inserts_a_newline(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "a", "shift-enter");
    live.window(cx, |window, cx| window.input("b", cx));
    settle(cx);
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "a\nb");
    assert_eq!(rows(&live, &id, cx), 0);
}

#[gpui_kit::test]
fn blank_text_is_not_sent(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "   ", "enter");
    settle(cx);
    assert_eq!(rows(&live, &id, cx), 0);
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "   ");
}

#[gpui_kit::test]
fn enter_while_composing_only_commits(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    let input =
        cx.update(|cx| composer(&live.view, cx).read(cx).input().clone());
    let bounds = live.window(cx, |window, cx| {
        let bounds = window.find("composer-input").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .compose_text(None, "nihao", None, cx);
        window.press("enter", cx);
        bounds
    });
    // The composer reacts to Enter once the window update has flushed its events.
    let marked = live.window(cx, |window, cx| {
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .marked_text_range(cx)
    });
    assert_eq!(marked, None);
    settle(cx);
    assert_eq!(rows(&live, &id, cx), 0);
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "nihao");
}

#[gpui_kit::test]
fn enter_while_running_queues_a_follow_up(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    // `slower` keeps the run busy long enough to observe the chip reliably.
    type_and_press(&live, cx, "slower", "enter");
    wait_running(&live, &id, cx);
    type_and_press(&live, cx, "hello", "enter");
    live.until(cx, "follow-up chip", |window| shows(window, "Follow-up"));
    live.until(cx, "queue drained and reply shown", |window| {
        !shows(window, "Follow-up") && shows(window, "Hi there")
    });
}

#[gpui_kit::test]
fn chip_actions_promote_and_remove(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "slower", "enter");
    wait_running(&live, &id, cx);
    type_and_press(&live, cx, "hello", "enter");
    live.until(cx, "follow-up chip", |window| shows(window, "Follow-up"));
    let queue_id = cx.update(|cx| {
        let handle = live.session(&id, cx).expect("attached");
        handle.read(cx).projection().pending().follow_up[0]
            .queue_id
            .to_string()
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from(format!("promote:{queue_id}")), cx)
    });
    live.until(cx, "steering chip", |window| {
        shows(window, "Steering") && !shows(window, "Follow-up")
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from(format!("remove:{queue_id}")), cx)
    });
    let chip = SharedString::from(format!("chip:{queue_id}"));
    live.until(cx, "chip removed", |window| {
        window.try_find(chip.clone()).is_none()
    });
    // The removed prompt's request ends as cancelled: its text comes back
    // to the composer, and it is not a send failure.
    eventually(cx, "removed text returned", |cx| {
        value(&live.view, cx) == "hello"
    });
    assert_eq!(
        live.window(cx, |window, cx| window.notifications(cx).len()),
        0
    );
}

#[gpui_kit::test]
fn escape_with_a_queued_follow_up_is_not_a_send_failure(
    cx: &mut TestAppContext,
) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "slower", "enter");
    wait_running(&live, &id, cx);
    type_and_press(&live, cx, "hello", "enter");
    live.until(cx, "follow-up chip", |window| shows(window, "Follow-up"));

    live.window(cx, |window, cx| window.press("escape", cx));
    eventually(cx, "run stopped", |cx| {
        live.view.read(cx).session_status(&id, cx) == SessionStatus::Idle
    });
    // The kernel withdraws the queued follow-up; its text returns silently.
    eventually(cx, "withdrawn text returned", |cx| {
        value(&live.view, cx) == "hello"
    });
    settle(cx);
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "hello");
    assert_eq!(
        live.window(cx, |window, cx| window.notifications(cx).len()),
        0
    );
}

#[gpui_kit::test]
fn sent_text_keeps_its_leading_indentation(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);
    type_and_press(&live, cx, "  indented  ", "enter");
    eventually(cx, "prompt delivered as typed", |cx| {
        live.session(&id, cx).is_some_and(|handle| {
            let projection = handle.read(cx).projection();
            (0..projection.len()).any(|index| {
                matches!(
                    projection.row(index),
                    Some(desktop::projection::TranscriptRow::Message(message))
                        if message.text == "  indented  "
                )
            })
        })
    });
    assert_eq!(cx.update(|cx| value(&live.view, cx)), "");
}

#[gpui_kit::test]
fn workspace_picker_is_disabled_while_the_session_is_created(
    cx: &mut TestAppContext,
) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    live.window(cx, |window, _| {
        assert_eq!(window.find("composer-workspace").disabled(), None)
    });
    // No parking between the two updates: the creation task has not run yet.
    cx.update_window(live.handle, |_, window, cx| {
        window.input("hello", cx);
        window.press("enter", cx);
    })
    .expect("window");
    cx.update_window(live.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("composer-workspace").disabled(), Some(true));
    })
    .expect("window");
    eventually(cx, "draft replaced by a session", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });
}

#[gpui_kit::test]
fn second_enter_during_creation_creates_one_session(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    live.window(cx, |_, _| {});
    // Two window updates without parking in between: the first Enter's
    // creation task has not run yet when the second Enter arrives.
    for text in ["hello", "again"] {
        cx.update_window(live.handle, |_, window, cx| {
            window.input(text, cx);
            window.press("enter", cx);
        })
        .expect("window");
    }
    eventually(cx, "draft replaced by a session", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });
    live.until(cx, "reply", |window| shows(window, "Hi there"));
    let sessions = live
        .backend
        .block_on(live.backend.client.list_sessions())
        .expect("sessions");
    assert_eq!(sessions.len(), 2, "the discovered session plus one new one");
    cx.update(|cx| {
        assert_eq!(live.view.read(cx).tabs().len(), 1);
        assert_eq!(value(&live.view, cx), "again");
    });
}

#[gpui_kit::test]
fn stale_catalog_keeps_a_just_created_session(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    // A listing taken before the draft's session existed.
    let stale = live
        .backend
        .block_on(live.backend.client.list_sessions())
        .expect("sessions");
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    type_and_press(&live, cx, "hello", "enter");
    eventually(cx, "draft replaced by a session", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });

    live.view.update(cx, |view, cx| {
        view.replace_catalog(stale.iter().map(Into::into).collect(), cx)
    });
    cx.update(|cx| {
        assert!(matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)]))
    });
}

#[gpui_kit::test]
fn draft_picks_a_workspace_added_later(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    live.window(cx, |window, _| {
        assert_eq!(window.find("composer-send").disabled(), Some(true))
    });
    discover_fixture_workspace(&live, cx);
    live.until(cx, "workspace picker", |window| {
        window.try_find("composer-workspace").is_some()
    });

    live.window(cx, |window, cx| window.click("composer-workspace", cx));
    let name = live
        .backend
        .workspace
        .path()
        .file_name()
        .expect("folder name")
        .to_string_lossy()
        .into_owned();
    live.window(cx, |window, cx| {
        support::shell::click_label(window, &name, cx)
    });
    eventually(cx, "draft moved to the workspace", |cx| {
        matches!(
            live.view.read(cx).tabs(),
            [TabEntry::Draft { workspace: Some(_) }]
        )
    });
    live.window(cx, |window, _| {
        assert_eq!(window.find("composer-send").disabled(), None)
    });
}

#[gpui_kit::test]
fn escape_stops_only_a_running_session(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    focus(&live, cx);

    // Idle: Escape changes nothing.
    live.window(cx, |window, cx| window.press("escape", cx));
    settle(cx);
    cx.update(|cx| {
        assert_eq!(
            live.view.read(cx).session_status(&id, cx),
            SessionStatus::Idle
        )
    });
    assert_eq!(rows(&live, &id, cx), 0);

    // Running: Escape stops the run before its reply arrives.
    type_and_press(&live, cx, "slow", "enter");
    wait_running(&live, &id, cx);
    live.window(cx, |window, cx| window.press("escape", cx));
    eventually(cx, "run stopped", |cx| {
        live.view.read(cx).session_status(&id, cx) == SessionStatus::Idle
    });
    std::thread::sleep(Duration::from_millis(600));
    assert!(!live.window(cx, |window, _| shows(window, "late")));
}

#[gpui_kit::test]
fn draft_first_message_creates_the_session(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    // A new draft's composer takes focus on first show.
    live.window(cx, |window, cx| {
        assert!(composer_focused(&live.view, window, cx))
    });
    type_and_press(&live, cx, "hello", "enter");

    eventually(cx, "draft replaced by a session", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });
    let id = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("unexpected tabs {other:?}"),
    });
    let listed = model(cx, &live.view).groups.iter().any(|group| {
        group.rows.iter().any(|row| {
            matches!(row, SidebarRow::Session(SessionRow { id: listed, .. }) if listed == &id)
        })
    });
    assert!(listed, "the new session is in the catalog");
    live.until(cx, "reply", |window| shows(window, "Hi there"));
}

#[gpui_kit::test]
fn draft_send_failure_keeps_the_text(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    live.window(cx, |window, cx| {
        window.press("ctrl-n", cx);
        window.input("hello", cx);
    });
    let Live {
        handle,
        view,
        backend,
        _dir,
    } = live;
    backend.runtime.block_on(backend.connection.close());

    cx.update_window(handle, |_, window, cx| window.press("enter", cx))
        .expect("window");
    eventually(cx, "creation failed and the text came back", |cx| {
        value(&view, cx) == "hello"
    });
    cx.update(|cx| {
        assert!(matches!(view.read(cx).tabs(), [TabEntry::Draft { .. }]))
    });
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.notifications(cx).len(), 1, "one error notification")
    })
    .expect("window");
}

#[gpui_kit::test]
fn draft_without_workspace_disables_send(cx: &mut TestAppContext) {
    let (handle, _view, _dir) = open(cx, DesktopState::default());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-n", cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("composer-send").disabled(), Some(true));
        assert!(shows(window, "Add a workspace to start"));
    })
    .expect("window");
}

#[gpui_kit::test]
fn ctrl_l_focuses_the_composer(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    live.window(cx, |window, cx| window.press("ctrl-k", cx));
    live.window(cx, |window, cx| {
        assert!(!composer_focused(&live.view, window, cx))
    });
    live.window(cx, |window, cx| window.press("ctrl-l", cx));
    live.window(cx, |window, cx| {
        assert!(composer_focused(&live.view, window, cx))
    });
}

#[gpui_kit::test]
fn draft_first_run_folds_once_it_settles(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    discover_fixture_workspace(&live, cx);
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    type_and_press(&live, cx, "read", "enter");
    eventually(cx, "draft replaced by a session", |cx| {
        matches!(live.view.read(cx).tabs(), [TabEntry::Session(_)])
    });
    let id = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("unexpected tabs {other:?}"),
    });
    eventually(cx, "run settled", |cx| {
        live.view.read(cx).session_status(&id, cx) == SessionStatus::Idle
            && live
                .session(&id, cx)
                .is_some_and(|handle| handle.read(cx).projection().len() > 1)
    });
    // The first run of a draft folds like any other finished run.
    let prompt = live.user_message(cx, &id, "read");
    let run = SharedString::from(format!("run:message:{prompt}"));
    live.until(cx, "folded first run", |window| {
        window.try_find(run.clone()).is_some()
    });
}

/// A 1×1 transparent PNG's bytes.
fn pixel_png() -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("png")
}

/// Attaches bytes to the active composer as if a file were picked.
fn attach(live: &Live, cx: &mut TestAppContext, name: &str, bytes: Vec<u8>) {
    live.window(cx, |window, cx| {
        let composer = composer(&live.view, cx);
        composer.update(cx, |composer, cx| {
            composer.add_attachment(name.to_string(), bytes, window, cx)
        });
    });
}

#[gpui_kit::test]
fn attached_image_is_sent_with_the_text(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    attach(&live, cx, "pixel.png", pixel_png());
    focus(&live, cx);

    live.window(cx, |window, cx| {
        window.input("look", cx);
        window.press("enter", cx);
    });

    eventually(cx, "image sent", |cx| {
        live.session(&id, cx).is_some_and(|handle| {
            let projection = handle.read(cx).projection();
            (0..projection.len()).any(|index| {
                matches!(projection.row(index),
                    Some(desktop::projection::TranscriptRow::Message(message))
                        if message.images.len() == 1)
            })
        })
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("attachment-0").is_none())
    });
}

#[gpui_kit::test]
fn pasted_image_becomes_an_attachment(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    focus(&live, cx);
    cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_image(
            &gpui_kit::Image::from_bytes(
                gpui_kit::ImageFormat::Png,
                pixel_png(),
            ),
        ))
    });

    live.window(cx, |window, cx| window.press("ctrl-v", cx));

    live.until(cx, "attachment chip", |window| {
        window.try_find("attachment-0").is_some()
    });
}

#[gpui_kit::test]
fn removing_an_attachment(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    attach(&live, cx, "pixel.png", pixel_png());
    live.until(cx, "attachment chip", |window| {
        window.try_find("attachment-0").is_some()
    });

    live.window(cx, |window, cx| {
        // The remove control shows on hover, under the attachment's own id.
        use gpui_kit::test::TestEventExt as _;
        let center = window.find("attachment-0").bounds().center();
        window.pointer_move(center, None, cx);
        window.render_frame(cx);
        window.click(
            gpui_kit::ElementId::NamedChild(
                std::sync::Arc::new(
                    SharedString::from("attachment-remove-0").into(),
                ),
                "remove".into(),
            ),
            cx,
        )
    });

    live.until(cx, "no chip", |window| {
        window.try_find("attachment-0").is_none()
    });
}

#[gpui_kit::test]
fn rejected_file_notifies_and_sends_nothing(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);

    attach(&live, cx, "pixel.bmp", b"BM\0\0\0\0\0\0\0\0".to_vec());

    live.window(cx, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find("attachment-0").is_none());
    });
}

#[gpui_kit::test]
fn restore_reports_images_that_no_longer_fit(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    for n in 0..3 {
        attach(&live, cx, &format!("new-{n}.png"), pixel_png());
    }
    let mut unsent = desktop::ui::AttachmentSet::default();
    for n in 0..3 {
        unsent
            .add(format!("old-{n}.png"), pixel_png())
            .expect("png");
    }

    live.window(cx, |window, cx| {
        let composer = composer(&live.view, cx);
        composer.update(cx, |composer, cx| {
            composer.restore(
                desktop::ui::Outgoing {
                    text: String::new(),
                    images: unsent,
                },
                window,
                cx,
            )
        });
    });

    live.window(cx, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find("attachment-4").is_some());
        assert!(window.try_find("attachment-5").is_none());
    });
}

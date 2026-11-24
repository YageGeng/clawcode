//! Signing in from the shell: the device code dialog, switching to a model
//! that needs it, sending without it, and drafts that chose one.

mod support;

use std::sync::Arc;

use desktop::backend::SessionId;
use desktop::state::DesktopState;
use desktop::ui::{Outgoing, SessionView, TabEntry};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, Entity, SharedString, TestAppContext, VisualTestContext};
use support::login::{LOGIN_MODEL, LoginModel};
use support::shell::{Live, eventually, live_with_login, shows};
use support::trajectory::session_view;

/// A shell whose catalog holds a model that needs a sign-in, with the
/// fixture workspace registered.
fn start(cx: &mut TestAppContext) -> (Live, Arc<LoginModel>) {
    let model = LoginModel::new();
    let live = live_with_login(cx, DesktopState::default(), &model);
    let root = live.backend.workspace.path().to_path_buf();
    live.add_workspace(cx, &root);
    (live, model)
}

/// Chooses the model that needs a sign-in in the active view.
fn choose_locked(live: &Live, cx: &mut TestAppContext) {
    let view = session_view(live, cx);
    live.window(cx, |window, cx| {
        view.update(cx, |view, cx| {
            view.set_config("model", LOGIN_MODEL.to_owned(), window, cx)
        })
    });
}

/// The model a session uses now.
fn model_of(
    live: &Live,
    cx: &mut TestAppContext,
    id: &SessionId,
) -> Option<String> {
    cx.update(|cx| {
        let handle = live.session(id, cx)?;
        let controls = handle.read(cx).projection().controls();
        Some(controls.config("model")?.current.clone())
    })
}

/// Waits for the dialog showing this code.
fn until_code(live: &Live, cx: &mut TestAppContext, code: &str) {
    live.until(cx, "device code", |window| {
        window.try_find("sign-in-cancel").is_some() && shows(window, code)
    });
}

/// Waits until no dialog is open.
fn until_closed(live: &Live, cx: &mut TestAppContext) {
    let deadline = std::time::Instant::now() + support::WAIT;
    while live.window(cx, |window, cx| window.has_active_dialog(cx)) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the dialog to close"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Clicks an element by id in a fresh frame.
fn click(live: &Live, cx: &mut TestAppContext, id: &'static str) {
    live.window(cx, |window, cx| window.click(SharedString::from(id), cx));
}

/// Puts `text` into a view's composer.
fn fill(
    live: &Live,
    cx: &mut TestAppContext,
    view: &Entity<SessionView>,
    text: &str,
) {
    let composer = cx.update(|cx| view.read(cx).composer().clone());
    live.window(cx, |window, cx| {
        composer.update(cx, |composer, cx| {
            composer.restore(
                Outgoing {
                    text: text.to_owned(),
                    ..Outgoing::default()
                },
                window,
                cx,
            )
        })
    });
}

/// The text of a view's composer.
fn text_of(view: &Entity<SessionView>, cx: &App) -> String {
    let composer = view.read(cx).composer().read(cx);
    composer.input().read(cx).value().to_string()
}

/// The text of a view's composer, from a test context.
fn composed(cx: &mut TestAppContext, view: &Entity<SessionView>) -> String {
    cx.update(|cx| text_of(view, cx))
}

/// Presses Enter in the composer.
fn send(live: &Live, cx: &mut TestAppContext) {
    live.window(cx, |window, cx| window.press("ctrl-l", cx));
    live.window(cx, |window, cx| window.press("enter", cx));
}

/// How many notifications are up; their text is not in the snapshots.
fn notices(live: &Live, cx: &mut TestAppContext) -> usize {
    live.window(cx, |window, cx| window.notifications(cx).len())
}

/// Every session the backend lists.
fn sessions(live: &Live) -> Vec<SessionId> {
    live.backend
        .block_on(live.backend.client.list_sessions())
        .expect("list")
        .into_iter()
        .map(|session| session.session_id)
        .collect()
}

/// Sends `hello` from a draft that chose the model needing a sign-in.
fn send_locked_draft(
    live: &Live,
    cx: &mut TestAppContext,
) -> Entity<SessionView> {
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    live.window(cx, |_, _| {});
    choose_locked(live, cx);
    let view = session_view(live, cx);
    fill(live, cx, &view, "hello");
    send(live, cx);
    view
}

/// Once the user authorized the code the dialog closes by itself and the
/// switch goes through.
#[gpui_kit::test]
fn a_switch_completes_after_the_sign_in(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    let id = live.open_new_session(cx);
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-1");
    model.complete();
    until_closed(&live, cx);
    eventually(cx, "switched", |cx| {
        live.session(&id, cx).is_some_and(|handle| {
            handle
                .read(cx)
                .projection()
                .controls()
                .config("model")
                .is_some_and(|option| option.current == LOGIN_MODEL)
        })
    });
}

/// Cancelling leaves the model alone and says nothing.
#[gpui_kit::test]
fn a_cancelled_switch_keeps_the_model(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    let id = live.open_new_session(cx);
    let before = model_of(&live, cx, &id);
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-1");
    click(&live, cx, "sign-in-cancel");
    until_closed(&live, cx);

    assert_eq!(model_of(&live, cx, &id), before);
    assert_eq!(notices(&live, cx), 0);
    // The model can be chosen again, which asks for a new code.
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-2");
    assert_eq!(model.codes(), 2);
}

/// Escape gives up like the button does.
#[gpui_kit::test]
fn escape_cancels_the_sign_in(cx: &mut TestAppContext) {
    let (live, _model) = start(cx);
    live.open_new_session(cx);
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-1");
    live.window(cx, |window, cx| window.press("escape", cx));
    until_closed(&live, cx);
    // Nothing waits any more: the next choice starts a sign-in of its own.
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-2");
}

/// A prompt that needs a sign-in comes back to the composer, and sending it
/// again is left to the user.
#[gpui_kit::test]
fn a_refused_prompt_returns_and_asks_for_the_sign_in(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    live.open_new_session(cx);
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-1");
    model.complete();
    until_closed(&live, cx);
    // The credential goes away while the session is on that model.
    model.sign_out();
    let view = session_view(&live, cx);
    fill(&live, cx, &view, "hello again");
    send(&live, cx);

    until_code(&live, cx, "CODE-2");
    assert_eq!(composed(cx, &view), "hello again");
    model.complete();
    until_closed(&live, cx);
    assert_eq!(composed(cx, &view), "hello again");
    assert_eq!(notices(&live, cx), 0);
}

/// A draft that chose a model needing a sign-in sends nothing before it.
#[gpui_kit::test]
fn a_draft_signs_in_before_it_sends(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    send_locked_draft(&live, cx);
    until_code(&live, cx, "CODE-1");
    model.complete();

    // The locked model answered, so the choice was applied first.
    live.until(cx, "the locked model's reply", |window| {
        shows(window, "unlocked")
    });
}

/// While a draft waits for its sign-in there is no session yet, so leaving
/// now leaves nothing behind, and the send button shows it is busy.
#[gpui_kit::test]
fn a_waiting_draft_has_no_session(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    send_locked_draft(&live, cx);
    until_code(&live, cx, "CODE-1");

    assert!(sessions(&live).is_empty());
    live.window(cx, |window, _| {
        assert_eq!(window.find("composer-send").disabled(), Some(true));
    });
    model.complete();
    live.until(cx, "the locked model's reply", |window| {
        shows(window, "unlocked")
    });
    assert_eq!(sessions(&live).len(), 1);
}

/// A draft whose choice cannot be applied is not sent on other settings.
#[gpui_kit::test]
fn a_draft_with_an_unusable_choice_is_not_sent(cx: &mut TestAppContext) {
    let (live, _model) = start(cx);
    live.window(cx, |window, cx| window.press("ctrl-n", cx));
    live.window(cx, |_, _| {});
    let view = session_view(&live, cx);
    live.window(cx, |window, cx| {
        view.update(cx, |view, cx| {
            view.set_config("model", "nope/none".to_owned(), window, cx)
        })
    });
    fill(&live, cx, &view, "hello");
    send(&live, cx);

    let deadline = std::time::Instant::now() + support::WAIT;
    while notices(&live, cx) == 0 {
        assert!(std::time::Instant::now() < deadline, "no failure notice");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(composed(cx, &view), "hello");
    assert!(sessions(&live).is_empty());
}

/// A sign-in that ends under the quit confirmation leaves the confirmation
/// alone and closes its own dialog after it.
#[gpui_kit::test]
fn a_finished_sign_in_leaves_the_quit_confirmation(cx: &mut TestAppContext) {
    let (live, model) = start(cx);
    let busy = live.open_new_session(cx);
    live.backend
        .block_on(live.backend.client.prompt(&busy, vec!["working".into()]))
        .expect("prompt");
    live.open_new_session(cx);
    choose_locked(&live, cx);
    until_code(&live, cx, "CODE-1");
    assert!(
        !VisualTestContext::from_window(live.handle, cx).simulate_close(),
        "a running session asks first"
    );
    live.until(cx, "confirmation", |window| {
        shows(window, "Stop the running session and quit?")
    });

    model.complete();
    live.until(cx, "the code gone", |window| !shows(window, "CODE-1"));
    // Frames past the moment the sign-in's dialog would close.
    for _ in 0..20 {
        live.window(cx, |_, _| {});
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    live.window(cx, |window, _| {
        assert!(shows(window, "Stop the running session and quit?"))
    });
    live.window(cx, |window, cx| window.press("escape", cx));
    until_closed(&live, cx);
}

/// Cancelling a draft's sign-in gives the text back and leaves no session.
#[gpui_kit::test]
fn a_cancelled_draft_keeps_its_text_and_no_session(cx: &mut TestAppContext) {
    let (live, _model) = start(cx);
    let view = send_locked_draft(&live, cx);
    until_code(&live, cx, "CODE-1");
    click(&live, cx, "sign-in-cancel");
    until_closed(&live, cx);

    eventually(cx, "text returned", |cx| text_of(&view, cx) == "hello");
    cx.update(|cx| {
        assert!(matches!(
            live.view.read(cx).tabs(),
            [TabEntry::Draft { .. }]
        ))
    });
    assert!(sessions(&live).is_empty());
    assert_eq!(notices(&live, cx), 0);
}

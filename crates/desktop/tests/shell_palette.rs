//! The slash command list in the composer.

mod support;

use desktop::backend::SessionId;
use desktop::projection::SlashCommand;
use desktop::session::SessionStatus;
use desktop::state::DesktopState;
use desktop::ui::filter_commands;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, TestAppContext};
use support::shell::{Live, eventually, live, shows};

/// A command with this name.
fn command(name: &str) -> SlashCommand {
    SlashCommand::builder()
        .name(name.to_string())
        .description(format!("{name} command"))
        .build()
}

/// The active composer's text.
fn value(live: &Live, cx: &App) -> String {
    live.view
        .read(cx)
        .active_view()
        .expect("active view")
        .read(cx)
        .composer()
        .read(cx)
        .input()
        .read(cx)
        .value()
        .to_string()
}

/// Opens a session whose commands have arrived, with its composer focused.
fn ready(cx: &mut TestAppContext) -> (Live, SessionId) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    eventually(cx, "commands", |cx| {
        live.session(&id, cx).is_some_and(|handle| {
            !handle
                .read(cx)
                .projection()
                .controls()
                .commands()
                .is_empty()
        })
    });
    live.window(cx, |window, cx| {
        let composer = live
            .view
            .read(cx)
            .active_view()
            .expect("active view")
            .read(cx)
            .composer()
            .clone();
        composer.update(cx, |composer, cx| composer.focus(window, cx));
    });
    (live, id)
}

#[test]
fn filter_is_a_case_insensitive_prefix_match() {
    let commands = [command("compact"), command("name"), command("session")];

    let found: Vec<&str> = filter_commands(&commands, "SE")
        .into_iter()
        .map(|command| command.name.as_str())
        .collect();

    assert_eq!(found, ["session"]);
    assert_eq!(filter_commands(&commands, "").len(), 3);
}

#[gpui_kit::test]
fn slash_opens_the_palette_and_tab_completes(cx: &mut TestAppContext) {
    let (live, _id) = ready(cx);

    live.window(cx, |window, cx| window.input("/co", cx));
    live.until(cx, "palette", |window| {
        window.find("command-palette").visible() && shows(window, "/compact")
    });
    live.window(cx, |window, cx| window.press("tab", cx));

    // `/compact` takes an argument, so completion leaves a space after it.
    cx.update(|cx| assert_eq!(value(&live, cx), "/compact "));
    live.window(cx, |window, _| {
        assert!(
            !window
                .try_find("command-palette")
                .is_some_and(|palette| palette.visible())
        )
    });
}

#[gpui_kit::test]
fn arrows_cycle_and_enter_picks(cx: &mut TestAppContext) {
    let (live, id) = ready(cx);
    let third = cx.update(|cx| {
        live.session(&id, cx)
            .and_then(|handle| {
                handle
                    .read(cx)
                    .projection()
                    .controls()
                    .commands()
                    .get(2)
                    .map(|command| command.name.clone())
            })
            .expect("three commands")
    });

    live.window(cx, |window, cx| window.input("/", cx));
    live.window(cx, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });

    cx.update(|cx| assert!(value(&live, cx).starts_with(&format!("/{third}"))));
}

#[gpui_kit::test]
fn escape_closes_the_palette_only(cx: &mut TestAppContext) {
    let (live, id) = ready(cx);
    live.backend
        .block_on(live.backend.client.prompt(&id, vec!["slower".into()]))
        .expect("prompt");
    eventually(cx, "running", |cx| {
        live.view.read(cx).session_status(&id, cx) == SessionStatus::Running
    });

    live.window(cx, |window, cx| window.input("/", cx));
    live.until(cx, "palette", |window| {
        window.find("command-palette").visible()
    });
    live.window(cx, |window, cx| window.press("escape", cx));

    live.window(cx, |window, _| {
        assert!(
            !window
                .try_find("command-palette")
                .is_some_and(|palette| palette.visible())
        )
    });
    cx.update(|cx| {
        assert_eq!(
            live.view.read(cx).session_status(&id, cx),
            SessionStatus::Running
        )
    });
}

#[gpui_kit::test]
fn whitespace_closes_the_palette(cx: &mut TestAppContext) {
    let (live, _id) = ready(cx);

    live.window(cx, |window, cx| window.input("/compact ", cx));

    live.window(cx, |window, _| {
        assert!(
            !window
                .try_find("command-palette")
                .is_some_and(|palette| palette.visible())
        )
    });
}

#[gpui_kit::test]
fn no_matches_message(cx: &mut TestAppContext) {
    let (live, _id) = ready(cx);

    live.window(cx, |window, cx| window.input("/zzz", cx));

    live.until(cx, "empty palette", |window| {
        shows(window, "No matching commands")
    });
}

#[gpui_kit::test]
fn enter_sends_slash_text_without_matching_commands(cx: &mut TestAppContext) {
    let (live, _id) = ready(cx);

    // A path is not a command: the empty list must not swallow Enter.
    live.window(cx, |window, cx| window.input("/home/me/notes", cx));
    live.until(cx, "empty palette", |window| {
        shows(window, "No matching commands")
    });
    live.window(cx, |window, cx| window.press("enter", cx));

    live.until(cx, "sent message", |window| {
        shows(window, "/home/me/notes")
            && !shows(window, "No matching commands")
    });
    cx.update(|cx| assert_eq!(value(&live, cx), ""));
}

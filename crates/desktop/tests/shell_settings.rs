//! The settings tab: opening, applying and saving preferences, and re-measuring transcripts.

mod support;

use std::time::Duration;

use desktop::projection::TranscriptRow;
use desktop::state::{AppearancePreference, DesktopState, ThemeModePreference};
use desktop::ui::TabEntry;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, SharedString, TestAppContext};
use support::shell::{live, open, shows, with_workspaces};

/// Opens the settings tab.
fn open_settings(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<desktop::ui::AppView>,
) {
    view.update(cx, |view, cx| view.open_settings(cx));
}

#[gpui_kit::test]
fn ctrl_comma_opens_one_settings_tab(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.press("ctrl-,", cx);
        window.press("ctrl-,", cx);
    })
    .expect("window");
    cx.run_until_parked();
    let count = cx.update(|cx| {
        view.read(cx)
            .tabs()
            .iter()
            .filter(|tab| **tab == TabEntry::Settings)
            .count()
    });
    assert_eq!(count, 1);
    cx.update_window(handle, |_, window, cx| {
        // The settings panels size themselves on the second frame.
        window.render_frame(cx);
        window.render_frame(cx);
        // Page navigation text is not exposed to the text snapshot, so the General
        // page content stands in for "the settings page is on screen".
        assert!(shows(window, "Language"));
    })
    .expect("window");
}

#[gpui_kit::test]
fn appearance_changes_apply_and_persist(cx: &mut TestAppContext) {
    let (_handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.set_appearance(
            AppearancePreference::builder()
                .mode(ThemeModePreference::Dark)
                .dark_theme("Catppuccin Mocha".to_string())
                .build(),
            cx,
        )
    });
    cx.update(|cx| {
        assert_eq!(cx.theme().theme_name().as_ref(), "Catppuccin Mocha")
    });
    cx.executor().advance_clock(Duration::from_millis(700));
    cx.run_until_parked();
    let saved = DesktopState::load(&dir.path().join("desktop.json"));
    assert_eq!(saved.appearance.dark_theme, "Catppuccin Mocha");
    assert_eq!(saved.appearance.mode, ThemeModePreference::Dark);
}

#[gpui_kit::test]
fn settings_tab_is_not_persisted(cx: &mut TestAppContext) {
    let (_handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    open_settings(cx, &view);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs(), [TabEntry::Settings]);
        assert!(view.state().tabs.open.is_empty());
        assert_eq!(view.state().tabs.active, None);
    });
}

#[gpui_kit::test]
fn font_size_change_remeasures_rows(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["hello".into()]);
    live.until(cx, "reply", |window| shows(window, "Hi there"));
    let (first, second) = cx.update(|cx| {
        let handle = live.session(&id, cx).expect("attached");
        let projection = handle.read(cx).projection();
        let key = |index: usize| match projection.row(index) {
            Some(TranscriptRow::Message(message)) => {
                SharedString::from(format!("message:{}", message.message_id))
            }
            other => panic!("unexpected row {other:?}"),
        };
        (key(0), key(1))
    });
    let bounds = |cx: &mut TestAppContext, key: &SharedString| {
        live.window(cx, |window, _| window.find(key.clone()).bounds())
    };
    let before = bounds(cx, &first);

    live.view.update(cx, |view, cx| {
        let mut appearance = view.state().appearance.clone();
        appearance.font_size = 20.0;
        view.set_appearance(appearance, cx);
    });
    let after = bounds(cx, &first);
    let next = bounds(cx, &second);
    assert!(
        after.size.height > before.size.height,
        "row did not grow: {before:?} -> {after:?}"
    );
    assert!(
        next.top() >= after.bottom(),
        "rows overlap: {after:?} / {next:?}"
    );
}

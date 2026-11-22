//! The settings dialog: opening it, applying and saving preferences, fonts,
//! and re-measuring transcripts.

mod support;

use std::time::Duration;

use desktop::projection::TranscriptRow;
use desktop::state::{AppearancePreference, DesktopState, ThemeModePreference};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, SharedString, TestAppContext};
use support::shell::{live, open, shows, with_workspaces};

#[gpui_kit::test]
fn ctrl_comma_opens_one_dialog_and_no_tab(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.press("ctrl-,", cx);
        window.render_frame(cx);
        window.press("ctrl-,", cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.settings_open());
        assert!(view.tabs().is_empty(), "settings open no tab");
    });
    cx.update_window(handle, |_, window, cx| {
        // The settings panels size themselves on the second frame.
        window.render_frame(cx);
        window.render_frame(cx);
        // The General page's content stands in for "the dialog is on screen".
        assert!(shows(window, "Language"));
        let languages = support::shell::painted(window)
            .iter()
            .filter(|text| text.text() == "Language")
            .count();
        assert_eq!(languages, 1, "one settings dialog");
        // A dropdown names its setting for screen readers, then its value.
        let settings = gpui_kit::ElementId::Name("settings".into());
        let labels: Vec<String> =
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|element| element.path().contains(&settings))
                .filter_map(|element| element.label().map(|l| l.to_string()))
                .collect();
        assert!(
            labels
                .iter()
                .any(|label| label == "Language: Follow system"),
            "{labels:?}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn the_settings_button_opens_the_dialog(cx: &mut TestAppContext) {
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(SharedString::from("open-settings"), cx);
    })
    .expect("window");
    cx.update(|cx| assert!(view.read(cx).settings_open()));
}

/// Clicks the topmost dropdown button inside the settings dialog.
///
/// Settings dropdowns all use the button id `btn`, so the one under the
/// dialog's `settings` element is found through the snapshots.
fn click_settings_dropdown(
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        ElementId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, PlatformInput,
    };

    let settings = ElementId::Name("settings".into());
    let button = ElementId::Name("btn".into());
    // The topmost one on the General page is Language.
    let target = gpui_kit::base::test_support::snapshots(window)
        .into_iter()
        .filter(|element| {
            element.path().contains(&settings)
                && element.path().last() == Some(&button)
        })
        .min_by(|a, b| {
            f32::from(a.bounds().origin.y)
                .total_cmp(&f32::from(b.bounds().origin.y))
        })
        .expect("a settings dropdown");
    let position = target.bounds().center();
    // Like gpui-kit's own click: move, then down and up, a frame after each step.
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
        cx,
    );
}

/// Whether a dropdown's popup menu is on screen; its items sit under a `popup-menu` element.
fn popup_open(window: &gpui_kit::Window) -> bool {
    let menu = gpui_kit::ElementId::Name("popup-menu".into());
    gpui_kit::base::test_support::snapshots(window)
        .iter()
        .any(|element| element.path().contains(&menu))
}

#[gpui_kit::test]
fn escape_closes_one_layer_at_a_time(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    // The composer has focus; the dialog opened from the keyboard gives it back.
    live.window(cx, |window, cx| window.press("ctrl-l", cx));
    live.window(cx, |window, cx| window.press("ctrl-,", cx));
    live.window(cx, |_, _| ());
    // The General page's one dropdown (Language) opens a popup menu.
    live.window(cx, click_settings_dropdown);
    live.window(cx, |window, _| {
        assert!(popup_open(window), "language menu open")
    });
    live.window(cx, |window, cx| window.press("escape", cx));
    live.window(cx, |window, _| {
        assert!(!popup_open(window), "language menu closed")
    });
    cx.update(|cx| {
        assert!(live.view.read(cx).settings_open(), "dialog still open")
    });
    live.window(cx, |window, cx| window.press("escape", cx));
    cx.update(|cx| {
        assert!(!live.view.read(cx).settings_open(), "dialog closed")
    });
    live.window(cx, |window, cx| {
        let composer = live
            .view
            .read(cx)
            .active_view()
            .expect("session view")
            .read(cx)
            .composer()
            .clone();
        use gpui_kit::Focusable as _;
        let focused = composer
            .read(cx)
            .input()
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        assert!(focused, "focus back in the composer");
    });
}

#[gpui_kit::test]
fn font_fields_write_and_clamp(cx: &mut TestAppContext) {
    let (_handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        let appearance = AppearancePreference {
            code_font_weight: 975,
            code_font_size: 18.0,
            ..view.state().appearance.clone()
        };
        view.set_appearance(appearance, cx);
    });
    cx.update(|cx| {
        assert_eq!(view.read(cx).state().appearance.code_font_weight, 900);
        assert_eq!(cx.theme().mono_font_size, gpui_kit::px(18.));
    });
    cx.executor().advance_clock(Duration::from_millis(700));
    cx.run_until_parked();
    let saved = DesktopState::load(&dir.path().join("desktop.json"));
    assert_eq!(saved.appearance.code_font_size, 18.0);
    assert_eq!(saved.appearance.code_font_weight, 900);
}

#[gpui_kit::test]
fn subagent_target_setting_persists(cx: &mut TestAppContext) {
    use desktop::state::SubagentOpenTarget;

    let (_handle, view, dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.set_subagent_open_target(SubagentOpenTarget::Main, cx)
    });
    cx.executor().advance_clock(Duration::from_millis(700));
    cx.run_until_parked();
    let saved = DesktopState::load(&dir.path().join("desktop.json"));
    assert_eq!(saved.subagent_open_target, SubagentOpenTarget::Main);
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

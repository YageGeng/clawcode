//! Language switching in the shell; a binary of its own because the locale is process-wide.

mod support;

use desktop::state::LanguagePreference;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::shell::{open, shows, with_workspaces};

/// Serializes the tests here: the locale is process-wide and test threads run in parallel.
static LOCALE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[gpui_kit::test]
fn language_switch_relabels_the_shell(cx: &mut TestAppContext) {
    let _locale = LOCALE.lock().unwrap_or_else(|poison| poison.into_inner());
    let (handle, _view, _dir) = open(cx, with_workspaces(&["/w/a"]));

    cx.update(|cx| LanguagePreference::ZhCn.apply(cx));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "会话"));
    })
    .expect("window");
    cx.update(|cx| LanguagePreference::En.apply(cx));
}

#[gpui_kit::test]
fn language_setting_switches_the_ui(cx: &mut TestAppContext) {
    let _locale = LOCALE.lock().unwrap_or_else(|poison| poison.into_inner());
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.open_settings(window, cx))
    })
    .expect("window");

    view.update(cx, |view, cx| {
        view.set_language(LanguagePreference::ZhCn, cx)
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "语言"));
    })
    .expect("window");

    view.update(cx, |view, cx| view.set_language(LanguagePreference::En, cx));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(shows(window, "Language"));
    })
    .expect("window");
}

#[gpui_kit::test]
fn composer_placeholder_follows_the_language(cx: &mut TestAppContext) {
    let _locale = LOCALE.lock().unwrap_or_else(|poison| poison.into_inner());
    let (handle, view, _dir) = open(cx, with_workspaces(&["/w/a"]));
    view.update(cx, |view, cx| {
        view.open_draft(Some(std::path::PathBuf::from("/w/a")), cx)
    });
    // The draft's view is created by the first frame that shows it.
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    // The input paints its placeholder outside the text snapshots, so read it from the state.
    let placeholder = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let session = view.read(cx).active_view().expect("draft view");
            let composer = session.read(cx).composer().read(cx);
            composer
                .input()
                .read(cx)
                .presentation()
                .placeholder()
                .to_string()
        })
    };
    assert_eq!(placeholder(cx), "Describe the task");

    // The composer follows the switch through its subscription; no frame is drawn.
    cx.update(|cx| LanguagePreference::ZhCn.apply(cx));
    cx.run_until_parked();
    assert_eq!(placeholder(cx), "描述任务");
    cx.update(|cx| LanguagePreference::En.apply(cx));
    cx.run_until_parked();
    assert_eq!(placeholder(cx), "Describe the task");
}

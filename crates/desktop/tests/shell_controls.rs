//! Composer bar controls: plan mode, model and thinking level, and the context ring.

mod support;

use desktop::backend::SessionId;
use desktop::state::DesktopState;
use desktop::ui::SessionView;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, AppContext as _, Entity, SharedString, TestAppContext};
use support::shell::{
    Live, click_label, eventually, fixture_state, live, painted, shows,
};

/// The active tab's session view.
fn active(live: &Live, cx: &App) -> Entity<SessionView> {
    live.view
        .read(cx)
        .active_view()
        .expect("active session view")
        .clone()
}

/// Current value of one configuration option of the session.
fn current(live: &Live, cx: &App, id: &SessionId, option: &str) -> String {
    live.session(id, cx)
        .and_then(|handle| {
            handle
                .read(cx)
                .projection()
                .controls()
                .config(option)
                .map(|option| option.current.clone())
        })
        .unwrap_or_default()
}

/// Waits until the session's configuration options have arrived.
fn wait_for_options(live: &Live, cx: &mut TestAppContext, id: &SessionId) {
    eventually(cx, "configuration options", |cx| {
        !current(live, cx, id, "model").is_empty()
    });
}

#[gpui_kit::test]
fn plan_mode_chip_toggles_the_mode(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    wait_for_options(&live, cx, &id);

    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-plan-mode"), cx)
    });
    eventually(cx, "plan mode", |cx| {
        current(&live, cx, &id, "mode") == "plan"
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-plan-mode"), cx)
    });
    eventually(cx, "default mode", |cx| {
        current(&live, cx, &id, "mode") == "default"
    });
}

#[gpui_kit::test]
fn selected_plan_chip_uses_the_sidebar_accent_fill(cx: &mut TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;

    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    wait_for_options(&live, cx, &id);
    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-plan-mode"), cx)
    });
    eventually(cx, "plan mode", |cx| {
        current(&live, cx, &id, "mode") == "plan"
    });

    // Spec §6.2: a selected control is a `sidebar_accent` fill, not the ghost
    // button's `secondary_active`.
    let (accent, ghost) = live.window(cx, |window, cx| {
        let chip = window.find("composer-plan-mode").bounds();
        let scale = window.scale_factor();
        let fills: Vec<gpui_kit::Background> = window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let left = quad.bounds.origin.x.0 / scale;
                let top = quad.bounds.origin.y.0 / scale;
                (left - f32::from(chip.origin.x)).abs() < 1.
                    && (top - f32::from(chip.origin.y)).abs() < 1.
            })
            .map(|quad| quad.background)
            .collect();
        let theme = cx.theme();
        (
            fills.contains(&theme.sidebar_accent.into()),
            fills.contains(&theme.secondary_active.into()),
        )
    });
    assert!(accent, "the selected chip is filled with `sidebar_accent`");
    assert!(
        !ghost,
        "the selected chip is not the ghost `secondary_active`"
    );
}

#[gpui_kit::test]
fn composer_chips_share_the_queue_chip_height(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    wait_for_options(&live, cx, &id);

    // Plan Global Constraints: composer chips keep the M1b chip height, 1.625 rem.
    live.window(cx, |window, _| {
        let expected = f32::from(window.rem_size()) * 1.625;
        for chip in ["composer-plan-mode", "composer-context", "composer-model"]
        {
            let height = f32::from(window.find(chip).bounds().size.height);
            assert!(
                (height - expected).abs() < 0.5,
                "{chip} is {height}px tall, expected {expected}px"
            );
        }
    });
}

#[gpui_kit::test]
fn model_menu_switches_thinking_level(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    wait_for_options(&live, cx, &id);

    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-model"), cx)
    });
    live.window(cx, |window, cx| click_label(window, "High", cx));

    eventually(cx, "thinking level", |cx| {
        current(&live, cx, &id, "thinking_level") == "high"
    });
    live.until(cx, "chip label", |window| {
        painted(window)
            .iter()
            .any(|text| text.text().ends_with("· High"))
    });
}

#[gpui_kit::test]
fn context_ring_shows_percent_after_a_reply(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["hello".into()]);
    live.until(cx, "reply", |window| shows(window, "Hi there"));

    live.until(cx, "percent", |window| {
        painted(window)
            .iter()
            .any(|text| text.text().ends_with('%'))
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-context"), cx)
    });
    live.until(cx, "context details", |window| {
        painted(window)
            .iter()
            .any(|text| text.text().ends_with("tokens"))
    });
}

#[gpui_kit::test]
fn failed_switch_reverts_and_notifies(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    wait_for_options(&live, cx, &id);
    let model = cx.update(|cx| current(&live, cx, &id, "model"));

    live.window(cx, |window, cx| {
        active(&live, cx).update(cx, |view, cx| {
            view.set_config("model", "nope/none".to_string(), window, cx)
        })
    });

    let deadline = std::time::Instant::now() + support::WAIT;
    while live.window(cx, |window, cx| window.notifications(cx).len()) != 1 {
        assert!(std::time::Instant::now() < deadline, "no failure notice");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.update(|cx| assert_eq!(current(&live, cx, &id, "model"), model));
    live.window(cx, |window, _| {
        assert_ne!(window.find("composer-model").disabled(), Some(true))
    });
}

#[gpui_kit::test]
fn draft_tab_controls_are_disabled(cx: &mut TestAppContext) {
    let mut backend = support::ScriptedBackend::start();
    let (handle, view, _dir) =
        support::shell::open(cx, fixture_state(&backend));
    support::shell::connect(cx, &view, &mut backend);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-n", cx);
        window.render_frame(cx);
        assert_eq!(window.find("composer-model").disabled(), Some(true));
        assert_eq!(window.find("composer-plan-mode").disabled(), Some(true));
    })
    .expect("window");
}

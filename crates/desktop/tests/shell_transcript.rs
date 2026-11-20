//! Transcript rendering: replayed and streamed messages, folds, tool details, and images.

mod support;

use agent_client_protocol::v2::schema as wire;
use desktop::state::DesktopState;
use desktop::ui::{MarkdownCache, MarkdownPart, TabEntry};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{SharedString, TestAppContext};
use support::shell::{live, painted, shows};

/// A 1×1 transparent PNG.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

#[gpui_kit::test]
fn replayed_and_streamed_messages_render(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["hello".into()]);
    live.until(cx, "live reply", |window| shows(window, "Hi there"));

    // Closing and reopening the tab replays the transcript from the backend.
    live.view.update(cx, |view, cx| view.close_tab(0, cx));
    support::shell::eventually(cx, "session released", |cx| {
        live.session(&id, cx).is_none()
    });
    live.open(cx, &id);
    live.until(cx, "replayed turn", |window| {
        shows(window, "hello") && shows(window, "Hi there")
    });

    live.backend
        .block_on(live.backend.client.prompt(&id, vec!["think".into()]))
        .expect("prompt");
    live.until(cx, "streamed reply with a thinking header", |window| {
        shows(window, "done")
            && painted(window)
                .iter()
                .any(|text| text.text().starts_with("Thinking"))
    });
}

#[gpui_kit::test]
fn thinking_is_collapsed_until_clicked(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["think".into()]);
    live.until(cx, "reply", |window| shows(window, "done"));
    assert!(!live.window(cx, |window, _| shows(window, "pondering")));

    let message = cx.update(|cx| {
        let handle = live.session(&id, cx).expect("attached");
        let projection = handle.read(cx).projection();
        (0..projection.len())
            .find_map(|index| match projection.row(index) {
                Some(desktop::projection::TranscriptRow::Message(message))
                    if !message.reasoning.is_empty() =>
                {
                    Some(message.message_id.clone())
                }
                _ => None,
            })
            .expect("assistant message with thinking")
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from(format!("thinking:{message}")), cx)
    });
    live.until(cx, "thinking text", |window| shows(window, "pondering"));
}

#[gpui_kit::test]
fn finished_run_folds_and_expands(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["read".into()]);
    let prompt = live.user_message(cx, &id, "read");
    let run = SharedString::from(format!("run:message:{prompt}"));
    live.until(cx, "folded run", |window| {
        window
            .try_find(run.clone())
            .is_some_and(|row| row.visible())
    });
    assert!(
        live.window(cx, |window, _| window.try_find("tool:read-1").is_none())
    );

    live.window(cx, |window, cx| window.click(run.clone(), cx));
    live.until(cx, "expanded run", |window| {
        window
            .try_find("tool:read-1")
            .is_some_and(|row| row.visible())
            && shows(window, "notes.txt")
    });

    // The summary row stays, so the run folds again.
    live.window(cx, |window, cx| window.click(run.clone(), cx));
    live.until(cx, "refolded run", |window| {
        window.try_find("tool:read-1").is_none()
    });
}

#[gpui_kit::test]
fn edit_tool_shows_line_stats(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["edit".into()]);
    let prompt = live.user_message(cx, &id, "edit");
    let run = SharedString::from(format!("run:message:{prompt}"));
    live.until(cx, "folded run", |window| {
        window.try_find(run.clone()).is_some()
    });

    live.window(cx, |window, cx| window.click(run.clone(), cx));
    live.until(cx, "edit stats", |window| {
        shows(window, "+1") && shows(window, "\u{2212}1")
    });
}

#[gpui_kit::test]
fn exec_output_shows_the_tail_until_expanded(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["exec30".into()]);
    let prompt = live.user_message(cx, &id, "exec30");
    let run = SharedString::from(format!("run:message:{prompt}"));
    live.until(cx, "folded run", |window| {
        window.try_find(run.clone()).is_some()
    });
    live.window(cx, |window, cx| window.click(run.clone(), cx));
    live.until(cx, "tool row", |window| {
        window.try_find("tool:exec-30").is_some()
    });

    live.window(cx, |window, cx| window.click("tool:exec-30", cx));
    live.until(cx, "output tail", |window| {
        shows(window, "30") && shows(window, "11")
    });
    assert!(!live.window(cx, |window, _| shows(window, "10")));

    live.window(cx, |window, cx| window.click("full-output:exec-30", cx));
    // Line 10 lies outside the 20-line tail; the first lines may sit above the
    // viewport now that the composer shares the card's height.
    live.until(cx, "full output", |window| shows(window, "10"));
}

#[gpui_kit::test]
fn markdown_cache_appends_and_resets(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let id = wire::MessageId::from("m1".to_string());
    let mut cache = MarkdownCache::default();
    let first = cx.update(|cx| cache.sync(&id, MarkdownPart::Body, "a", cx));
    let appended =
        cx.update(|cx| cache.sync(&id, MarkdownPart::Body, "ab", cx));
    assert_eq!(first.entity_id(), appended.entity_id());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| first
            .read(cx)
            .rendered_text()
            .as_str()
            .trim_end()
            .to_string()),
        "ab"
    );

    let reset = cx.update(|cx| cache.sync(&id, MarkdownPart::Body, "x", cx));
    assert_eq!(first.entity_id(), reset.entity_id());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| first
            .read(cx)
            .rendered_text()
            .as_str()
            .trim_end()
            .to_string()),
        "x"
    );
    // The thinking text of the same message is a separate document.
    let thinking =
        cx.update(|cx| cache.sync(&id, MarkdownPart::Thinking, "x", cx));
    assert_ne!(first.entity_id(), thinking.entity_id());
}

#[gpui_kit::test]
fn user_images_render_as_thumbnails(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(
        cx,
        &id,
        vec![
            "img".into(),
            wire::ContentBlock::Image(wire::ImageContent::new(
                PIXEL_PNG,
                wire::MediaType::new("image/png"),
            )),
        ],
    );
    let prompt = live.user_message(cx, &id, "img");
    let image = SharedString::from(format!("image:{prompt}:0"));
    live.until(cx, "thumbnail", |window| {
        window
            .try_find(image.clone())
            .is_some_and(|image| image.visible())
    });
}

#[gpui_kit::test]
fn multi_line_command_shows_one_line_until_expanded(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.run(cx, &id, vec!["execml".into()]);
    let prompt = live.user_message(cx, &id, "execml");
    let run = SharedString::from(format!("run:message:{prompt}"));
    live.until(cx, "folded run", |window| {
        window.try_find(run.clone()).is_some()
    });
    live.window(cx, |window, cx| window.click(run.clone(), cx));
    // The collapsed row keeps to one line: the first line and a marker.
    live.until(cx, "one-line target", |window| {
        shows(window, "echo first \u{2026}")
    });
    assert!(!live.window(cx, |window, _| shows(window, "echo second")));

    // Expanded, the full command is readable.
    live.window(cx, |window, cx| window.click("tool:exec-ml", cx));
    live.until(cx, "full command", |window| {
        shows(window, "echo second") && shows(window, "echo third")
    });
}

#[gpui_kit::test]
fn active_tab_uses_the_muted_fill(cx: &mut TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;

    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    let active = live.open_new_session(cx);
    let tab_id = format!("tab:{}", TabEntry::Session(active).key());
    // Mockup `.tab.cur`: the selected tab is a `muted` fill, never a `primary` pill.
    let (muted, primary) = live.window(cx, |window, cx| {
        let tab = window.try_find(tab_id).expect("active tab").bounds();
        let scale = window.scale_factor();
        let fills: Vec<gpui_kit::Background> = window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let left = quad.bounds.origin.x.0 / scale;
                let top = quad.bounds.origin.y.0 / scale;
                (left - f32::from(tab.origin.x)).abs() < 1.
                    && (top - f32::from(tab.origin.y)).abs() < 1.
            })
            .map(|quad| quad.background)
            .collect();
        let theme = cx.theme();
        (
            fills.contains(&theme.muted.into()),
            fills.contains(&theme.primary.into()),
        )
    });
    assert!(muted, "the active tab is filled with `muted`");
    assert!(!primary, "the active tab is not a `primary` pill");
}

//! The chat / trajectory switch and the trajectory view: rows, selection,
//! loading states, live refresh, and following the newest records.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use desktop::actions::ToggleTrajectory;
use desktop::backend::SessionId;
use desktop::state::DesktopState;
use desktop::ui::{LoadState, TrajectoryEvent, TrajectoryView, ViewMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, ScrollDelta, TestAppContext, point, px};
use protocol::{
    AssistantRecord, CommandKind, CommandRecord, ContentHash, ExtensionRecord,
    ModelUsage, RequestOptions, RequestRecord, RequestSnapshot, RunId, RunRef,
    SettingChange, StopReason, ThinkingLevel, TimestampMs, ToolCallId,
    ToolRecord, TrajectoryCursor, TrajectoryRecord, TrajectoryRecordKind,
    UserRecord,
};
use support::shell::{Live, eventually, live, live_with_history};
use support::trajectory::{
    drawn, in_run, open_trajectory, page, session_view, show, switch,
};

/// A user record in Run 1.
fn user(id: &str, sequence: u64) -> TrajectoryRecord {
    in_run(
        id,
        sequence,
        TrajectoryRecordKind::User(
            UserRecord::builder().text(id.into()).build(),
        ),
    )
}

/// One record of every kind.
fn every_kind() -> Vec<TrajectoryRecord> {
    let snapshot = RequestSnapshot::builder()
        .system_prompt(ContentHash::of(b"p"))
        .tools(ContentHash::of(b"t"))
        .options(
            RequestOptions::builder()
                .provider_id("deepseek".into())
                .model_id("flash".into())
                .thinking_level(ThinkingLevel::Off)
                .build(),
        )
        .message_count(1)
        .build();
    let at = TimestampMs::from(1_003);
    vec![
        user("u1", 1),
        in_run(
            "c1",
            2,
            TrajectoryRecordKind::Command(CommandRecord {
                kind: CommandKind::Slash,
                text: "/review".into(),
            }),
        ),
        in_run(
            "request:a1",
            3,
            TrajectoryRecordKind::Request(
                RequestRecord::builder()
                    .index(1)
                    .snapshot(snapshot)
                    .first(true)
                    .build(),
            ),
        ),
        in_run(
            "a1",
            3,
            TrajectoryRecordKind::Assistant(
                AssistantRecord::builder()
                    .request_index(1)
                    .provider_id("deepseek".into())
                    .model_id("flash".into())
                    .text("answer".into())
                    .usage(ModelUsage::default())
                    .requested_at_ms(at)
                    .first_token_at_ms(at)
                    .last_token_at_ms(at)
                    .stop_reason(StopReason::ToolUse)
                    .build(),
            ),
        ),
        in_run(
            "tool:t1",
            3,
            TrajectoryRecordKind::Tool(
                ToolRecord::builder()
                    .call_id(ToolCallId::try_from("t1").expect("call"))
                    .name("read".into())
                    .arguments(serde_json::json!({"path": "a.rs"}))
                    .result_text(Some("done".into()))
                    .build(),
            ),
        ),
        in_run(
            "k1",
            5,
            TrajectoryRecordKind::Compaction {
                summary: "summary".into(),
                tokens_before: Some(100),
            },
        ),
        in_run(
            "b1",
            6,
            TrajectoryRecordKind::BranchSummary {
                summary: "branch".into(),
            },
        ),
        in_run(
            "s1",
            7,
            TrajectoryRecordKind::Setting(SettingChange::Model {
                provider: "deepseek".into(),
                model: "pro".into(),
            }),
        ),
        in_run(
            "e1",
            8,
            TrajectoryRecordKind::Extension(ExtensionRecord {
                extension_id: "notes".into(),
                custom_type: "note".into(),
                text: "remember".into(),
            }),
        ),
    ]
}

/// The active tab's view mode.
fn mode(live: &Live, cx: &mut TestAppContext) -> ViewMode {
    let view = session_view(live, cx);
    cx.update(|cx| view.read(cx).view_mode())
}

/// Switch shows the trajectory and back.
#[gpui_kit::test]
fn switch_shows_the_trajectory_and_back(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, every_kind());
    assert!(drawn(&live, cx, "trajectory-table"));
    assert!(drawn(&live, cx, "composer-input"));

    switch(&live, cx, 0);
    assert_eq!(mode(&live, cx), ViewMode::Chat);
    assert!(!drawn(&live, cx, "trajectory-table"));
    assert!(drawn(&live, cx, "composer-input"));
}

/// Each tab remembers its view.
#[gpui_kit::test]
fn each_tab_remembers_its_view(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let first = live.open_new_session(cx);
    switch(&live, cx, 1);
    let second = live.open_new_session(cx);
    assert_eq!(mode(&live, cx), ViewMode::Chat);

    live.open(cx, &first);
    assert_eq!(mode(&live, cx), ViewMode::Trajectory);
    live.open(cx, &second);
    assert_eq!(mode(&live, cx), ViewMode::Chat);
}

/// Toggle trajectory action switches the active tab.
#[gpui_kit::test]
fn toggle_trajectory_action_switches_the_active_tab(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.open_new_session(cx);
    live.window(cx, |window, cx| {
        window.dispatch_action(Box::new(ToggleTrajectory), cx)
    });
    assert_eq!(mode(&live, cx), ViewMode::Trajectory);
    live.window(cx, |window, cx| {
        window.dispatch_action(Box::new(ToggleTrajectory), cx)
    });
    assert_eq!(mode(&live, cx), ViewMode::Chat);
}

/// Rows render every kind.
#[gpui_kit::test]
fn rows_render_every_kind(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, every_kind());

    assert!(drawn(&live, cx, "run-header:1"));
    for record in every_kind() {
        let id = format!("ledger-row:{}", record.id);
        assert!(drawn(&live, cx, &id), "{id} not drawn");
    }
}

/// Enter opens the selected record.
#[gpui_kit::test]
fn enter_opens_the_selected_record(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, every_kind());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _subscription = cx.update(|cx| {
        let opened = Rc::clone(&opened);
        cx.subscribe(&view, move |_, event: &TrajectoryEvent, _| {
            opened.borrow_mut().push(event.clone());
        })
    });

    live.window(cx, |window, cx| window.click("ledger-row:u1", cx));
    live.window(cx, |window, cx| window.press("enter", cx));

    assert_eq!(*opened.borrow(), [TrajectoryEvent::Open("u1".into())]);
    let selected = cx.update(|cx| {
        view.read(cx)
            .model()
            .selected_record()
            .map(|r| r.id.clone())
    });
    assert_eq!(selected.as_deref(), Some("u1"));
}

/// A live run appears in the open trajectory.
#[gpui_kit::test]
fn a_live_run_appears_in_the_open_trajectory(cx: &mut TestAppContext) {
    let live = live_with_history(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    let id: SessionId = cx.update(|cx| view.read(cx).session_id(cx));

    live.run(cx, &id, vec!["read".into()]);
    settle(
        cx,
        "the first run's requests and tool result",
        |model| {
            requests(model) == 2 && model.records().iter().any(|r| {
            matches!(&r.kind, TrajectoryRecordKind::Tool(tool) if tool.result_text.is_some())
        })
        },
        &view,
    );

    // A second run appends; the leaf moves but the branch stays.
    live.run(cx, &id, vec!["hello".into()]);
    settle(
        cx,
        "the second run's request beside the first run's",
        |model| {
            requests(model) == 3
                && model
                    .records()
                    .iter()
                    .filter(|r| r.run.is_some())
                    .any(|r| r.run.as_ref().is_some_and(|run| run.index == 1))
        },
        &view,
    );
}

/// The request records loaded.
fn requests(model: &desktop::trajectory::TrajectoryModel) -> usize {
    model
        .records()
        .iter()
        .filter(|r| r.id.starts_with("request:"))
        .count()
}

/// Waits until the trajectory's model satisfies `check`; refreshes wait out
/// a debounce on the test clock, which only moves on demand.
fn settle(
    cx: &mut TestAppContext,
    what: &str,
    check: impl Fn(&desktop::trajectory::TrajectoryModel) -> bool,
    view: &Entity<TrajectoryView>,
) {
    let deadline = std::time::Instant::now() + support::WAIT;
    while !cx.update(|cx| check(view.read(cx).model())) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Unavailable history offers retry.
#[gpui_kit::test]
fn unavailable_history_offers_retry(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    eventually(cx, "history unavailable", |cx| {
        view.read(cx).load_state() == &LoadState::Unavailable
    });
    assert!(drawn(&live, cx, "trajectory-retry"));
    assert!(!drawn(&live, cx, "trajectory-table"));
}

/// Following stops after scrolling up.
#[gpui_kit::test]
fn following_stops_after_scrolling_up(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(
        &view,
        cx,
        (1..=80).map(|n| user(&format!("u{n}"), n)).collect(),
    );
    live.window(cx, |_, _| ());
    assert!(!drawn(&live, cx, "trajectory-latest"), "follows at the end");

    live.window(cx, |window, cx| {
        window.scroll(
            "trajectory-table",
            ScrollDelta::Pixels(point(px(0.), px(4_000.))),
            cx,
        )
    });
    view.update(cx, |view, cx| {
        let mut newer = page(vec![user("u81", 81)], "leaf", false);
        newer.first_sequence = Some(81);
        view.apply_page(TrajectoryCursor::After(80), newer, cx)
    });
    assert!(drawn(&live, cx, "trajectory-latest"));
    assert!(!drawn(&live, cx, "ledger-row:u81"), "the view stays put");

    // The button sits over the ledger and must take the click itself.
    live.window(cx, |window, cx| window.click("trajectory-latest", cx));
    live.until(cx, "the newest row", |window| {
        window.try_find("ledger-row:u81").is_some()
    });
    assert!(!drawn(&live, cx, "trajectory-latest"), "follows again");
}

/// Draft tab has no switch.
#[gpui_kit::test]
fn draft_tab_has_no_switch(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.window(cx, |window, cx| {
        window.dispatch_action(Box::new(desktop::actions::NewSession), cx)
    });
    cx.update(|cx| {
        assert!(matches!(
            live.view.read(cx).active_entry(),
            Some(desktop::ui::TabEntry::Draft { .. })
        ))
    });
    // A tab bar's id names its tabs' path, not an element of its own.
    let switch = gpui_kit::ElementId::Name("view-switch".into());
    let shown = live.window(cx, |window, _| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|element| element.path().contains(&switch))
    });
    assert!(!shown);
}

/// Three requests of the first Run, 1 s apart.
fn three_requests() -> Vec<TrajectoryRecord> {
    (0..3)
        .map(|n| {
            let sent = 10_000 + n * 1_000;
            let at = |ms| TimestampMs::from(ms);
            TrajectoryRecord::builder()
                .id(format!("a{n}"))
                .sequence(n + 1)
                .run(Some(RunRef {
                    id: RunId::try_from("run-1").expect("run"),
                    index: 1,
                }))
                .started_at_ms(Some(at(sent)))
                .ended_at_ms(Some(at(sent + 600)))
                .kind(TrajectoryRecordKind::Assistant(
                    AssistantRecord::builder()
                        .request_index(u32::try_from(n + 1).expect("index"))
                        .provider_id("deepseek".into())
                        .model_id("flash".into())
                        .usage(ModelUsage::default())
                        .requested_at_ms(at(sent))
                        .first_token_at_ms(at(sent + 200))
                        .last_token_at_ms(at(sent + 600))
                        .stop_reason(StopReason::EndTurn)
                        .build(),
                ))
                .build()
        })
        .collect()
}

/// The timeline's bounds and the window-space center of the second
/// request bar, laid out the way the canvas lays it out.
fn second_bar(
    live: &Live,
    cx: &mut TestAppContext,
) -> (
    gpui_kit::Bounds<gpui_kit::Pixels>,
    gpui_kit::Point<gpui_kit::Pixels>,
    f32,
) {
    use desktop::trajectory::timeline::{
        DurationMode, Lane, TimelineLayout, Viewport,
    };
    let (bounds, rem) = live.window(cx, |window, _| {
        (
            window.find("trajectory-timeline").bounds(),
            window.rem_size(),
        )
    });
    let layout = TimelineLayout::new(
        &three_requests(),
        &[],
        20_000,
        DurationMode::Actual,
    );
    let width = f32::from(bounds.size.width);
    let bars = layout.bars(&Viewport::full(layout.domain()), width);
    let bar = bars.iter().find(|bar| bar.record_id == "a1").expect("bar");
    let (top, height) = Lane::Request.band_rems();
    let y = bounds.origin.y + rem * (top + height / 2.0);
    let x = bounds.origin.x + px(bar.x + bar.w / 2.0);
    (bounds, point(x, y), bar.w)
}

/// Clicking a bar selects its record.
#[gpui_kit::test]
fn clicking_a_bar_selects_its_record(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, three_requests());

    let (_, center, _) = second_bar(&live, cx);
    live.window(cx, |window, cx| {
        use gpui_kit::test::TestEventExt as _;
        window.pointer_move(center, None, cx);
        window.pointer_down(center, gpui_kit::MouseButton::Left, cx);
        window.pointer_up(center, gpui_kit::MouseButton::Left, cx);
    });

    let selected = cx.update(|cx| {
        view.read(cx)
            .model()
            .selected_record()
            .map(|r| r.id.clone())
    });
    assert_eq!(selected.as_deref(), Some("a1"));
}

/// Dragging filters the table.
#[gpui_kit::test]
fn dragging_filters_the_table(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, three_requests());

    let (_, center, w) = second_bar(&live, cx);
    let from = point(center.x - px(w / 2.0 - 1.0), center.y);
    let to = point(center.x + px(w / 2.0 - 1.0), center.y);
    live.window(cx, |window, cx| window.drag(from, to, cx));

    assert!(cx.update(|cx| view.read(cx).model().interval().is_some()));
    assert!(drawn(&live, cx, "ledger-row:a1"));
    assert!(!drawn(&live, cx, "ledger-row:a0"));
    assert!(!drawn(&live, cx, "ledger-row:a2"));
    assert!(drawn(&live, cx, "trajectory-interval-clear"));

    live.window(cx, |window, cx| {
        window.click("trajectory-interval-clear", cx)
    });
    assert!(cx.update(|cx| view.read(cx).model().interval().is_none()));
    assert!(drawn(&live, cx, "ledger-row:a0"));
}

/// Timeline toggle hides the overview.
#[gpui_kit::test]
fn timeline_toggle_hides_the_overview(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, three_requests());
    assert!(drawn(&live, cx, "trajectory-timeline"));

    live.window(cx, |window, cx| {
        window.click("trajectory-timeline-toggle", cx)
    });
    assert!(!drawn(&live, cx, "trajectory-timeline"));
    assert!(cx.update(|cx| !live.view.read(cx).state().layout.timeline_open));
}

/// A short ledger offers no jump to latest.
#[gpui_kit::test]
fn a_short_ledger_offers_no_jump_to_latest(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    show(&view, cx, every_kind());
    live.window(cx, |_, _| ());
    assert!(!drawn(&live, cx, "trajectory-latest"));
}

/// The accessible label of the in-flight row: its cells, joined.
fn in_flight_label(window: &mut gpui_kit::Window) -> Option<String> {
    window.try_find("trajectory-table")?;
    window
        .within("trajectory-table")
        .elements()
        .into_iter()
        .find(|element| {
            matches!(element.path().last(), Some(gpui_kit::ElementId::Name(name)) if name.starts_with("in-flight:"))
        })
        .and_then(|element| element.label().map(str::to_owned))
}

/// In flight time advances.
#[gpui_kit::test]
fn in_flight_time_advances(cx: &mut TestAppContext) {
    let live = live_with_history(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    let id: SessionId = cx.update(|cx| view.read(cx).session_id(cx));
    let client = live.backend.client.clone();
    let runtime = live.backend.runtime.handle().clone();
    let run = std::thread::spawn(move || {
        runtime.block_on(client.prompt(&id, vec!["working".into()]))
    });

    live.until(cx, "the in-flight row", |window| {
        in_flight_label(window).is_some()
    });
    // Let the refresh for the user message land, so nothing rebuilds the rows.
    for _ in 0..3 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let before = live.window(cx, |window, _| in_flight_label(window));
    // The shell's one-second ticker redraws a running session; the row's
    // elapsed time must change while the run is still going.
    let deadline = std::time::Instant::now() + support::WAIT;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(150));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        let now = live.window(cx, |window, _| in_flight_label(window));
        if now.is_some() && now != before {
            break;
        }
        assert!(
            now.is_some() && std::time::Instant::now() < deadline,
            "the in-flight time stayed {before:?} until {now:?}"
        );
    }
    run.join().expect("prompt thread").expect("prompt");
}

/// The ledger rows the window draws, by record id.
fn drawn_rows(live: &Live, cx: &mut TestAppContext) -> Vec<String> {
    live.window(cx, |window, _| {
        let mut rows: Vec<String> =
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter_map(|element| match element.path().last()? {
                    gpui_kit::ElementId::Name(name)
                        if name.starts_with("ledger-row:") =>
                    {
                        Some(name.to_string())
                    }
                    _ => None,
                })
                .collect();
        rows.sort();
        rows.dedup();
        rows
    })
}

/// Loading earlier records keeps the rows in view where they were, even
/// with a selected row, whose index the new rows push down.
#[gpui_kit::test]
fn loading_earlier_keeps_the_view(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    let users = |range: std::ops::RangeInclusive<u64>| {
        range.map(|n| user(&format!("u{n}"), n)).collect::<Vec<_>>()
    };
    view.update(cx, |view, cx| {
        view.apply_page(
            TrajectoryCursor::Tail,
            page(users(41..=120), "leaf", true),
            cx,
        )
    });
    live.window(cx, |window, cx| {
        window.scroll(
            "trajectory-table",
            ScrollDelta::Pixels(point(px(0.), px(1_200.))),
            cx,
        )
    });
    let before = drawn_rows(&live, cx);
    let middle = before[before.len() / 2].clone();
    live.window(cx, |window, cx| {
        window.click(gpui_kit::SharedString::from(middle), cx)
    });
    let before = drawn_rows(&live, cx);

    view.update(cx, |view, cx| {
        let mut earlier = page(users(1..=40), "leaf", false);
        earlier.has_more_before = false;
        view.apply_page(TrajectoryCursor::Before(41), earlier, cx)
    });

    assert_eq!(drawn_rows(&live, cx), before);
}

/// Loading earlier records from the top, where "load earlier" sits above
/// the first record, keeps that record in place: the rows come in above
/// what was on screen.
#[gpui_kit::test]
fn loading_earlier_from_the_top_keeps_the_view(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let view = open_trajectory(&live, cx);
    let users = |range: std::ops::RangeInclusive<u64>| {
        range.map(|n| user(&format!("u{n}"), n)).collect::<Vec<_>>()
    };
    view.update(cx, |view, cx| {
        view.apply_page(
            TrajectoryCursor::Tail,
            page(users(41..=120), "leaf", true),
            cx,
        )
    });
    live.window(cx, |window, cx| {
        window.scroll(
            "trajectory-table",
            ScrollDelta::Pixels(point(px(0.), px(100_000.))),
            cx,
        )
    });
    let before = drawn_rows(&live, cx);
    assert!(before.contains(&"ledger-row:u41".to_string()), "{before:?}");

    view.update(cx, |view, cx| {
        let mut earlier = page(users(1..=40), "leaf", false);
        earlier.has_more_before = false;
        view.apply_page(TrajectoryCursor::Before(41), earlier, cx)
    });

    let after = drawn_rows(&live, cx);
    assert!(after.contains(&"ledger-row:u41".to_string()), "{after:?}");
    assert!(!after.contains(&"ledger-row:u1".to_string()), "{after:?}");
}

//! The record detail panel tab: opening it from the trajectory, its
//! sections per record kind, following the selection, and its states.

mod support;

use desktop::state::{DesktopState, PanelTabKey};
use desktop::ui::{LoadState, RecordSection};
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use protocol::{
    AssistantRecord, ContentHash, EntryId, ModelUsage, RequestOptions,
    RequestRecord, RequestSnapshot, StopReason, ThinkingLevel, TimestampMs,
    ToolCallId, ToolRecord, TrajectoryCursor, TrajectoryPage, TrajectoryRecord,
    TrajectoryRecordKind, UserRecord,
};
use support::shell::{Live, eventually, live, live_with_history, shows};
use support::trajectory::{drawn, in_run, open_trajectory, session_view, show};

/// Request 1, its answer calling one tool, and the tool; the request's
/// prompt changed when `changed`.
fn request_records(changed: bool) -> Vec<TrajectoryRecord> {
    let snapshot = RequestSnapshot::builder()
        .system_prompt(ContentHash::of(b"absent prompt"))
        .tools(ContentHash::of(b"[]"))
        .options(
            RequestOptions::builder()
                .provider_id("deepseek".into())
                .model_id("flash".into())
                .thinking_level(ThinkingLevel::Off)
                .build(),
        )
        .message_count(1)
        .build();
    let at = TimestampMs::from;
    let mut earlier = snapshot.clone();
    earlier.system_prompt = ContentHash::of(b"earlier prompt");
    vec![
        // The request before, for the prompt to be compared with.
        in_run(
            "request:a0",
            0,
            TrajectoryRecordKind::Request(
                RequestRecord::builder()
                    .index(1)
                    .snapshot(earlier)
                    .first(true)
                    .build(),
            ),
        ),
        in_run(
            "u1",
            1,
            TrajectoryRecordKind::User(
                UserRecord::builder().text("go".into()).build(),
            ),
        ),
        in_run(
            "request:a1",
            2,
            TrajectoryRecordKind::Request(
                RequestRecord::builder()
                    .index(2)
                    .snapshot(snapshot)
                    .prompt_changed(changed)
                    .build(),
            ),
        ),
        in_run(
            "a1",
            2,
            TrajectoryRecordKind::Assistant(
                AssistantRecord::builder()
                    .request_index(2)
                    .provider_id("deepseek".into())
                    .model_id("flash".into())
                    .text("answer".into())
                    .tool_call_ids(vec![
                        ToolCallId::try_from("t1").expect("call"),
                    ])
                    .usage(ModelUsage::default())
                    .requested_at_ms(at(1_002))
                    .first_token_at_ms(at(1_200))
                    .last_token_at_ms(at(1_502))
                    .stop_reason(StopReason::ToolUse)
                    .build(),
            ),
        ),
        in_run(
            "tool:t1",
            2,
            TrajectoryRecordKind::Tool(
                ToolRecord::builder()
                    .call_id(ToolCallId::try_from("t1").expect("call"))
                    .name("read".into())
                    .arguments(serde_json::json!({"path": "a.rs"}))
                    .result_text(Some("fn main() {}".into()))
                    .build(),
            ),
        ),
    ]
}

/// Selects `id` and presses Enter in the ledger, opening the record tab.
fn open_record(live: &Live, cx: &mut TestAppContext, id: &str) {
    let row = format!("ledger-row:{id}");
    live.window(cx, |window, cx| {
        window.click(gpui_kit::SharedString::from(row), cx)
    });
    live.window(cx, |window, cx| window.press("enter", cx));
}

/// The section labels the record tab shows, left to right, read from the
/// switch's tabs.
fn sections(live: &Live, cx: &mut TestAppContext) -> Vec<String> {
    live.window(cx, |window, _| {
        let mut tabs = window
            .within("record-sections")
            .find_all_by_role(gpui_kit::Role::Tab);
        tabs.sort_by(|a, b| {
            f32::from(a.bounds().origin.x)
                .total_cmp(&f32::from(b.bounds().origin.x))
        });
        tabs.iter()
            .filter_map(|tab| tab.label().map(str::to_owned))
            .collect()
    })
}

/// The labels of `sections`, as the tab draws them.
fn labels(sections: &[RecordSection]) -> Vec<String> {
    sections
        .iter()
        .map(|section| section.label().to_string())
        .collect()
}

/// Opening a record shows its sections.
#[gpui_kit::test]
fn opening_a_record_shows_its_sections(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    show(&open_trajectory(&live, cx), cx, request_records(false));
    open_record(&live, cx, "request:a1");

    assert!(drawn(&live, cx, "panel-tab:record"));
    // The header marks the kind before the title; a request's title names
    // it already, so its kind is not spelled out a second time.
    assert!(drawn(&live, cx, "record-kind"));
    assert!(!live.window(cx, |window, _| shows(window, "Request")));
    assert_eq!(
        sections(&live, cx),
        labels(&[
            RecordSection::Summary,
            RecordSection::Timing,
            RecordSection::Usage,
            RecordSection::SystemPrompt,
            RecordSection::Tools,
            RecordSection::Options,
            RecordSection::Raw,
        ])
    );
    // Other kinds are named next to their icon.
    open_record(&live, cx, "a1");
    assert!(live.window(cx, |window, _| shows(window, "Assistant")));
}

/// Detail follows the selection.
#[gpui_kit::test]
fn detail_follows_the_selection(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let trajectory = open_trajectory(&live, cx);
    show(&trajectory, cx, request_records(false));
    open_record(&live, cx, "request:a1");

    live.window(cx, |window, cx| window.click("record-next", cx));
    let selected = cx.update(|cx| {
        trajectory
            .read(cx)
            .model()
            .selected_record()
            .map(|r| r.id.clone())
    });
    assert_eq!(selected.as_deref(), Some("a1"));
    live.window(cx, |window, cx| window.click("record-prev", cx));
    let selected = cx.update(|cx| {
        trajectory
            .read(cx)
            .model()
            .selected_record()
            .map(|r| r.id.clone())
    });
    assert_eq!(selected.as_deref(), Some("request:a1"));
}

/// Tool record sections.
#[gpui_kit::test]
fn tool_record_sections(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    show(&open_trajectory(&live, cx), cx, request_records(false));
    open_record(&live, cx, "tool:t1");

    assert_eq!(
        sections(&live, cx),
        labels(&[
            RecordSection::Summary,
            RecordSection::Arguments,
            RecordSection::Result,
            RecordSection::Timing,
            RecordSection::Raw,
        ])
    );
}

/// Prompt diff toggle only when changed.
#[gpui_kit::test]
fn prompt_diff_toggle_only_when_changed(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    for changed in [false, true] {
        show(&open_trajectory(&live, cx), cx, request_records(changed));
        open_record(&live, cx, "request:a1");
        live.window(cx, |window, cx| {
            window.within("record-sections").click(3usize, cx)
        });
        assert_eq!(drawn(&live, cx, "prompt-mode:diff"), changed);
    }

    // A changed prompt whose previous request is not loaded has nothing to
    // compare with: only the full prompt is offered.
    let mut records = request_records(true);
    records.retain(|record| record.id != "request:a0");
    show(&open_trajectory(&live, cx), cx, records);
    open_record(&live, cx, "request:a1");
    live.window(cx, |window, cx| {
        window.within("record-sections").click(3usize, cx)
    });
    assert!(!drawn(&live, cx, "prompt-mode:diff"));
}

/// Record tab without selection shows hint.
#[gpui_kit::test]
fn record_tab_without_selection_shows_hint(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    show(&open_trajectory(&live, cx), cx, request_records(false));
    live.window(cx, |window, cx| {
        live.view.update(cx, |view, cx| {
            view.open_panel_tab(PanelTabKey::Record, window, cx)
        })
    });
    assert!(drawn(&live, cx, "record-empty"));
}

/// Missing content is reported.
#[gpui_kit::test]
fn missing_content_is_reported(cx: &mut TestAppContext) {
    let live = live_with_history(cx, DesktopState::default());
    show(&open_trajectory(&live, cx), cx, request_records(false));
    open_record(&live, cx, "request:a1");
    live.window(cx, |window, cx| {
        window.within("record-sections").click(3usize, cx)
    });

    let missing = desktop::ui::RecordView::missing_text();
    live.until(cx, "missing content notice", |window| {
        shows(window, &missing)
    });
}

/// A retargeted session gets a fresh record tab.
#[gpui_kit::test]
fn a_retargeted_session_gets_a_fresh_record_tab(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    show(&open_trajectory(&live, cx), cx, request_records(false));
    open_record(&live, cx, "request:a1");

    // Showing the session again drops its trajectory; the detail must follow
    // the new one, not the dropped one.
    let view = session_view(&live, cx);
    live.window(cx, |window, cx| {
        view.update(cx, |view, cx| {
            let handle = view.handle().cloned().expect("session");
            view.set_target(
                desktop::ui::SessionTarget::Session(handle),
                window,
                cx,
            )
        })
    });
    live.window(cx, |window, cx| {
        window.within("view-switch").click(1usize, cx)
    });
    let trajectory = live
        .window(cx, |window, cx| {
            view.update(cx, |view, cx| view.trajectory(window, cx))
        })
        .expect("trajectory");
    eventually(cx, "first read settled", |cx| {
        !matches!(trajectory.read(cx).load_state(), LoadState::Loading)
    });
    let grep = in_run(
        "tool:g1",
        9,
        TrajectoryRecordKind::Tool(
            ToolRecord::builder()
                .call_id(ToolCallId::try_from("g1").expect("call"))
                .name("grepper".into())
                .arguments(serde_json::json!({"pattern": "x"}))
                .build(),
        ),
    );
    trajectory.update(cx, |trajectory, cx| {
        let page = TrajectoryPage::builder()
            .first_sequence(Some(9))
            .last_sequence(Some(9))
            .records(vec![grep])
            .leaf_id(Some(EntryId::try_from("leaf-2").expect("leaf")))
            .build();
        trajectory.apply_page(TrajectoryCursor::Tail, page, cx)
    });
    open_record(&live, cx, "tool:g1");

    live.until(cx, "the new record in the detail", |window| {
        support::shell::painted(window).iter().any(|text| {
            text.text() == "grepper"
                && text.path().iter().any(|id| {
                    matches!(id, gpui_kit::ElementId::Name(name) if name.as_ref() == "record-view")
                })
        })
    });
}

/// An answer without its request offers no request sections.
#[gpui_kit::test]
fn an_answer_without_its_request_offers_no_request_sections(
    cx: &mut TestAppContext,
) {
    // Sessions recorded before request snapshots carry answers alone.
    let live = live(cx, DesktopState::default());
    let answer = support::trajectory::assistant(1, 1, None, (10, 0));
    show(&open_trajectory(&live, cx), cx, vec![answer]);
    open_record(&live, cx, "a1");

    assert_eq!(
        sections(&live, cx),
        labels(&[
            RecordSection::Summary,
            RecordSection::Timing,
            RecordSection::Usage,
            RecordSection::Raw,
        ])
    );
}

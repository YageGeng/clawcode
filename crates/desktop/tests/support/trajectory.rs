//! Trajectory records and pages built in memory, and the shell steps the
//! trajectory and record-detail tests share.

use desktop::ui::{LoadState, SessionView, TrajectoryView};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, TestAppContext};
use protocol::{
    AssistantRecord, ContentHash, EntryId, ModelUsage, RequestOptions,
    RequestRecord, RequestSnapshot, RunId, RunRef, StopReason, ThinkingLevel,
    TimestampMs, ToolCallId, ToolRecord, TrajectoryCursor, TrajectoryPage,
    TrajectoryRecord, TrajectoryRecordKind, UserRecord,
};

use super::shell::{Live, eventually};

/// A Run reference.
pub fn run(id: &str, index: u32) -> Option<RunRef> {
    Some(RunRef {
        id: RunId::try_from(id).expect("run"),
        index,
    })
}

/// A record of `kind` at `sequence`, spanning `span` milliseconds.
pub fn record(
    id: &str,
    sequence: u64,
    run: Option<RunRef>,
    span: (u64, Option<u64>),
    kind: TrajectoryRecordKind,
) -> TrajectoryRecord {
    TrajectoryRecord::builder()
        .id(id.to_string())
        .sequence(sequence)
        .run(run)
        .started_at_ms(Some(TimestampMs::from(span.0)))
        .ended_at_ms(span.1.map(TimestampMs::from))
        .kind(kind)
        .build()
}

/// A record of `kind` in Run 1, one millisecond long.
pub fn in_run(
    id: &str,
    sequence: u64,
    kind: TrajectoryRecordKind,
) -> TrajectoryRecord {
    let start = 1_000 + sequence;
    record(
        id,
        sequence,
        run("run-1", 1),
        (start, Some(start + 1)),
        kind,
    )
}

/// A user record whose text is its id.
pub fn user(id: &str, sequence: u64, run: Option<RunRef>) -> TrajectoryRecord {
    record(
        id,
        sequence,
        run,
        (sequence, None),
        TrajectoryRecordKind::User(
            UserRecord::builder().text(id.into()).build(),
        ),
    )
}

/// A request snapshot whose prompt hashes `prompt`.
pub fn snapshot(prompt: &[u8]) -> RequestSnapshot {
    RequestSnapshot::builder()
        .system_prompt(ContentHash::of(prompt))
        .tools(ContentHash::of(b"t"))
        .options(
            RequestOptions::builder()
                .provider_id("deepseek".into())
                .model_id("flash".into())
                .thinking_level(ThinkingLevel::Off)
                .build(),
        )
        .message_count(1)
        .build()
}

/// The request record `request:a{index}`.
pub fn request(
    index: u32,
    sequence: u64,
    run: Option<RunRef>,
) -> TrajectoryRecord {
    record(
        &format!("request:a{index}"),
        sequence,
        run,
        (sequence, Some(sequence + 1)),
        TrajectoryRecordKind::Request(
            RequestRecord::builder()
                .index(index)
                .snapshot(snapshot(b"p"))
                .first(index == 1)
                .build(),
        ),
    )
}

/// The output `a{index}` of request `index`, with `input` and `cached`
/// tokens and ten output tokens.
pub fn assistant(
    index: u32,
    sequence: u64,
    run: Option<RunRef>,
    (input, cached): (u64, u64),
) -> TrajectoryRecord {
    let usage = ModelUsage::from(
        protocol::Usage::new()
            .input_tokens(input)
            .output_tokens(10)
            .cached_input_tokens(cached),
    );
    let at = TimestampMs::from(sequence);
    record(
        &format!("a{index}"),
        sequence,
        run,
        (sequence, Some(sequence + 1)),
        TrajectoryRecordKind::Assistant(
            AssistantRecord::builder()
                .request_index(index)
                .provider_id("deepseek".into())
                .model_id("flash".into())
                .text(format!("answer {index}"))
                .usage(usage)
                .requested_at_ms(at)
                .first_token_at_ms(at)
                .last_token_at_ms(at)
                .stop_reason(StopReason::EndTurn)
                .build(),
        ),
    )
}

/// The call `tool:{call}` of `read`; `result` `None` while it runs.
pub fn tool(
    call: &str,
    sequence: u64,
    run: Option<RunRef>,
    result: Option<&str>,
) -> TrajectoryRecord {
    record(
        &format!("tool:{call}"),
        sequence,
        run,
        (sequence, result.map(|_| sequence + 2)),
        TrajectoryRecordKind::Tool(
            ToolRecord::builder()
                .call_id(ToolCallId::try_from(call).expect("call"))
                .name("read".into())
                .arguments(serde_json::json!({"path": "a.rs"}))
                .result_text(result.map(str::to_owned))
                .build(),
        ),
    )
}

/// A page of `records` on branch `leaf`, with earlier entries when `more`.
pub fn page(
    records: Vec<TrajectoryRecord>,
    leaf: &str,
    more: bool,
) -> TrajectoryPage {
    TrajectoryPage::builder()
        .first_sequence(records.first().map(|record| record.sequence))
        .last_sequence(records.last().map(|record| record.sequence))
        .records(records)
        .leaf_id(Some(EntryId::try_from(leaf).expect("leaf")))
        .has_more_before(more)
        .build()
}

/// The active tab's session view.
pub fn session_view(
    live: &Live,
    cx: &mut TestAppContext,
) -> Entity<SessionView> {
    cx.update(|cx| live.view.read(cx).active_view().cloned().expect("view"))
}

/// Clicks tab `index` of the chat / trajectory switch: 0 chat, 1 trajectory.
pub fn switch(live: &Live, cx: &mut TestAppContext, index: usize) {
    live.window(cx, |window, cx| {
        window.within("view-switch").click(index, cx)
    });
}

/// The active tab's trajectory view, created when missing.
pub fn trajectory(
    live: &Live,
    cx: &mut TestAppContext,
) -> Entity<TrajectoryView> {
    let view = session_view(live, cx);
    live.window(cx, |window, cx| {
        view.update(cx, |view, cx| view.trajectory(window, cx))
    })
    .expect("trajectory")
}

/// Shows the active tab's trajectory and waits for its first read.
pub fn show_trajectory(
    live: &Live,
    cx: &mut TestAppContext,
) -> Entity<TrajectoryView> {
    switch(live, cx, 1);
    let view = trajectory(live, cx);
    eventually(cx, "first read settled", |cx| {
        !matches!(view.read(cx).load_state(), LoadState::Loading)
    });
    view
}

/// A new session shown in the trajectory view, its first read settled.
pub fn open_trajectory(
    live: &Live,
    cx: &mut TestAppContext,
) -> Entity<TrajectoryView> {
    live.open_new_session(cx);
    show_trajectory(live, cx)
}

/// Applies `records` as the tail page of branch `leaf`.
pub fn show(
    view: &Entity<TrajectoryView>,
    cx: &mut TestAppContext,
    records: Vec<TrajectoryRecord>,
) {
    view.update(cx, |view, cx| {
        view.apply_page(
            TrajectoryCursor::Tail,
            page(records, "leaf", false),
            cx,
        )
    });
}

/// Whether the last frame drew an element with this id.
pub fn drawn(live: &Live, cx: &mut TestAppContext, id: &str) -> bool {
    let id = gpui_kit::SharedString::from(id.to_string());
    live.window(cx, |window, _| window.try_find(id).is_some())
}

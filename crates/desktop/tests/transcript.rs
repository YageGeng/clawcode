//! Transcript layout: grouping, run folding, minimal splices, and labels.

use std::collections::HashSet;

use agent_client_protocol::v2::schema as wire;
use desktop::projection::{RowKey, RunState, SessionProjection, ToolCategory};
use desktop::transcript::{
    GroupPosition, LayoutDiff, LayoutRow, RunSummary, TranscriptLayout,
};
use desktop::ui::{Elapsed, single_line};
use serde_json::{Value, json};

/// Product metadata with a sequence and timestamp.
fn meta(sequence: u64, ts: u64) -> Value {
    json!({ "clawcode": { "sequence": sequence, "timestampMs": ts.to_string() } })
}

/// Applies one JSON update to the projection.
fn apply(projection: &mut SessionProjection, value: Value) {
    let update: wire::SessionUpdate =
        serde_json::from_value(value).expect("valid session update");
    projection.apply(&update);
}

/// Adds a user prompt.
fn user(projection: &mut SessionProjection, id: &str, seq: u64, ts: u64) {
    apply(
        projection,
        json!({ "sessionUpdate": "user_message", "messageId": id,
            "content": [{ "type": "text", "text": "go" }], "_meta": meta(seq, ts) }),
    );
}

/// Adds a final assistant message.
fn assistant(projection: &mut SessionProjection, id: &str, seq: u64, ts: u64) {
    apply(
        projection,
        json!({ "sessionUpdate": "agent_message", "messageId": id,
            "content": [{ "type": "text", "text": "done" }], "_meta": meta(seq, ts) }),
    );
}

/// Adds a tool call of the given kind.
fn tool(
    projection: &mut SessionProjection,
    id: &str,
    kind: &str,
    seq: u64,
    ts: u64,
) {
    apply(
        projection,
        json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "title": id,
            "kind": kind, "status": "completed", "_meta": meta(seq, ts) }),
    );
}

/// Sets the run state through a state update.
fn state(projection: &mut SessionProjection, name: &str) {
    apply(
        projection,
        json!({ "sessionUpdate": "state_update", "state": name }),
    );
}

/// Key of the message with this id.
fn msg(id: &str) -> RowKey {
    RowKey::Message(wire::MessageId::from(id.to_string()))
}

/// Builds a layout with nothing expanded.
fn layout(projection: &SessionProjection) -> TranscriptLayout {
    TranscriptLayout::build(projection, &HashSet::new())
}

/// Group positions of every row.
fn groups(layout: &TranscriptLayout) -> Vec<Option<GroupPosition>> {
    (0..layout.len())
        .filter_map(|ix| match layout.row(ix)? {
            LayoutRow::Row { group, .. } => Some(*group),
            LayoutRow::RunSummary { .. } | LayoutRow::Usage { .. } => None,
        })
        .collect()
}

/// A finished-or-running run: user, read, edit, edit, assistant.
fn standard_run(run_state: &str) -> SessionProjection {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    tool(&mut p, "a", "read", 2, 1_000);
    tool(&mut p, "b", "edit", 3, 2_000);
    tool(&mut p, "c", "edit", 4, 3_000);
    assistant(&mut p, "m1", 5, 88_000);
    state(&mut p, run_state);
    p
}

#[test]
fn consecutive_tools_form_one_group() {
    let p = standard_run("running");
    assert_eq!(
        groups(&layout(&p)),
        vec![
            None,
            Some(GroupPosition::First),
            Some(GroupPosition::Middle),
            Some(GroupPosition::Last),
            None
        ]
    );
    let mut lone = SessionProjection::default();
    user(&mut lone, "u1", 1, 0);
    tool(&mut lone, "a", "read", 2, 1);
    state(&mut lone, "running");
    assert_eq!(
        groups(&layout(&lone)),
        vec![None, Some(GroupPosition::Only)]
    );
}

#[test]
fn finished_run_folds_its_tools_into_one_summary() {
    let l = layout(&standard_run("idle"));
    assert_eq!(l.len(), 3);
    assert_eq!(
        l.row(1),
        Some(&LayoutRow::RunSummary {
            anchor: msg("u1"),
            summary: RunSummary {
                elapsed_ms: Some(88_000),
                counts: vec![(ToolCategory::Read, 1), (ToolCategory::Edit, 2)],
            },
            expanded: false,
        })
    );
    assert_eq!(
        l.row(1).expect("row").element_id().as_str(),
        "run:message:u1"
    );
}

#[test]
fn running_run_stays_expanded() {
    let l = layout(&standard_run("running"));
    assert_eq!(l.len(), 5);
}

#[test]
fn expanded_anchor_unfolds_a_finished_run() {
    let p = standard_run("idle");
    let expanded = HashSet::from([msg("u1")]);
    let l = TranscriptLayout::build(&p, &expanded);
    // The summary stays above the unfolded tools so the run can be folded again.
    assert_eq!(l.len(), 6);
    assert!(matches!(
        l.row(1),
        Some(LayoutRow::RunSummary { expanded: true, .. })
    ));
    assert_eq!(
        l.row(1).expect("row").element_id().as_str(),
        "run:message:u1"
    );
    // Summaries are left out of `groups`: u1, then the three tools as one box.
    assert_eq!(
        groups(&l)[1..4],
        [
            Some(GroupPosition::First),
            Some(GroupPosition::Middle),
            Some(GroupPosition::Last)
        ]
    );
}

#[test]
fn only_the_last_segment_can_be_live() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    tool(&mut p, "a", "read", 2, 1);
    user(&mut p, "u2", 3, 10);
    tool(&mut p, "b", "read", 4, 11);
    state(&mut p, "running");
    let l = layout(&p);
    assert!(matches!(l.row(1), Some(LayoutRow::RunSummary { .. })));
    assert!(matches!(l.row(3), Some(LayoutRow::Row { .. })));
    assert_eq!(l.len(), 4);
}

#[test]
fn empty_messages_are_skipped() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    apply(
        &mut p,
        json!({ "sessionUpdate": "agent_message", "messageId": "e",
            "content": [], "_meta": meta(2, 1) }),
    );
    assert_eq!(layout(&p).len(), 1);
    assert_eq!(layout(&p).position_of(&msg("e")), None);
}

#[test]
fn streaming_append_only_touches_the_tail() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    tool(&mut p, "a", "read", 2, 1);
    tool(&mut p, "b", "read", 3, 2);
    tool(&mut p, "c", "read", 4, 3);
    let chunk = |text: &str, seq| {
        json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": text }, "_meta": meta(seq, 4) })
    };
    apply(&mut p, chunk("a", 5));
    state(&mut p, "running");
    let a = layout(&p);
    assert_eq!(a.len(), 5);
    apply(&mut p, chunk("b", 5));
    let b = layout(&p);
    assert_eq!(
        a.diff(&b, &HashSet::from([msg("m1")])),
        LayoutDiff {
            splice: None,
            remeasure: vec![4]
        }
    );
    tool(&mut p, "d", "read", 6, 5);
    let c = layout(&p);
    assert_eq!(b.diff(&c, &HashSet::new()).splice, Some((5..5, 1)));
}

#[test]
fn content_diff_remeasures_only_displayed_updated_rows() {
    let p = standard_run("idle");
    let folded = layout(&p);
    // The run is folded: its tools are not displayed, its prompt is row 0.
    let diff = folded.content_diff(&HashSet::from([
        msg("u1"),
        RowKey::Tool(wire::ToolCallId::from("a".to_string())),
    ]));
    assert_eq!(
        diff,
        LayoutDiff {
            splice: None,
            remeasure: vec![0]
        }
    );
    assert_eq!(
        folded.content_diff(&HashSet::new()).remeasure,
        Vec::<usize>::new()
    );
}

#[test]
fn folding_replaces_the_tool_range() {
    let mut p = standard_run("running");
    let before = layout(&p);
    state(&mut p, "idle");
    let after = layout(&p);
    assert_eq!(before.diff(&after, &HashSet::new()).splice, Some((1..4, 1)));
    assert_eq!(p.run_state(), RunState::Idle);
}

#[test]
fn summary_and_elapsed_labels() {
    rust_i18n::set_locale("en");
    assert_eq!(Elapsed(14_000).label(), "14s");
    assert_eq!(Elapsed(88_000).label(), "1m 28s");
    assert_eq!(Elapsed(3_720_000).label(), "1h 2m");
    let summary = RunSummary {
        elapsed_ms: Some(88_000),
        counts: vec![
            (ToolCategory::Read, 3),
            (ToolCategory::Edit, 2),
            (ToolCategory::Execute, 1),
        ],
    };
    assert_eq!(summary.label(), "Worked 1m 28s · Read 3 · Edit 2 · Run 1");
    rust_i18n::set_locale("zh-CN");
    assert_eq!(
        summary.label(),
        "已工作 1 分 28 秒 · 读取 3 · 编辑 2 · 执行 1"
    );
    rust_i18n::set_locale("en");
}

/// Builds a layout from a user prompt followed by running tools `ids` (all read).
fn tools_layout(ids: &[&str]) -> (SessionProjection, TranscriptLayout) {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    for (n, id) in ids.iter().enumerate() {
        tool(&mut p, id, "read", 2 + n as u64, 1 + n as u64);
    }
    state(&mut p, "running");
    let l = layout(&p);
    (p, l)
}

#[test]
fn diff_covers_a_change_in_the_middle() {
    let (_, a) = tools_layout(&["a", "b", "c"]);
    let (_, b) = tools_layout(&["a", "x", "c"]);
    // The old tool b was replaced by x; the group labels stay the same.
    assert_eq!(a.diff(&b, &HashSet::new()).splice, Some((2..3, 1)));
}

#[test]
fn diff_covers_an_insert_at_the_front() {
    let (_, a) = tools_layout(&["a", "b"]);
    let (_, b) = tools_layout(&["z", "a", "b"]);
    // z is new and a turns First -> Middle; the unchanged tail row b is kept.
    assert_eq!(a.diff(&b, &HashSet::new()).splice, Some((1..2, 2)));
}

#[test]
fn diff_covers_a_removed_row() {
    let (_, a) = tools_layout(&["a", "b", "c"]);
    let (_, b) = tools_layout(&["a", "c"]);
    assert_eq!(a.diff(&b, &HashSet::new()).splice, Some((2..3, 0)));
    assert_eq!(a.diff(&a, &HashSet::new()).splice, None);
}

#[test]
fn diff_remeasures_rows_in_the_suffix_region() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    assistant(&mut p, "m1", 9, 10);
    let a = layout(&p);
    // An earlier-sequenced reply lands between the prompt and `m1`.
    assistant(&mut p, "m0", 5, 5);
    let b = layout(&p);
    let diff = a.diff(&b, &HashSet::from([msg("m1"), msg("u1")]));
    assert_eq!(diff.splice, Some((1..1, 1)));
    // `u1` sits in the prefix (index 0), `m1` in the suffix (index 2).
    assert_eq!(diff.remeasure, vec![0, 2]);
}

#[test]
fn single_line_keeps_the_first_line_and_marks_the_rest() {
    assert_eq!(single_line("cat notes.md"), "cat notes.md");
    assert_eq!(
        single_line("\n  cat notes.md\nsleep 20\nls -la\n"),
        "cat notes.md \u{2026}"
    );
    // Trailing blank lines are not "more text".
    assert_eq!(single_line("ls -la\n\n"), "ls -la");
    assert_eq!(single_line(""), "");
}

#[test]
fn steering_mid_run_keeps_the_whole_run_live() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 1_000);
    tool(&mut p, "a", "read", 2, 1_100);
    // A promoted steering message arrives as another user prompt mid-run.
    user(&mut p, "u2", 3, 2_000);
    tool(&mut p, "b", "read", 4, 2_100);
    apply(
        &mut p,
        json!({ "sessionUpdate": "state_update", "state": "running",
            "_meta": meta(5, 1_000) }),
    );
    assert_eq!(p.run_started_ms(), Some(1_000));
    let l = layout(&p);
    assert!(
        (0..l.len())
            .all(|ix| !matches!(l.row(ix), Some(LayoutRow::RunSummary { .. }))),
        "no part of the running run folds"
    );
    assert_eq!(l.len(), 4);
}

#[test]
fn user_bash_message_is_hidden_behind_its_tool_row() {
    let mut p = SessionProjection::default();
    apply(
        &mut p,
        json!({ "sessionUpdate": "user_message", "messageId": "b1",
            "content": [{ "type": "text", "text": "!ls" }],
            "_meta": { "clawcode": { "sequence": 1, "timestampMs": "0", "origin": "user_bash" } } }),
    );
    // Without its tool row the command still shows as the message.
    assert_eq!(layout(&p).position_of(&msg("b1")), Some(0));
    tool(&mut p, "b1", "execute", 2, 1);
    let l = layout(&p);
    assert_eq!(l.position_of(&msg("b1")), None);
    assert_eq!(l.len(), 1);
}

/// Adds an assistant message that reports its model and usage.
fn assistant_with_usage(
    projection: &mut SessionProjection,
    id: &str,
    seq: u64,
    ts: u64,
    input: u64,
) {
    apply(
        projection,
        json!({ "sessionUpdate": "agent_message", "messageId": id,
            "content": [{ "type": "text", "text": "done" }],
            "_meta": { "clawcode": {
                "sequence": seq, "timestampMs": ts.to_string(),
                "assistant": { "provider_id": "p", "model_id": "m",
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": input.to_string(), "output_tokens": "2" } }
            } } }),
    );
}

/// Usage rows of a layout, as (anchor, input tokens).
fn usage_rows(layout: &TranscriptLayout) -> Vec<(RowKey, Option<u64>)> {
    (0..layout.len())
        .filter_map(|ix| match layout.row(ix)? {
            LayoutRow::Usage { anchor, usage } => {
                Some((anchor.clone(), usage.usage.input))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn finished_run_gets_one_usage_row() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    assistant_with_usage(&mut p, "a1", 2, 1_000, 10);
    assistant_with_usage(&mut p, "a2", 3, 3_000, 5);
    state(&mut p, "idle");

    let l = layout(&p);

    assert_eq!(usage_rows(&l), [(msg("a2"), Some(15))]);
    let last = l.row(l.len() - 1).expect("last row");
    assert_eq!(last.element_id(), "usage:message:a2");
    let LayoutRow::Usage { usage, .. } = last else {
        panic!("usage row last");
    };
    assert_eq!(usage.model, Some(("p".to_string(), "m".to_string())));
    assert_eq!(usage.usage.output, Some(4));
}

#[test]
fn running_or_usage_less_runs_have_no_usage_row() {
    let mut running = SessionProjection::default();
    user(&mut running, "u1", 1, 0);
    assistant_with_usage(&mut running, "a1", 2, 1_000, 10);
    state(&mut running, "running");
    let mut silent = SessionProjection::default();
    user(&mut silent, "u1", 1, 0);
    assistant(&mut silent, "a1", 2, 1_000);
    state(&mut silent, "idle");

    assert!(usage_rows(&layout(&running)).is_empty());
    assert!(usage_rows(&layout(&silent)).is_empty());
}

/// Adds an assistant step that only thought before calling its tools.
fn thought(projection: &mut SessionProjection, id: &str, seq: u64, ts: u64) {
    apply(
        projection,
        json!({ "sessionUpdate": "agent_thought", "messageId": id,
            "content": [{ "type": "text", "text": "hmm" }], "_meta": meta(seq, ts) }),
    );
}

/// A folded run takes its thought only steps along.
#[test]
fn a_folded_run_takes_its_thought_only_steps_along() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    thought(&mut p, "t1", 2, 500);
    tool(&mut p, "a", "read", 3, 1_000);
    thought(&mut p, "t2", 4, 1_500);
    tool(&mut p, "b", "edit", 5, 2_000);
    assistant(&mut p, "m1", 6, 3_000);
    state(&mut p, "idle");

    // Folded: the prompt, the summary, the answer, and its usage at most.
    let folded = layout(&p);
    let shown: Vec<_> = (0..folded.len())
        .filter_map(|ix| match folded.row(ix)? {
            LayoutRow::Row { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![msg("u1"), msg("m1")]);

    // Unfolded, each thought stays beside the tools it led to.
    let open = TranscriptLayout::build(&p, &HashSet::from([msg("u1")]));
    let shown: Vec<_> = (0..open.len())
        .filter_map(|ix| match open.row(ix)? {
            LayoutRow::Row { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(shown.len(), 6);
}

/// A run whose steps all fold away as thinking still reports its usage,
/// after the summary that stands for them.
#[test]
fn a_run_of_thought_only_steps_keeps_its_usage() {
    let mut p = SessionProjection::default();
    user(&mut p, "u1", 1, 0);
    thought(&mut p, "t1", 2, 500);
    // The step's message carries no text, only its usage.
    apply(
        &mut p,
        json!({ "sessionUpdate": "agent_message", "messageId": "t1",
            "content": [],
            "_meta": { "clawcode": {
                "sequence": 2, "timestampMs": "500",
                "assistant": { "provider_id": "p", "model_id": "m",
                    "stop_reason": "tool_use",
                    "usage": { "input_tokens": "10", "output_tokens": "2" } }
            } } }),
    );
    tool(&mut p, "a", "read", 3, 1_000);
    state(&mut p, "idle");

    let l = layout(&p);

    assert_eq!(usage_rows(&l), [(msg("t1"), Some(10))]);
    assert!(matches!(l.row(l.len() - 1), Some(LayoutRow::Usage { .. })));
}

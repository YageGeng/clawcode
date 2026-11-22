//! The desktop trajectory model: page merging, rows, folding, filters,
//! selection, in-flight rows, and the summary.

mod support;

use agent_client_protocol::v2::schema as wire;
use desktop::projection::{RunState, SessionProjection};
use desktop::trajectory::{
    InFlight, LedgerRow, PageApplied, RecordText as _, RunKey, TimeInterval,
    TrajectoryModel,
};
use protocol::{
    ModelUsage, RunId, TrajectoryCursor, TrajectoryRecordKind, UserRecord,
};
use support::trajectory::{assistant, page, record, request, run, tool, user};

/// The ids of the record rows.
fn record_ids(rows: &[LedgerRow]) -> Vec<&str> {
    rows.iter()
        .filter_map(|row| match row {
            LedgerRow::Record { id } => Some(id.as_str()),
            _ => None,
        })
        .collect()
}

/// Two Runs: the first with requests 1 (three tools) and 2, the second with request 3.
fn two_runs() -> TrajectoryModel {
    let one = || run("run-1", 1);
    let two = || run("run-2", 2);
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(
            vec![
                user("u1", 1, one()),
                request(1, 2, one()),
                assistant(1, 2, one(), (100, 0)),
                tool("c1", 2, one(), Some("one")),
                tool("c2", 2, one(), Some("two")),
                tool("c3", 2, one(), Some("needle here")),
                request(2, 6, one()),
                assistant(2, 6, one(), (100, 0)),
                user("u2", 7, two()),
                request(3, 8, two()),
                assistant(3, 8, two(), (100, 0)),
            ],
            "leaf",
            false,
        ),
    );
    model
}

/// Tail then before then after merges in order.
#[test]
fn tail_then_before_then_after_merges_in_order() {
    let mut model = TrajectoryModel::default();
    let applied = model.apply(
        TrajectoryCursor::Tail,
        page(vec![user("u3", 3, None), user("u4", 4, None)], "leaf", true),
    );
    assert_eq!(applied, PageApplied::Replaced);
    let applied = model.apply(
        TrajectoryCursor::Before(3),
        page(
            vec![user("u1", 1, None), user("u2", 2, None)],
            "leaf",
            false,
        ),
    );
    assert_eq!(applied, PageApplied::Prepended);
    let applied = model.apply(
        TrajectoryCursor::After(4),
        page(vec![user("u5", 5, None)], "leaf", false),
    );
    assert_eq!(applied, PageApplied::Appended);

    let sequences: Vec<u64> = model
        .records()
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(sequences, [1, 2, 3, 4, 5]);
    assert!(!model.has_more_before());
    assert_eq!(model.first_sequence(), Some(1));
    assert_eq!(model.last_sequence(), Some(5));
}

/// A changed branch replaces every record.
#[test]
fn a_changed_branch_replaces_every_record() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(vec![user("u1", 1, None), user("u2", 2, None)], "old", false),
    );
    model.select("u1");
    let mut other =
        page(vec![user("u1", 1, None), user("u9", 9, None)], "new", false);
    other.branch_changed = true;
    let applied = model.apply(TrajectoryCursor::After(2), other);
    assert_eq!(applied, PageApplied::Replaced);
    assert_eq!(
        model
            .records()
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["u1", "u9"]
    );
    assert_eq!(model.selected_record().map(|r| r.id.as_str()), Some("u1"));

    model.select("u9");
    let mut other = page(vec![user("u7", 7, None)], "other", false);
    other.branch_changed = true;
    model.apply(TrajectoryCursor::After(9), other);
    assert_eq!(model.selected_record(), None);
}

/// Appending with a new leaf keeps the loaded records.
#[test]
fn appending_with_a_new_leaf_keeps_the_loaded_records() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(vec![user("u1", 1, None)], "leaf-1", false),
    );
    let applied = model.apply(
        TrajectoryCursor::After(1),
        page(vec![user("u2", 2, None)], "leaf-2", false),
    );
    assert_eq!(applied, PageApplied::Appended);
    assert_eq!(model.records().len(), 2);
    assert_eq!(model.leaf_id().map(|leaf| leaf.as_str()), Some("leaf-2"));
}

/// A later tool record replaces the pending one.
#[test]
fn a_later_tool_record_replaces_the_pending_one() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(vec![tool("c1", 3, None, None)], "leaf", false),
    );
    model.apply(
        TrajectoryCursor::After(3),
        page(vec![tool("c1", 3, None, Some("done"))], "leaf", false),
    );
    assert_eq!(model.records().len(), 1);
    let TrajectoryRecordKind::Tool(tool) = &model.records()[0].kind else {
        panic!("tool record expected");
    };
    assert_eq!(tool.result_text.as_deref(), Some("done"));
}

/// Rows group runs and fold calls.
#[test]
fn rows_group_runs_and_fold_calls() {
    let mut model = two_runs();
    let headers = |rows: &[LedgerRow]| {
        rows.iter()
            .filter(|row| matches!(row, LedgerRow::RunHeader { .. }))
            .count()
    };
    let rows = model.rows(&[]);
    assert_eq!(headers(&rows), 2);
    assert!(matches!(
        rows[0],
        LedgerRow::RunHeader {
            index: Some(1),
            collapsed: false,
            ..
        }
    ));

    model.set_calls_folded(true);
    let rows = model.rows(&[]);
    let groups: Vec<&LedgerRow> = rows
        .iter()
        .filter(|row| matches!(row, LedgerRow::CallGroup { .. }))
        .collect();
    assert_eq!(groups.len(), 1);
    assert!(matches!(
        groups[0],
        LedgerRow::CallGroup {
            request_index: 1,
            count: 3,
            failures: 0,
            ..
        }
    ));
    model.toggle_call_group(1);
    assert!(record_ids(&model.rows(&[])).contains(&"tool:c2"));

    let first = RunKey::Run(RunId::try_from("run-1").expect("run"));
    model.toggle_run(first.clone());
    let rows = model.rows(&[]);
    assert!(matches!(
        &rows[0],
        LedgerRow::RunHeader { key, collapsed: true, .. } if *key == first
    ));
    assert!(matches!(
        rows[1],
        LedgerRow::RunHeader { index: Some(2), .. }
    ));
    model.set_all_runs_collapsed(true);
    assert_eq!(model.rows(&[]).len(), 2);
}

/// Interval keeps overlapping records.
#[test]
fn interval_keeps_overlapping_records() {
    let mut model = TrajectoryModel::default();
    let spanning = |id: &str, start: u64| {
        record(
            id,
            start,
            None,
            (start, Some(start + 10)),
            TrajectoryRecordKind::User(
                UserRecord::builder().text(id.into()).build(),
            ),
        )
    };
    model.apply(
        TrajectoryCursor::Tail,
        page(
            vec![spanning("a", 0), spanning("b", 20), spanning("c", 40)],
            "leaf",
            false,
        ),
    );
    model.set_interval(Some(TimeInterval {
        start_ms: 25,
        end_ms: 45,
    }));
    let rows = model.rows(&[]);
    assert!(matches!(rows[0], LedgerRow::RunHeader { index: None, .. }));
    assert_eq!(record_ids(&rows), ["b", "c"]);
}

/// Selection survives prepending.
#[test]
fn selection_survives_prepending() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(
            vec![
                user("u3", 3, run("run-2", 2)),
                user("u4", 4, run("run-2", 2)),
            ],
            "leaf",
            true,
        ),
    );
    model.select("u3");
    model.apply(
        TrajectoryCursor::Before(3),
        page(vec![user("u1", 1, run("run-1", 1))], "leaf", false),
    );
    assert_eq!(model.selected_record().map(|r| r.id.as_str()), Some("u3"));

    let rows = model.rows(&[]);
    let next = model.select_adjacent(&rows, 1).map(|r| r.id.clone());
    assert_eq!(next.as_deref(), Some("u4"));
    // Moving back from u3 crosses the second Run's header to u1.
    model.select("u3");
    let previous = model.select_adjacent(&rows, -1).map(|r| r.id.clone());
    assert_eq!(previous.as_deref(), Some("u1"));
}

/// In flight follows the projection.
#[test]
fn in_flight_follows_the_projection() {
    let mut projection = SessionProjection::default();
    assert!(InFlight::from_projection(&projection, Some(5_000)).is_empty());

    let running: wire::SessionUpdate =
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "state_update", "state": "running",
            "_meta": { "clawcode": { "timestampMs": "1000" } }
        }))
        .expect("update");
    projection.apply(&running);
    assert_eq!(projection.run_state(), RunState::Running);
    assert_eq!(
        InFlight::from_projection(&projection, Some(5_000)),
        [InFlight::Request {
            started_at_ms: 5_000
        }]
    );
    assert_eq!(
        InFlight::from_projection(&projection, Some(500)),
        [InFlight::Request {
            started_at_ms: 1_000
        }]
    );
}

/// Summary counts cache hits.
#[test]
fn summary_counts_cache_hits() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(
            vec![
                assistant(1, 1, run("run-1", 1), (1000, 850)),
                assistant(2, 2, run("run-1", 1), (1000, 650)),
            ],
            "leaf",
            false,
        ),
    );
    let summary = model.summary();
    assert_eq!(summary.runs, 1);
    assert_eq!(summary.requests, 2);
    assert_eq!(summary.input, 2000);
    assert_eq!(summary.output, 20);
    assert_eq!(summary.cache_hit_percent, Some(75));
}

/// Record text is one bounded line.
#[test]
fn record_text_is_one_bounded_line() {
    let long = record(
        "u",
        1,
        None,
        (1, None),
        TrajectoryRecordKind::User(
            UserRecord::builder()
                .text(format!("first line\nsecond {}", "x".repeat(400)))
                .build(),
        ),
    );
    let summary = long.summary();
    assert!(!summary.contains('\n'));
    assert!(summary.chars().count() <= 200);
    assert!(long.text().contains("second"));

    let word = record(
        "w",
        1,
        None,
        (1, None),
        TrajectoryRecordKind::User(
            UserRecord::builder().text("x".repeat(400)).build(),
        ),
    );
    assert_eq!(word.summary().chars().count(), 200);
    assert!(word.summary().ends_with('…'));

    let call = tool("c1", 1, None, Some("fn main() {}"));
    assert_eq!(call.summary(), r#"read {"path":"a.rs"} → fn main() {}"#);
    assert!(call.text().contains("fn main() {}"));
}

/// A notification during a read reads again.
#[test]
fn a_notification_during_a_read_reads_again() {
    use desktop::trajectory::RefreshGate;
    let mut gate = RefreshGate::default();
    assert!(gate.request(), "the first notification schedules a read");
    assert!(!gate.request(), "a burst shares the scheduled read");
    gate.start();
    assert!(!gate.request(), "a read is running");
    assert!(
        gate.finish(),
        "the notification during the read reads again"
    );
    assert!(gate.request());
    gate.start();
    assert!(!gate.finish(), "nothing arrived during this read");
}

/// Usage through a request adds the earlier ones.
#[test]
fn usage_through_a_request_adds_the_earlier_ones() {
    let mut model = TrajectoryModel::default();
    let mut first = page(
        vec![
            request(2, 2, None),
            assistant(2, 2, None, (100, 0)),
            request(3, 3, None),
            assistant(3, 3, None, (100, 0)),
        ],
        "leaf",
        true,
    );
    first.usage_before =
        ModelUsage::from(protocol::Usage::new().input_tokens(50));
    model.apply(TrajectoryCursor::Tail, first);

    let usage = |index| model.usage_through(index).as_ref().input_tokens;
    assert_eq!(usage(2), Some(150));
    assert_eq!(usage(3), Some(250));
    assert_eq!(model.record("a3").map(|r| r.sequence), Some(3));
    assert_eq!(model.previous_request(3).map(|r| r.index), Some(2));
    assert_eq!(model.previous_request(2), None);
}

/// A loaded call that is still executing shows once: as its record, not
/// again as an in-flight row; calls not loaded yet keep their row.
#[test]
fn a_loaded_call_replaces_its_in_flight_row() {
    let mut model = TrajectoryModel::default();
    model.apply(
        TrajectoryCursor::Tail,
        page(vec![tool("c1", 1, run("run-1", 1), None)], "leaf", false),
    );
    let executing = |call: &str| InFlight::Tool {
        call_id: call.into(),
        name: "read".into(),
        started_at_ms: 5,
    };
    let rows = model.rows(&[executing("c1"), executing("c2")]);
    let in_flight: Vec<_> = rows
        .iter()
        .filter_map(|row| match row {
            LedgerRow::InFlight(InFlight::Tool { call_id, .. }) => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(in_flight, ["c2"]);
    assert_eq!(record_ids(&rows), ["tool:c1"]);
}

/// A line filled exactly before more words still ends within 200
/// characters, its last character giving way to the ellipsis.
#[test]
fn a_full_summary_line_keeps_room_for_its_ellipsis() {
    let full = record(
        "f",
        1,
        None,
        (1, None),
        TrajectoryRecordKind::User(
            UserRecord::builder()
                .text(format!("{} more", "x".repeat(200)))
                .build(),
        ),
    );
    let summary = full.summary();
    assert_eq!(summary.chars().count(), 200);
    assert!(summary.ends_with('…'));
}

/// With no Run loaded, nothing counts as all collapsed.
#[test]
fn no_runs_are_not_all_collapsed() {
    assert!(!TrajectoryModel::default().all_runs_collapsed());
}

/// Choosing something that is not a loaded record, such as a running
/// request's bar, keeps the selection.
#[test]
fn selecting_an_unloaded_id_keeps_the_selection() {
    let mut model = two_runs();
    model.select("a1");
    model.select("in-flight:0");
    assert_eq!(
        model.selected_record().map(|record| record.id.as_str()),
        Some("a1")
    );
}

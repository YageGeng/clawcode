use std::convert::TryFrom;

use protocol::{
    RunId, StopReason, TimestampMs, TurnId, TurnIdentity, TurnOutcome,
    TurnRecord, TurnTiming, TurnTimingError,
};

/// A persisted turn carries its run identity and exact complete interval.
#[test]
fn turn_record_serializes_complete_identity_and_timing() {
    let record = TurnRecord {
        identity: TurnIdentity {
            run_id: RunId::try_from("run-1").expect("valid run id"),
            turn_id: TurnId::try_from("turn-1").expect("valid turn id"),
        },
        timing: TurnTiming::try_from((
            TimestampMs::try_from("100").expect("valid timestamp"),
            TimestampMs::try_from("250").expect("valid timestamp"),
        ))
        .expect("ordered turn timing"),
        outcome: TurnOutcome::Completed {
            stop_reason: StopReason::ToolUse,
        },
    };

    let value = serde_json::to_value(record).expect("turn should serialize");

    assert_eq!(value["run_id"], "run-1");
    assert_eq!(value["turn_id"], "turn-1");
    assert_eq!(value["started_at_ms"], "100");
    assert_eq!(value["ended_at_ms"], "250");
    assert_eq!(value["outcome"]["type"], "completed");
    assert_eq!(value["outcome"]["stop_reason"], "tool_use");
}

/// Turn timing rejects an interval whose end precedes its start.
#[test]
fn turn_timing_rejects_reversed_intervals() {
    let started = TimestampMs::try_from("251").expect("valid timestamp");
    let ended = TimestampMs::try_from("250").expect("valid timestamp");

    assert_eq!(
        TurnTiming::try_from((started, ended)),
        Err(TurnTimingError::EndBeforeStart { started, ended })
    );
}

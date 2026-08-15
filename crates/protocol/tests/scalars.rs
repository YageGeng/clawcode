use std::convert::TryFrom;

use protocol::{
    EntryId, IdKind, LaneId, MessageId, QueueId, RecordId, RunId, ScalarError,
    Sequence, SessionId, TimestampMs, ToolCallId, TraceId, TurnId,
};

/// A timestamp larger than JavaScript's safe integer limit remains exact on the wire.
#[test]
fn timestamp_roundtrips_as_a_decimal_string() {
    let timestamp = TimestampMs::try_from("9007199254740993")
        .expect("a u64 timestamp should parse");

    let encoded =
        serde_json::to_string(&timestamp).expect("timestamp should serialize");
    let decoded: TimestampMs =
        serde_json::from_str(&encoded).expect("timestamp should deserialize");

    assert_eq!(encoded, "\"9007199254740993\"");
    assert_eq!(decoded, timestamp);
}

/// Timestamp parsing rejects signs, whitespace, and non-decimal input.
#[test]
fn timestamp_rejects_non_decimal_strings() {
    for invalid in ["", "-1", "+1", " 1", "1.0", "now"] {
        assert!(
            TimestampMs::try_from(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
}

/// Numeric clock values become string-serialized timestamps without truncation.
#[test]
fn timestamp_accepts_unsigned_clock_values() {
    let timestamp = TimestampMs::from(9_007_199_254_740_993_u64);

    assert_eq!(timestamp.get(), 9_007_199_254_740_993_u64);
    assert_eq!(
        serde_json::to_string(&timestamp).expect("timestamp should serialize"),
        "\"9007199254740993\""
    );
}

/// Event sequences serialize as numbers while remaining JavaScript-safe.
#[test]
fn sequence_enforces_the_javascript_safe_integer_range() {
    let largest = Sequence::try_from(9_007_199_254_740_991_u64)
        .expect("the largest safe integer should be accepted");

    assert_eq!(
        serde_json::to_string(&largest).expect("sequence should serialize"),
        "9007199254740991"
    );
    assert_eq!(Sequence::try_from(0_u64), Err(ScalarError::InvalidSequence));
    assert_eq!(
        Sequence::try_from(9_007_199_254_740_992_u64),
        Err(ScalarError::InvalidSequence)
    );
}

/// Domain identifiers reject empty values and keep a string-only wire shape.
#[test]
fn domain_identifiers_are_non_empty_string_newtypes() {
    assert_eq!(
        SessionId::try_from(""),
        Err(ScalarError::EmptyIdentifier { kind: "session" })
    );
    assert_eq!(
        TurnId::try_from("   "),
        Err(ScalarError::EmptyIdentifier { kind: "turn" })
    );

    let cases = [
        serde_json::to_string(
            &SessionId::try_from("session-1").expect("valid session id"),
        ),
        serde_json::to_string(
            &TurnId::try_from("turn-1").expect("valid turn id"),
        ),
        serde_json::to_string(
            &MessageId::try_from("message-1").expect("valid message id"),
        ),
        serde_json::to_string(
            &ToolCallId::try_from("call-1").expect("valid tool call id"),
        ),
        serde_json::to_string(
            &EntryId::try_from("entry-1").expect("valid entry id"),
        ),
        serde_json::to_string(&RunId::try_from("run-1").expect("valid run id")),
        serde_json::to_string(
            &LaneId::try_from("main").expect("valid lane id"),
        ),
        serde_json::to_string(
            &RecordId::try_from("record-1").expect("valid record id"),
        ),
        serde_json::to_string(
            &QueueId::try_from("queue-1").expect("valid queue id"),
        ),
        serde_json::to_string(
            &TraceId::try_from("trace-1").expect("valid trace id"),
        ),
    ];

    for encoded in cases {
        assert!(
            encoded
                .expect("identifier should serialize")
                .starts_with('"')
        );
    }
}

/// Trace identifiers use the shared string scalar contract and remain readable in logs.
#[test]
fn trace_identifier_is_validated_and_displayable() {
    assert_eq!(
        TraceId::try_from("  "),
        Err(ScalarError::EmptyIdentifier { kind: "trace" })
    );

    let trace_id =
        TraceId::try_from("trace-operation-1").expect("valid trace identifier");
    assert_eq!(trace_id.to_string(), "trace-operation-1");
    assert_eq!(IdKind::Trace.prefix(), "trace");
}

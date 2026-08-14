use protocol::{
    AgentMessage, CompactionData, CompactionDetails, ContentBlock,
    MessageContent, MessageId, MessageIdentity, MessageTiming, QueueId,
    QueueKind, QueuedMessage, SessionTitle, TimestampMs, TurnId,
};

/// Builds one complete user message with hand-controlled precision-sensitive fields.
fn sample_user_message(
    turn: &str,
    message: &str,
    timestamp: u64,
) -> AgentMessage {
    let timestamp_ms = TimestampMs::from(timestamp);
    AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from(message).expect("message id"),
            turn_id: TurnId::try_from(turn).expect("turn id"),
        },
        timing: MessageTiming::try_from((
            timestamp_ms,
            timestamp_ms,
            timestamp_ms,
        ))
        .expect("message timing"),
        content: MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: "queued".to_string(),
            }],
        },
    }
}

/// Queue records preserve stable IDs and string millisecond timestamps on the wire.
#[test]
fn queue_id_and_capabilities_keep_precision_safe_wire_values() {
    let queued = QueuedMessage::builder()
        .queue_id(QueueId::try_from("queue-1").expect("queue id"))
        .kind(QueueKind::FollowUp)
        .message(sample_user_message(
            "turn-1",
            "message-1",
            9_007_199_254_740_999,
        ))
        .build();

    let value = serde_json::to_value(queued).expect("serialize queue item");
    assert_eq!(value["queueId"], "queue-1");
    assert_eq!(value["message"]["turn_id"], "turn-1");
    assert_eq!(value["message"]["timestamp_ms"], "9007199254740999");
}

/// Compaction payloads use the exact pi v4 field names and retain opaque details.
#[test]
fn compaction_data_uses_pi_v4_wire_fields() {
    let data = CompactionData::builder()
        .summary("summary".to_string())
        .retained_tail(vec![sample_user_message("turn-2", "message-2", 42)])
        .tokens_before(123)
        .details(Some(
            CompactionDetails::builder()
                .reason(protocol::CompactionReason::Manual)
                .turn_id(TurnId::try_from("turn-compact").expect("turn id"))
                .started_at_ms(TimestampMs::from(10))
                .ended_at_ms(TimestampMs::from(20))
                .build(),
        ))
        .build();

    let value = serde_json::to_value(data).expect("serialize compaction");
    assert_eq!(value["summary"], "summary");
    assert!(value["retainedTail"].is_array());
    assert_eq!(value["tokensBefore"], 123);
    assert_eq!(value["details"]["turnId"], "turn-compact");
    assert_eq!(value["details"]["startedAtMs"], "10");
    assert_eq!(value["details"]["endedAtMs"], "20");
    assert_eq!(value["details"]["reason"], "manual");
    assert!(value.get("summaryMessage").is_none());
}

/// Session titles are normalized once and reject empty or oversized values.
#[test]
fn session_title_trims_and_enforces_the_public_limit() {
    let title = SessionTitle::try_from("  Design agent  ".to_string())
        .expect("valid title");
    assert_eq!(title.as_str(), "Design agent");
    assert_eq!(
        serde_json::to_value(title).expect("serialize title"),
        "Design agent"
    );
    SessionTitle::try_from("   ".to_string()).expect_err("empty title");
    SessionTitle::try_from("界".repeat(121)).expect_err("long title");
}

/// Optional compaction details are omitted when no producer metadata exists.
#[test]
fn empty_details_are_omitted_from_compaction_payloads() {
    let data = CompactionData::builder()
        .summary("summary".to_string())
        .retained_tail(Vec::new())
        .tokens_before(0)
        .details(None)
        .build();

    let value = serde_json::to_value(data).expect("serialize compaction");
    assert!(value.get("details").is_none());
}

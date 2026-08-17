use std::convert::TryFrom;

use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, CompactionOutcome,
    CompactionReason, CompactionResult, CompactionSummaryMessage, ContentBlock,
    EntryId, EventMetadata, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelUsage, RunId, Sequence, SlashCommandAliasKind,
    SlashCommandDefinition, SlashCommandSource, TimestampMs, ToolCall,
    ToolCallId, ToolResult, TurnId,
};

/// Every streamed event carries an exact turn id, timestamp, and ordered sequence.
#[test]
fn streamed_event_flattens_required_metadata() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-1").expect("valid turn id"),
            timestamp_ms: TimestampMs::try_from("9007199254740993")
                .expect("valid timestamp"),
            sequence: Sequence::try_from(7_u64).expect("valid sequence"),
        },
        payload: AgentEventPayload::MessageTextDelta {
            message_id: MessageId::try_from("message-1")
                .expect("valid message id"),
            delta: "hello".to_string(),
        },
    };

    let value = serde_json::to_value(event).expect("event should serialize");

    assert_eq!(value["turn_id"], "turn-1");
    assert_eq!(value["timestamp_ms"], "9007199254740993");
    assert_eq!(value["sequence"], 7);
    assert_eq!(value["event"], "message_text_delta");
    assert_eq!(value["message_id"], "message-1");
    assert_eq!(value["delta"], "hello");
}

/// Lifecycle events keep typed data while sharing mandatory event metadata.
#[test]
fn lifecycle_events_share_turn_and_timestamp_metadata() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-2").expect("valid turn id"),
            timestamp_ms: TimestampMs::from(1_725_000_000_999),
            sequence: Sequence::try_from(8_u64).expect("valid sequence"),
        },
        payload: AgentEventPayload::ToolExecutionStart {
            run_id: RunId::try_from("run-1").expect("valid run id"),
            call: ToolCall {
                tool_call_id: ToolCallId::try_from("call-1")
                    .expect("valid tool call id"),
                name: "read".to_string(),
                arguments: serde_json::json!({ "path": "README.md" }),
            },
        },
    };

    let value = serde_json::to_value(event).expect("event should serialize");
    assert_eq!(value["turn_id"], "turn-2");
    assert_eq!(value["timestamp_ms"], "1725000000999");
    assert_eq!(value["event"], "tool_execution_start");
    assert_eq!(value["call"]["name"], "read");
}

/// Partial tool output is a first-class correlated event before final completion.
#[test]
fn tool_execution_update_serializes_partial_result() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-tool").expect("turn id"),
            timestamp_ms: TimestampMs::from(1_725_000_001_000),
            sequence: Sequence::try_from(9_u64).expect("sequence"),
        },
        payload: AgentEventPayload::ToolExecutionUpdate {
            run_id: RunId::try_from("run-tool").expect("run id"),
            result: ToolResult::builder()
                .tool_call_id(
                    ToolCallId::try_from("call-tool").expect("call id"),
                )
                .blocks(vec![ContentBlock::Text {
                    text: "partial".to_string(),
                }])
                .is_error(false)
                .build(),
        },
    };

    let value = serde_json::to_value(event).expect("serialize update");
    assert_eq!(value["event"], "tool_execution_update");
    assert_eq!(value["result"]["tool_call_id"], "call-tool");
    assert_eq!(value["result"]["blocks"][0]["text"], "partial");
}

/// Available Commands use the standard event envelope and preserve typed metadata.
#[test]
fn available_commands_changed_round_trips() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-commands").expect("turn id"),
            timestamp_ms: TimestampMs::from(1_725_000_001_001),
            sequence: Sequence::try_from(10_u64).expect("sequence"),
        },
        payload: AgentEventPayload::AvailableCommandsChanged {
            commands: vec![
                SlashCommandDefinition::builder()
                    .name("review".to_string())
                    .description("Review changes".to_string())
                    .argument_hint(Some("<path>".to_string()))
                    .source(SlashCommandSource::PromptTemplate)
                    .qualified_name(None)
                    .alias_kind(SlashCommandAliasKind::Canonical)
                    .build(),
            ],
        },
    };

    let value = serde_json::to_value(&event).expect("serialize Commands event");
    assert_eq!(value["event"], "available_commands_changed");
    assert_eq!(value["commands"][0]["argument_hint"], "<path>");
    let decoded: AgentEvent =
        serde_json::from_value(value).expect("decode Commands event");
    assert_eq!(decoded, event);
}

/// Compaction summaries remain semantically distinct and keep exact token strings.
#[test]
fn compaction_summary_message_round_trips_as_typed_content() {
    let timestamp = TimestampMs::from(1_725_000_001_002);
    let message = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-compaction")
                .expect("message id"),
            turn_id: TurnId::try_from("turn-compaction").expect("turn id"),
        },
        timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
            .expect("message timing"),
        content: MessageContent::CompactionSummary {
            compaction: CompactionSummaryMessage::builder()
                .entry_id(
                    EntryId::try_from("entry-compaction").expect("entry id"),
                )
                .summary("structured summary".to_string())
                .tokens_before(9_007_199_254_740_993)
                .build(),
        },
    };

    let value = serde_json::to_value(&message).expect("serialize summary");
    assert_eq!(value["content"]["type"], "compaction_summary");
    assert_eq!(value["content"]["entryId"], "entry-compaction");
    assert_eq!(value["content"]["summary"], "structured summary");
    assert_eq!(value["content"]["tokensBefore"], "9007199254740993");
    let decoded: AgentMessage =
        serde_json::from_value(value).expect("decode summary");
    assert_eq!(decoded, message);
}

/// Compaction terminal events distinguish success, failure, and cancellation.
#[test]
fn compaction_end_round_trips_typed_outcomes() {
    let usage = ModelUsage::builder()
        .input_tokens(100)
        .output_tokens(20)
        .cache_read_tokens(5)
        .cache_write_tokens(0)
        .total_tokens(125)
        .build();
    let completed = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-compaction").expect("turn id"),
            timestamp_ms: TimestampMs::from(1_725_000_001_004),
            sequence: Sequence::try_from(11_u64).expect("sequence"),
        },
        payload: AgentEventPayload::CompactionEnd {
            run_id: RunId::try_from("run-compaction").expect("run id"),
            reason: CompactionReason::Manual,
            outcome: CompactionOutcome::Completed {
                result: CompactionResult::builder()
                    .entry_id(
                        EntryId::try_from("entry-compaction")
                            .expect("entry id"),
                    )
                    .turn_id(
                        TurnId::try_from("turn-compaction").expect("turn id"),
                    )
                    .summary("structured summary".to_string())
                    .tokens_before(42_000)
                    .usage(Some(usage))
                    .started_at_ms(TimestampMs::from(1_725_000_001_003))
                    .ended_at_ms(TimestampMs::from(1_725_000_001_004))
                    .build(),
            },
        },
    };
    let failed = AgentEventPayload::CompactionEnd {
        run_id: RunId::try_from("run-failed").expect("run id"),
        reason: CompactionReason::Threshold,
        outcome: CompactionOutcome::Failed {
            message: "context compaction failed".to_string(),
        },
    };
    let cancelled = AgentEventPayload::CompactionEnd {
        run_id: RunId::try_from("run-cancelled").expect("run id"),
        reason: CompactionReason::Overflow,
        outcome: CompactionOutcome::Cancelled,
    };

    let value = serde_json::to_value(&completed).expect("serialize completed");
    assert_eq!(value["outcome"]["status"], "completed");
    assert_eq!(value["outcome"]["result"]["tokensBefore"], "42000");
    assert_eq!(value["outcome"]["result"]["usage"]["total_tokens"], "125");
    let decoded: AgentEvent =
        serde_json::from_value(value).expect("decode completed");
    assert_eq!(decoded, completed);
    assert_eq!(
        serde_json::to_value(failed).expect("serialize failed")["outcome"]["status"],
        "failed"
    );
    assert_eq!(
        serde_json::to_value(cancelled).expect("serialize cancelled")["outcome"]
            ["status"],
        "cancelled"
    );
}

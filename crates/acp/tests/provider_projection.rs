use acp::AcpEventMapper;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    ContentBlock, EventMetadata, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelUsage, Sequence, StopReason, TimestampMs, TurnId,
    Usage,
};
use serde_json::{Value, json};

/// Builds a complete assistant event without retaining any live provider or session state.
fn assistant(blocks: Vec<ContentBlock>, usage: ModelUsage) -> AgentEvent {
    let timestamp = TimestampMs::from(1);
    AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("t").expect("valid synthetic test state"),
            timestamp_ms: timestamp,
            sequence: Sequence::try_from(1)
                .expect("valid synthetic test state"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("m")
                        .expect("valid synthetic test state"),
                    turn_id: TurnId::try_from("t")
                        .expect("valid synthetic test state"),
                },
                timing: MessageTiming::try_from((
                    timestamp, timestamp, timestamp,
                ))
                .expect("valid synthetic test state"),
                content: MessageContent::Assistant {
                    blocks,
                    model_message: None,
                    metadata: AssistantMetadata::builder()
                        .provider_id("openai".into())
                        .model_id("audit".into())
                        .stop_reason(StopReason::EndTurn)
                        .usage(usage)
                        .build(),
                },
            },
        },
    }
}

/// A full assistant snapshot always clears or replaces previously streamed thought content.
#[test]
fn final_assistant_emits_empty_thought_snapshot() {
    let updates = AcpEventMapper::map(assistant(
        vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        ModelUsage::default(),
    ))
    .expect("valid synthetic test state");
    let values: Vec<_> = updates
        .into_iter()
        .map(|update| {
            serde_json::to_value(update).expect("valid synthetic test state")
        })
        .collect();
    let thought = values
        .iter()
        .find(|value| value["sessionUpdate"] == "agent_thought")
        .expect("complete thought snapshot");
    assert_eq!(thought["messageId"], "m");
    assert_eq!(thought["content"], json!([]));
}

/// Project metadata keeps exact decimal integers and distinguishes absence from reported zero.
#[test]
fn usage_metadata_preserves_large_values_zero_and_absence() {
    let usage = Usage::new()
        .input_tokens(9_007_199_254_740_993)
        .output_tokens(0)
        .cached_input_tokens(0)
        .into();
    let updates = AcpEventMapper::map(assistant(Vec::new(), usage))
        .expect("valid synthetic test state");
    let value =
        serde_json::to_value(&updates[0]).expect("valid synthetic test state");
    let usage = &value["_meta"]["clawcode"]["assistant"]["usage"];
    assert_eq!(usage["input_tokens"], "9007199254740993");
    assert_eq!(usage["output_tokens"], "0");
    assert_eq!(usage["cache_read_tokens"], "0");
    assert!(usage.get("cache_write_tokens").is_none());
    assert!(usage.get("format").is_none());
}

/// Missing mandatory standard counters produce a lossless project update instead of false zero.
#[test]
fn unknown_context_usage_is_sent_without_fabricated_standard_counts() {
    let mut event = assistant(Vec::new(), ModelUsage::default());
    event.payload = AgentEventPayload::UsageUpdated {
        usage: ModelUsage::default(),
        context_window: 32768,
    };
    let values = serde_json::to_value(
        AcpEventMapper::map(event).expect("valid synthetic test state"),
    )
    .expect("valid synthetic test state");
    assert_eq!(values[0]["sessionUpdate"], "_clawcode/event");
    assert_eq!(values[0]["payload"]["usage"], json!({}));
    assert_eq!(values[0]["_meta"]["clawcode"]["contextWindow"], "32768");
}

/// Native encrypted payloads stay in model history and never reach browser metadata.
#[test]
fn native_message_is_not_exposed_in_acp_updates() {
    let mut event = assistant(
        vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        ModelUsage::default(),
    );
    if let AgentEventPayload::MessageEnd { message } = &mut event.payload
        && let MessageContent::Assistant { model_message, .. } =
            &mut message.content
    {
        *model_message=Some(serde_json::from_value(json!({
                "content":[{"type":"reasoning","text":"display","native":{"fingerprint":"0000000000000000","item":{"type":"reasoning","encrypted_content":"synthetic-encrypted"}}}]
            })).expect("valid synthetic test state"));
    }
    assert!(
        serde_json::to_string(&event)
            .expect("valid synthetic test state")
            .contains("synthetic-encrypted")
    );
    let value: Value = serde_json::to_value(
        AcpEventMapper::map(event).expect("valid synthetic test state"),
    )
    .expect("valid synthetic test state");
    assert!(!value.to_string().contains("synthetic-encrypted"));
    assert!(!value.to_string().contains("model_message"));
}
/// Context size follows upstream fallback without inventing a reported billing total.
#[test]
fn context_usage_falls_back_to_input_and_saturating_output() {
    for (input, output, total, expected) in [
        (Some(100), Some(20), None, 120),
        (Some(100), None, None, 100),
        (Some(u64::MAX), Some(1), None, u64::MAX),
        (Some(100), Some(20), Some(0), 0),
        (Some(0), None, None, 0),
    ] {
        let mut event = assistant(Vec::new(), ModelUsage::default());
        event.payload = AgentEventPayload::UsageUpdated {
            usage: Usage::new()
                .input_tokens(input)
                .output_tokens(output)
                .total_tokens(total)
                .into(),
            context_window: 32768,
        };
        let values =
            serde_json::to_value(AcpEventMapper::map(event).unwrap()).unwrap();
        assert_eq!(values[0]["sessionUpdate"], "usage_update");
        assert_eq!(values[0]["used"], expected);
        if total.is_none() {
            assert!(
                values[0]["_meta"]["clawcode"]["usage"]
                    .get("total_tokens")
                    .is_none()
            );
        }
    }
}

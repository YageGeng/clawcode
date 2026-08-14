use std::collections::BTreeMap;
use std::convert::TryFrom;

use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, ExtensionMessage,
    MessageContent, MessageId, MessageIdentity, MessageTiming,
    MessageTimingError, ModelUsage, Role, StopReason, TimestampMs, TurnId,
};

/// A complete agent message exposes exact turn and timing metadata at the wire boundary.
#[test]
fn agent_message_flattens_turn_and_timing_metadata() {
    let instant =
        TimestampMs::try_from("9007199254740993").expect("valid timestamp");
    let message = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-1")
                .expect("valid message id"),
            turn_id: TurnId::try_from("turn-1").expect("valid turn id"),
        },
        timing: MessageTiming::try_from((instant, instant, instant))
            .expect("ordered message timing"),
        content: MessageContent::Assistant {
            blocks: vec![ContentBlock::Text {
                text: "done".to_string(),
            }],
            metadata: AssistantMetadata::builder()
                .provider_id("test".to_string())
                .model_id("test-model".to_string())
                .stop_reason(StopReason::EndTurn)
                .usage(
                    ModelUsage::builder()
                        .input_tokens(1)
                        .output_tokens(1)
                        .cache_read_tokens(0)
                        .cache_write_tokens(0)
                        .total_tokens(2)
                        .build(),
                )
                .build(),
        },
    };

    let value =
        serde_json::to_value(&message).expect("message should serialize");

    assert_eq!(value["message_id"], "message-1");
    assert_eq!(value["turn_id"], "turn-1");
    assert_eq!(value["timestamp_ms"], "9007199254740993");
    assert_eq!(value["started_at_ms"], "9007199254740993");
    assert_eq!(value["ended_at_ms"], "9007199254740993");
    assert_eq!(value["content"]["type"], "assistant");
    assert_eq!(message.content.role(), Some(Role::Assistant));
    assert_eq!(message.content.text_content().as_deref(), Some("done"));
}

/// Message timing rejects an end timestamp that precedes the first token timestamp.
#[test]
fn message_timing_rejects_reversed_intervals() {
    let timestamp = TimestampMs::try_from("100").expect("valid timestamp");
    let started = TimestampMs::try_from("200").expect("valid timestamp");
    let ended = TimestampMs::try_from("199").expect("valid timestamp");

    assert_eq!(
        MessageTiming::try_from((timestamp, started, ended)),
        Err(MessageTimingError::EndBeforeStart { started, ended })
    );
}

/// Unknown pi message payloads survive protocol serialization without shape loss.
#[test]
fn extension_message_preserves_unknown_payload_and_metadata() {
    let message = ExtensionMessage {
        discriminator: "pi.custom.progress".to_string(),
        payload: serde_json::json!({ "completed": 3, "total": 8 }),
        meta: BTreeMap::from([(
            "source".to_string(),
            serde_json::json!("extension"),
        )]),
    };

    let encoded =
        serde_json::to_value(message).expect("extension should serialize");

    assert_eq!(encoded["discriminator"], "pi.custom.progress");
    assert_eq!(encoded["payload"]["completed"], 3);
    assert_eq!(encoded["meta"]["source"], "extension");
}

/// Image blocks preserve base64 payloads and MIME types for tool-result replay.
#[test]
fn image_content_roundtrips_without_text_projection() {
    let block = ContentBlock::Image {
        data: "iVBORw0KGgo=".to_string(),
        mime_type: "image/png".to_string(),
    };

    let encoded = serde_json::to_value(&block).expect("serialize image");
    let decoded: ContentBlock =
        serde_json::from_value(encoded.clone()).expect("deserialize image");

    assert_eq!(encoded["type"], "image");
    assert_eq!(encoded["mime_type"], "image/png");
    assert_eq!(decoded, block);
    assert_eq!(decoded.text(), None);
}

use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    ContentBlock, EventMetadata, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelInputModalities, ModelInputModality, ModelProfile,
    ModelRequest, ModelRequestOptions, ModelUsage, RunId, Sequence, StopReason,
    TimestampMs, ToolCallId, TurnId,
};

#[test]
fn model_input_modalities_reject_invalid_deserialization() {
    let error =
        serde_json::from_str::<ModelInputModalities>(r#"["image","text"]"#)
            .expect_err("reversed modalities must fail");

    assert!(error.to_string().contains("input must start with text"));
}

/// Builds deterministic message identity data for protocol wire assertions.
fn sample_identity(message_id: &str, turn_id: &str) -> MessageIdentity {
    MessageIdentity {
        message_id: MessageId::try_from(message_id).expect("message id"),
        turn_id: TurnId::try_from(turn_id).expect("turn id"),
    }
}

/// Builds deterministic complete message timing for protocol wire assertions.
fn sample_timing(
    timestamp_ms: &str,
    started_at_ms: &str,
    ended_at_ms: &str,
) -> MessageTiming {
    MessageTiming::try_from((
        TimestampMs::try_from(timestamp_ms).expect("timestamp"),
        TimestampMs::try_from(started_at_ms).expect("start timestamp"),
        TimestampMs::try_from(ended_at_ms).expect("end timestamp"),
    ))
    .expect("ordered timing")
}

/// Builds deterministic event metadata for lifecycle wire assertions.
fn sample_event_metadata(
    turn_id: &str,
    timestamp_ms: &str,
    sequence: u64,
) -> EventMetadata {
    EventMetadata {
        turn_id: TurnId::try_from(turn_id).expect("turn id"),
        timestamp_ms: TimestampMs::try_from(timestamp_ms)
            .expect("event timestamp"),
        sequence: Sequence::try_from(sequence).expect("sequence"),
    }
}

/// Assistant results preserve exact usage, provider identity, and output timing.
#[test]
fn assistant_metadata_keeps_exact_usage_and_timing() {
    let usage = ModelUsage::builder()
        .input_tokens(9_007_199_254_740_999)
        .output_tokens(7)
        .cache_read_tokens(3)
        .cache_write_tokens(2)
        .reasoning_tokens(Some(1))
        .total_tokens(9_007_199_254_741_012)
        .build();
    let message = AgentMessage {
        identity: sample_identity("assistant-1", "turn-1"),
        timing: sample_timing("100", "110", "140"),
        content: MessageContent::Assistant {
            blocks: vec![ContentBlock::Text {
                text: "ok".to_string(),
            }],
            metadata: AssistantMetadata::builder()
                .provider_id("deepseek".to_string())
                .model_id("deepseek-v4".to_string())
                .stop_reason(StopReason::EndTurn)
                .usage(usage)
                .build(),
        },
    };

    let value = serde_json::to_value(message).expect("serialize assistant");
    assert_eq!(value["turn_id"], "turn-1");
    assert_eq!(value["started_at_ms"], "110");
    assert_eq!(value["content"]["type"], "assistant");
    assert_eq!(value["content"]["metadata"]["provider_id"], "deepseek");
    assert_eq!(
        value["content"]["metadata"]["usage"]["input_tokens"],
        "9007199254740999"
    );
    assert_eq!(
        value["content"]["metadata"]["usage"]["total_tokens"],
        "9007199254741012"
    );
}

/// Usage decoding rejects values that are not precision-safe decimal strings.
#[test]
fn model_usage_rejects_non_decimal_wire_values() {
    let value = serde_json::json!({
        "input_tokens": "not-a-number",
        "output_tokens": "7",
        "cache_read_tokens": "3",
        "cache_write_tokens": "2",
        "reasoning_tokens": "1",
        "total_tokens": "13"
    });

    serde_json::from_value::<ModelUsage>(value)
        .expect_err("non-decimal usage must be rejected");
}

/// Retry lifecycle events preserve mandatory turn and timestamp metadata.
#[test]
fn retry_and_usage_events_keep_turn_metadata() {
    let retry = AgentEvent {
        metadata: sample_event_metadata("turn-retry", "1700000000001", 8),
        payload: AgentEventPayload::RetryScheduled {
            run_id: RunId::try_from("run-1").expect("run id"),
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2_000,
            error: "rate limited".to_string(),
        },
    };
    let usage = AgentEvent {
        metadata: sample_event_metadata("turn-retry", "1700000000002", 9),
        payload: AgentEventPayload::UsageUpdated {
            usage: ModelUsage::builder()
                .input_tokens(10)
                .output_tokens(2)
                .cache_read_tokens(1)
                .cache_write_tokens(0)
                .total_tokens(13)
                .build(),
            context_window: 128_000,
        },
    };

    let retry_value = serde_json::to_value(retry).expect("retry event json");
    assert_eq!(retry_value["turn_id"], "turn-retry");
    assert_eq!(retry_value["timestamp_ms"], "1700000000001");
    assert_eq!(retry_value["event"], "retry_scheduled");
    assert_eq!(retry_value["attempt"], 1);

    let usage_value = serde_json::to_value(usage).expect("usage event json");
    assert_eq!(usage_value["event"], "usage_updated");
    assert_eq!(usage_value["usage"]["input_tokens"], "10");
    assert_eq!(usage_value["context_window"], 128_000);
}

/// Text-only models receive one placeholder for each consecutive image run.
#[test]
fn text_only_model_request_replaces_unsupported_images() {
    let original_blocks = vec![
        ContentBlock::Text {
            text: "before".to_string(),
        },
        ContentBlock::Image {
            data: "first".to_string(),
            mime_type: "image/png".to_string(),
        },
        ContentBlock::Image {
            data: "second".to_string(),
            mime_type: "image/jpeg".to_string(),
        },
        ContentBlock::Text {
            text: "after".to_string(),
        },
    ];
    let mut request = ModelRequest {
        messages: vec![
            AgentMessage {
                identity: sample_identity("user-image", "turn-image"),
                timing: sample_timing("200", "200", "200"),
                content: MessageContent::User {
                    blocks: original_blocks.clone(),
                },
            },
            AgentMessage {
                identity: sample_identity("tool-image", "turn-image"),
                timing: sample_timing("201", "201", "201"),
                content: MessageContent::ToolResult {
                    tool_call_id: ToolCallId::try_from("tool-call")
                        .expect("tool call id"),
                    blocks: vec![ContentBlock::Image {
                        data: "tool".to_string(),
                        mime_type: "image/webp".to_string(),
                    }],
                    is_error: false,
                    details: None,
                },
            },
        ],
        tools: Vec::new(),
        options: ModelRequestOptions::default(),
    };
    let profile = ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("text-only".to_string())
        .display_name("Text only".to_string())
        .context_tokens(8_192)
        .max_output_tokens(1_024)
        .build();

    assert!(request.adapt_input(&profile));

    let MessageContent::User { blocks } = &request.messages[0].content else {
        panic!("user message expected");
    };
    assert_eq!(
        blocks,
        &vec![
            ContentBlock::Text {
                text: "before".to_string(),
            },
            ContentBlock::Text {
                text: "(image omitted: model does not support images)"
                    .to_string(),
            },
            ContentBlock::Text {
                text: "after".to_string(),
            },
        ]
    );
    let MessageContent::ToolResult { blocks, .. } =
        &request.messages[1].content
    else {
        panic!("tool result expected");
    };
    assert_eq!(
        blocks,
        &vec![ContentBlock::Text {
            text: "(tool image omitted: model does not support images)"
                .to_string(),
        }]
    );
    assert!(matches!(original_blocks[1], ContentBlock::Image { .. }));
}

/// Image-capable models retain the exact user image block.
#[test]
fn image_model_request_preserves_images() {
    let image = ContentBlock::Image {
        data: "image-data".to_string(),
        mime_type: "image/png".to_string(),
    };
    let mut request = ModelRequest {
        messages: vec![AgentMessage {
            identity: sample_identity("user-vision", "turn-vision"),
            timing: sample_timing("300", "300", "300"),
            content: MessageContent::User {
                blocks: vec![image.clone()],
            },
        }],
        tools: Vec::new(),
        options: ModelRequestOptions::default(),
    };
    let profile = ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("vision".to_string())
        .display_name("Vision".to_string())
        .context_tokens(8_192)
        .max_output_tokens(1_024)
        .input(
            ModelInputModalities::try_from(vec![
                ModelInputModality::Text,
                ModelInputModality::Image,
            ])
            .expect("image modalities"),
        )
        .build();

    assert!(!request.adapt_input(&profile));
    assert!(matches!(
        &request.messages[0].content,
        MessageContent::User { blocks } if blocks == &vec![image]
    ));
}

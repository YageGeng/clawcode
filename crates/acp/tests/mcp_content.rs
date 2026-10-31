use acp::AcpEventMapper;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    ContentBlock, EmbeddedResourceContent, EventMetadata, MessageContent,
    MessageId, MessageIdentity, MessageTiming, ProductIdentity, RunId,
    Sequence, StopReason, TimestampMs, ToolCallId, ToolResult, TurnId,
};

/// ACP v2 uses native MCP-compatible blocks and a product extension only for structured content.
#[test]
fn assistant_mcp_content_uses_native_and_extension_blocks() {
    let turn_id = TurnId::try_from("turn-mcp-content").expect("turn id");
    let timestamp = TimestampMs::from(9_007_199_254_740_999);
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: turn_id.clone(),
            timestamp_ms: timestamp,
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-mcp-content")
                        .expect("message id"),
                    turn_id,
                },
                timing: MessageTiming::try_from((
                    timestamp, timestamp, timestamp,
                ))
                .expect("message timing"),
                content: MessageContent::Assistant {
                    model_message: None,
                    blocks: vec![
                        ContentBlock::Audio {
                            data: "YXVkaW8=".to_string(),
                            mime_type: "audio/wav".to_string(),
                        },
                        ContentBlock::ResourceLink {
                            server_id: None,
                            uri: "file:///workspace/report.pdf".to_string(),
                            name: "report".to_string(),
                            title: Some("Report".to_string()),
                            description: None,
                            mime_type: Some("application/pdf".to_string()),
                            size: Some(4_096),
                        },
                        ContentBlock::EmbeddedResource {
                            uri: "file:///workspace/readme.md".to_string(),
                            mime_type: Some("text/markdown".to_string()),
                            content: EmbeddedResourceContent::Text {
                                text: "# Readme".to_string(),
                            },
                        },
                        ContentBlock::Structured {
                            value: serde_json::json!({ "count": 2 }),
                        },
                    ],
                    metadata: AssistantMetadata::builder()
                        .provider_id("provider".to_string())
                        .model_id("model".to_string())
                        .stop_reason(StopReason::EndTurn)
                        .usage(protocol::ModelUsage::from(
                            protocol::Usage::new()
                                .input_tokens(1)
                                .output_tokens(1)
                                .cached_input_tokens(0)
                                .cache_creation_input_tokens(0)
                                .total_tokens(2),
                        ))
                        .build(),
                },
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map MCP content");
    let value = serde_json::to_value(&updates[0]).expect("serialize update");
    let content = value["content"].as_array().expect("message content");

    assert_eq!(content[0]["type"], "audio");
    assert_eq!(content[0]["mimeType"], "audio/wav");
    assert_eq!(content[1]["type"], "resource_link");
    assert_eq!(content[1]["uri"], "file:///workspace/report.pdf");
    assert_eq!(content[2]["type"], "resource");
    assert_eq!(content[2]["resource"]["text"], "# Readme");
    assert_eq!(
        content[3]["type"],
        ProductIdentity::ACP_STRUCTURED_CONTENT_BLOCK
    );
    assert_eq!(content[3]["value"]["count"], 2);
    for block in content {
        assert_eq!(
            block["_meta"][ProductIdentity::ACP_NAMESPACE]["turnId"],
            "turn-mcp-content"
        );
        assert_eq!(
            block["_meta"][ProductIdentity::ACP_NAMESPACE]["timestampMs"],
            "9007199254740999"
        );
    }
}

/// Tool completion preserves MCP-native content and the unmodified structured result.
#[test]
fn tool_mcp_content_uses_native_blocks_and_raw_output() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-mcp-tool").expect("turn id"),
            timestamp_ms: TimestampMs::from(9_007_199_254_741_000),
            sequence: Sequence::try_from(2).expect("sequence"),
        },
        payload: AgentEventPayload::ToolExecutionEnd {
            run_id: RunId::try_from("run-mcp-tool").expect("run id"),
            result: ToolResult::builder()
                .tool_call_id(
                    ToolCallId::try_from("call-mcp-tool")
                        .expect("tool call id"),
                )
                .blocks(vec![
                    ContentBlock::EmbeddedResource {
                        uri: "memory://result.bin".to_string(),
                        mime_type: Some("application/octet-stream".to_string()),
                        content: EmbeddedResourceContent::Blob {
                            data: "AAEC".to_string(),
                        },
                    },
                    ContentBlock::Structured {
                        value: serde_json::json!({ "ok": true }),
                    },
                ])
                .is_error(false)
                .output(Some(
                    protocol::ToolOutputInfo::builder()
                        .original_text_bytes(30_000)
                        .visible_text_bytes(20_000)
                        .omitted_media(1)
                        .full_output_path(Some("/tmp/output.json".into()))
                        .build(),
                ))
                .build(),
        },
    };

    let updates =
        AcpEventMapper::map(event.clone()).expect("map MCP tool content");
    let value = serde_json::to_value(&updates[0]).expect("serialize update");

    assert_eq!(value["sessionUpdate"], "tool_call_update");
    assert_eq!(value["status"], "completed");
    assert_eq!(value["content"][0]["content"]["type"], "resource");
    assert_eq!(value["content"][0]["content"]["resource"]["blob"], "AAEC");
    assert_eq!(
        value["content"][1]["content"]["type"],
        ProductIdentity::ACP_STRUCTURED_CONTENT_BLOCK
    );
    assert_eq!(value["rawOutput"]["blocks"][1]["value"]["ok"], true);
    assert_eq!(value["rawOutput"]["output"]["visibleTextBytes"], 20_000);
    let AgentEventPayload::ToolExecutionEnd { result, .. } = event.payload
    else {
        unreachable!("tool result")
    };
    let replay = AgentEvent {
        metadata: event.metadata,
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("replay-output")
                        .expect("id"),
                    turn_id: TurnId::try_from("turn-mcp-tool").expect("id"),
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(1),
                    TimestampMs::from(1),
                    TimestampMs::from(1),
                ))
                .expect("timing"),
                content: MessageContent::ToolResult {
                    tool_call_id: result.tool_call_id,
                    blocks: result.blocks,
                    is_error: result.is_error,
                    details: result.details,
                    output: result.output,
                    cancelled: false,
                },
            },
        },
    };
    let replay = AcpEventMapper::map(replay).expect("replay");
    let replay =
        serde_json::to_value(replay.first().expect("update")).expect("wire");
    assert_eq!(
        replay["rawOutput"]["content"]["output"],
        value["rawOutput"]["output"]
    );
}

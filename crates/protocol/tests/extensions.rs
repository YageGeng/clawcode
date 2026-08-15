use std::path::PathBuf;

use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, ExtensionDescriptor,
    ExtensionId, ExtensionInvocation, ExtensionMessage, ExtensionMessageDraft,
    MessageContent, MessageId, MessageIdentity, MessageTiming, MessageUpdate,
    MessageUpdateEvent, ModelUsage, SessionId, StopReason, TimestampMs,
    ToolBlock, ToolCall, ToolCallId, ToolCallResult, TurnId,
};

/// Extension invocations preserve millisecond timestamps beyond JavaScript's safe integer range.
#[test]
fn extension_invocation_serializes_precision_safe_time() {
    let value = serde_json::to_value(
        ExtensionInvocation::builder()
            .extension_id(ExtensionId::try_from("audit").expect("extension id"))
            .session_id(SessionId::try_from("session-1").expect("session id"))
            .timestamp_ms(TimestampMs::from(u64::MAX))
            .cwd(PathBuf::from("/workspace"))
            .build(),
    )
    .expect("serialize invocation");

    assert_eq!(
        value["timestamp_ms"],
        serde_json::json!(u64::MAX.to_string())
    );
}

/// Tool-call handlers can block execution without representing unrelated directives.
#[test]
fn tool_call_result_has_a_domain_specific_block_variant() {
    let result = ToolCallResult::Block(
        ToolBlock::builder()
            .reason("denied".to_string())
            .terminate(true)
            .build(),
    );

    assert_eq!(
        serde_json::to_value(result).expect("serialize")["type"],
        "block"
    );
}

/// Extension messages expose typed identity and context behavior instead of opaque envelopes.
#[test]
fn extension_message_draft_and_persisted_message_share_content_shape() {
    let extension_id =
        ExtensionId::try_from("audit").expect("extension identifier");
    let draft = ExtensionMessageDraft::builder()
        .extension_id(extension_id.clone())
        .custom_type("progress".to_string())
        .blocks(vec![ContentBlock::Text {
            text: "working".to_string(),
        }])
        .display(true)
        .include_in_context(false)
        .details(serde_json::json!({ "completed": 1 }))
        .build();
    let message = ExtensionMessage::builder()
        .extension_id(extension_id)
        .custom_type(draft.custom_type.clone())
        .blocks(draft.blocks.clone())
        .display(draft.display)
        .include_in_context(draft.include_in_context)
        .details(
            draft
                .details
                .clone()
                .expect("draft contains structured details"),
        )
        .build();

    let value = serde_json::to_value(message).expect("serialize message");
    assert_eq!(value["extension_id"], "audit");
    assert_eq!(value["custom_type"], "progress");
    assert_eq!(value["include_in_context"], false);
}

/// Descriptors reject blank extension identifiers before runtime registration.
#[test]
fn extension_descriptor_uses_validated_identity() {
    ExtensionId::try_from("   ").expect_err("blank extension id");
    let descriptor = ExtensionDescriptor {
        id: ExtensionId::try_from("audit").expect("extension identifier"),
        name: "Audit".to_string(),
        version: "1.0.0".to_string(),
    };

    let decoded: ExtensionDescriptor = serde_json::from_value(
        serde_json::to_value(descriptor.clone()).expect("serialize descriptor"),
    )
    .expect("deserialize descriptor");
    assert_eq!(decoded, descriptor);
}

/// Message updates carry complete tool-call data instead of an untyped string delta.
#[test]
fn message_update_represents_tool_calls_with_the_shared_type() {
    let timestamp = TimestampMs::from(1);
    let message = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-1").expect("message id"),
            turn_id: TurnId::try_from("turn-1").expect("turn id"),
        },
        timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
            .expect("message timing"),
        content: MessageContent::Assistant {
            blocks: Vec::new(),
            metadata: AssistantMetadata::builder()
                .provider_id("provider".to_string())
                .model_id("model".to_string())
                .stop_reason(StopReason::ToolUse)
                .usage(
                    ModelUsage::builder()
                        .input_tokens(0)
                        .output_tokens(0)
                        .cache_read_tokens(0)
                        .cache_write_tokens(0)
                        .total_tokens(0)
                        .build(),
                )
                .build(),
        },
    };
    let call = ToolCall {
        tool_call_id: ToolCallId::try_from("call-1").expect("tool call id"),
        name: "read".to_string(),
        arguments: serde_json::json!({ "path": "README.md" }),
    };
    let event = MessageUpdateEvent {
        message,
        update: MessageUpdate::ToolCall { call: call.clone() },
    };

    assert_eq!(
        serde_json::to_value(event).expect("serialize message update")["update"]
            ["call"],
        serde_json::to_value(call).expect("serialize tool call")
    );
}

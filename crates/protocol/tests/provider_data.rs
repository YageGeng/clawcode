use protocol::ModelUsage;
use serde_json::{Value, json};

/// New usage records preserve reported zero independently of absent counters.
#[test]
fn upstream_usage_roundtrips_reported_zero_and_unknown() {
    let value = json!({
        "format": "rig-core-v1",
        "usage": { "input_tokens": 0, "cached_input_tokens": 0 }
    });
    let usage: ModelUsage =
        serde_json::from_value(value.clone()).expect("decode upstream usage");
    assert_eq!(serde_json::to_value(usage).expect("encode usage"), value);
}

/// Legacy Anthropic input is recovered from the complete total instead of double-counting cache.
#[test]
fn legacy_usage_recovers_inclusive_input() {
    let usage: ModelUsage = serde_json::from_value(json!({
        "input_tokens": "7", "output_tokens": "3", "total_tokens": "40",
        "cache_read_tokens": "10", "cache_write_tokens": "20"
    }))
    .expect("decode legacy usage");
    let value = serde_json::to_value(usage).expect("encode usage");
    assert_eq!(value["format"], "rig-core-v1");
    assert_eq!(value["usage"]["input_tokens"], 37);
    assert_eq!(value["usage"]["cached_input_tokens"], 10);
    assert_eq!(value["usage"]["cache_creation_input_tokens"], 20);
    assert_eq!(value["usage"]["total_tokens"], 40);
}

/// Old zero defaults cannot prove that a provider reported a zero counter.
#[test]
fn legacy_zero_defaults_become_unknown() {
    let usage: ModelUsage = serde_json::from_value(json!({
        "input_tokens": "0", "output_tokens": "0", "total_tokens": "0",
        "cache_read_tokens": "0", "cache_write_tokens": "0",
        "reasoning_tokens": "0"
    }))
    .expect("decode old zero defaults");
    let value = serde_json::to_value(usage).expect("encode usage");
    assert_eq!(value["usage"], json!({ "reasoning_tokens": 0 }));
}

/// Contradictory historical totals must not become authoritative context sizes.
#[test]
fn inconsistent_legacy_usage_discards_derived_totals() {
    for (output, total, read) in [("20", "10", "1"), ("3", "10", "20")] {
        let usage: ModelUsage = serde_json::from_value(json!({
            "input_tokens": "7", "output_tokens": output, "total_tokens": total,
            "cache_read_tokens": read, "cache_write_tokens": "0"
        }))
        .expect("decode inconsistent historical usage");
        let value = serde_json::to_value(usage).expect("encode usage");
        assert_eq!(value["usage"]["input_tokens"], Value::Null);
        assert_eq!(value["usage"]["total_tokens"], Value::Null);
        assert!(value["usage"]["output_tokens"].is_number());
    }
}

/// Unknown version tags are rejected explicitly rather than interpreted as legacy data.
#[test]
fn unknown_usage_format_is_rejected_explicitly() {
    let error = serde_json::from_value::<ModelUsage>(json!({
        "format": "future-v2", "usage": {}
    }))
    .expect_err("unknown version must fail");
    assert!(error.to_string().contains("future-v2"));
}

/// Native replay provenance and structured overflow facts survive the shared protocol.
#[test]
fn native_assistant_message_survives_protocol_storage() {
    use rig_core::message::{AssistantContent, AssistantMessage, Origin};
    let native = AssistantMessage::new(vec![
        AssistantContent::text("done").with_native(
            json!({"type":"message","id":"native-1","phase":"final_answer"}),
        ),
    ])
    .with_origin(Origin::new("openai.responses", "openai", "audit"));
    let native = serde_json::to_value(native).expect("native message");
    let content: protocol::MessageContent = serde_json::from_value(json!({
        "type":"assistant", "blocks":[{"type":"text","text":"done"}],
        "model_message":native,
        "metadata":{
            "provider_id":"openai","model_id":"audit","stop_reason":"end_turn",
            "usage":{"format":"rig-core-v1","usage":{}},"context_overflow":true
        }
    }))
    .expect("decode assistant");
    let value = serde_json::to_value(content).expect("encode assistant");
    assert_eq!(value["model_message"], native);
    assert_eq!(value["metadata"]["context_overflow"], true);
}

/// Invalid provider argument JSON must remain distinguishable from an empty valid object.
#[test]
fn invalid_tool_arguments_survive_protocol_storage() {
    let call: protocol::ToolCall = serde_json::from_value(json!({
        "tool_call_id":"call-1","name":"read","arguments":{},
        "invalid_arguments":"{\"path\":"
    }))
    .expect("decode tool call");
    assert_eq!(
        serde_json::to_value(call).expect("encode call")["invalid_arguments"],
        "{\"path\":"
    );
    let block: protocol::ContentBlock = serde_json::from_value(json!({
        "type":"tool_call","tool_call_id":"call-1","name":"read","arguments":{},
        "invalid_arguments":"{\"path\":"
    }))
    .expect("decode tool block");
    assert_eq!(
        serde_json::to_value(block).expect("encode block")["invalid_arguments"],
        "{\"path\":"
    );
}

/// Error projection preserves classification without copying an untrusted provider body.
#[test]
fn upstream_errors_keep_retry_and_overflow_without_body_text() {
    let error = rig_core::ProviderError::from_http_response(
        http::StatusCode::TOO_MANY_REQUESTS,
        "synthetic-sensitive-body",
    );
    let failure = protocol::ModelFailure::from(error);
    assert_eq!(failure.status, Some(429));
    assert_eq!(
        failure.retry_disposition,
        protocol::ModelRetryDisposition::Retryable
    );
    assert!(!failure.summary.contains("synthetic-sensitive-body"));
    let error = rig_core::ProviderError::from_http_response(
        http::StatusCode::PAYLOAD_TOO_LARGE,
        "context_length_exceeded",
    );
    assert!(protocol::ModelFailure::from(error).context_overflow);
}

/// The project boundary keeps malformed arguments and rejects whitespace-only call ids.
#[test]
fn upstream_tool_call_conversion_preserves_invalid_arguments() {
    let function = rig_core::message::ToolFunction::new(
        rig_core::message::ToolName::new("read").unwrap(),
        json!("{\"path\":"),
    );
    let call =
        rig_core::message::ToolCall::from_wire("call-1", function.clone());
    let converted =
        protocol::ToolCall::try_from(&call).expect("valid call identity");
    assert_eq!(converted.invalid_arguments.as_deref(), Some("{\"path\":"));
    let invalid = rig_core::message::ToolCall::from_wire(" ", function);
    protocol::ToolCall::try_from(&invalid).unwrap_err();
}

/// Recovery decisions use explicit upstream facts rather than matching diagnostic text.
#[test]
fn retryability_uses_structured_flags() {
    let failure = protocol::ModelFailure::builder()
        .summary("arbitrary diagnostic".into())
        .retry_disposition(protocol::ModelRetryDisposition::Retryable)
        .build();
    assert!(failure.is_retryable());
    let overflow = protocol::ModelFailure::builder()
        .summary("arbitrary diagnostic".into())
        .retry_disposition(protocol::ModelRetryDisposition::Retryable)
        .context_overflow(true)
        .build();
    assert!(!overflow.is_retryable());
}
/// Terminal failure categories never copy untrusted explanations and preserve upstream decisions.
#[test]
fn terminal_failures_keep_safe_categories_and_usage() {
    use protocol::ModelFailure;
    use rig_core::{
        completion::{CompletionResponse, FinishReason, Usage},
        message::{AssistantContent, Origin, ToolCall, ToolFunction, ToolName},
    };
    let sentinel = "PRIVATE_TERMINAL_SENTINEL";
    let call = AssistantContent::ToolCall(ToolCall::from_wire(
        "call-1",
        ToolFunction::new(ToolName::new("read").unwrap(), json!({})),
    ));
    for (choice, finish, error, aborted, expected) in [
        (
            vec![AssistantContent::text("ok")],
            FinishReason::Stop,
            None,
            None,
            None,
        ),
        (
            vec![AssistantContent::text("partial")],
            FinishReason::Length,
            None,
            None,
            None,
        ),
        (
            vec![],
            FinishReason::Length,
            None,
            None,
            Some("output limit"),
        ),
        (
            vec![AssistantContent::text("declined")],
            FinishReason::ContentFilter,
            Some(sentinel),
            None,
            Some("content_filter"),
        ),
        (
            vec![AssistantContent::text("ignored")],
            FinishReason::Stop,
            Some(sentinel),
            None,
            Some("provider-reported error"),
        ),
        (
            vec![],
            FinishReason::Other(sentinel.into()),
            None,
            None,
            Some("unsupported finish reason"),
        ),
        (
            vec![call.clone()],
            FinishReason::ToolCalls,
            Some(sentinel),
            None,
            Some("none of its tool calls ran"),
        ),
        (
            vec![call],
            FinishReason::ToolCalls,
            None,
            Some(sentinel),
            Some("aborted"),
        ),
    ] {
        let mut response = CompletionResponse::new(
            choice,
            Usage::new().input_tokens(10),
            Origin::new("openai.chat", "openai", "audit"),
            json!({"private":sentinel}),
        )
        .with_finish_reason(finish);
        response.error = error.map(str::to_owned);
        response.aborted = aborted.map(str::to_owned);
        let failure = ModelFailure::from_response(&response);
        match expected {
            None => assert!(failure.is_none()),
            Some(category) => {
                let failure = failure.expect("failed terminal response");
                assert!(
                    failure.summary.contains(category),
                    "{}",
                    failure.summary
                );
                assert!(!failure.summary.contains(sentinel));
                assert!(!failure.is_retryable());
                assert_eq!(
                    failure.usage.as_deref().unwrap().as_ref().input_tokens,
                    Some(10)
                );
            }
        }
    }
}

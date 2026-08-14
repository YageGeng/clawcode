use std::convert::TryFrom;

use protocol::{
    ContentBlock, ToolCall, ToolCallId, ToolDefinition, ToolResult,
    ToolResultDetails, TruncationDetails, TruncationLimit,
};

/// Tool definitions expose the exact JSON Schema consumed by provider adapters.
#[test]
fn tool_definition_preserves_its_input_schema() {
    let definition = ToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    };

    let value =
        serde_json::to_value(definition).expect("definition should serialize");

    assert_eq!(value["name"], "read");
    assert_eq!(value["parameters"]["required"][0], "path");
}

/// A tool result keeps the exact call id, ordered output blocks, and error state.
#[test]
fn tool_result_roundtrips_call_correlation() {
    let call_id = ToolCallId::try_from("call-1").expect("valid call id");
    let call = ToolCall {
        tool_call_id: call_id.clone(),
        name: "read".to_string(),
        arguments: serde_json::json!({ "path": "README.md" }),
    };
    let result = ToolResult::builder()
        .tool_call_id(call_id)
        .blocks(vec![ContentBlock::Text {
            text: "hello".to_string(),
        }])
        .is_error(false)
        .build();

    let call_value = serde_json::to_value(call).expect("call should serialize");
    let encoded =
        serde_json::to_string(&result).expect("result should serialize");
    let decoded: ToolResult =
        serde_json::from_str(&encoded).expect("result should deserialize");

    assert_eq!(call_value["tool_call_id"], "call-1");
    assert_eq!(decoded, result);
}

/// Tool results preserve pi-compatible structured truncation diagnostics.
#[test]
fn tool_result_roundtrips_typed_read_details() {
    let result = ToolResult::builder()
        .tool_call_id(ToolCallId::try_from("call-read").expect("call id"))
        .blocks(vec![ContentBlock::Text {
            text: "first page".to_string(),
        }])
        .is_error(false)
        .details(ToolResultDetails::Read {
            truncation: TruncationDetails::builder()
                .content("first page".to_string())
                .truncated(true)
                .truncated_by(TruncationLimit::Lines)
                .total_lines(2_500)
                .total_bytes(25_000)
                .output_lines(2_000)
                .output_bytes(20_000)
                .last_line_partial(false)
                .first_line_exceeds_limit(false)
                .max_lines(2_000)
                .max_bytes(51_200)
                .build(),
        })
        .build();

    let encoded = serde_json::to_value(&result).expect("serialize result");
    let decoded: ToolResult =
        serde_json::from_value(encoded.clone()).expect("deserialize result");

    assert_eq!(encoded["details"]["type"], "read");
    assert_eq!(encoded["details"]["truncation"]["truncated_by"], "lines");
    assert_eq!(decoded, result);
}

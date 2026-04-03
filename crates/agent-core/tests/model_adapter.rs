use agent_core::model::{ModelOutput, model_response_from_completion};
use llm::{
    completion::{AssistantContent, CompletionResponse},
    one_or_many::OneOrMany,
    usage::Usage,
};

#[test]
fn parses_tool_calls_and_text_from_completion_response() {
    let choice = OneOrMany::many(vec![
        AssistantContent::text("running echo"),
        AssistantContent::tool_call("call_1", "echo", serde_json::json!({"text": "hello"})),
    ])
    .unwrap();

    let response = CompletionResponse {
        choice,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 5,
            total_tokens: 16,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
        raw_response: (),
        message_id: Some("msg_1".to_string()),
    };

    let parsed = model_response_from_completion(response).unwrap();
    assert_eq!(parsed.message_id.as_deref(), Some("msg_1"));
    assert_eq!(parsed.usage.total_tokens, 16);

    match parsed.output {
        ModelOutput::ToolCalls { text, calls } => {
            assert_eq!(text.as_deref(), Some("running echo"));
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "echo");
            assert_eq!(calls[0].arguments["text"], "hello");
        }
        ModelOutput::Text(_) => panic!("expected tool calls"),
    }
}

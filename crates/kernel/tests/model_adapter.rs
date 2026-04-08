use futures_util::stream;
use kernel::{
    Result,
    model::{AgentModel, LlmAgentModel, ModelOutput, ModelRequest, model_response_from_completion},
};
use llm::{
    completion::{
        AssistantContent, CompletionModel, CompletionResponse, Message, ToolDefinition,
        message::ToolChoice,
        request::{CompletionError, CompletionRequest},
    },
    one_or_many::OneOrMany,
    streaming::{RawStreamingChoice, StreamingCompletionResponse},
    usage::Usage,
};
use serde::{Deserialize, Serialize};

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

#[derive(Clone)]
struct StubStreamingModel;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct StubStreamingResponse {
    usage: Usage,
}

impl llm::usage::GetTokenUsage for StubStreamingResponse {
    fn token_usage(&self) -> Option<Usage> {
        Some(self.usage)
    }
}

impl CompletionModel for StubStreamingModel {
    type Response = StubStreamingResponse;
    type StreamingResponse = StubStreamingResponse;
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!("streaming aggregation test should not use non-streaming completion")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let raw = stream::iter(vec![
            Ok(RawStreamingChoice::MessageId("msg_stream".to_string())),
            Ok(RawStreamingChoice::Message("running echo".to_string())),
            Ok(RawStreamingChoice::ToolCall(
                llm::streaming::RawStreamingToolCall::new(
                    "call_1".to_string(),
                    "echo".to_string(),
                    serde_json::json!({"text": "hello"}),
                ),
            )),
            Ok(RawStreamingChoice::FinalResponse(StubStreamingResponse {
                usage: Usage {
                    input_tokens: 9,
                    output_tokens: 7,
                    total_tokens: 16,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            })),
        ]);
        Ok(StreamingCompletionResponse::stream(Box::pin(raw)))
    }
}

#[tokio::test]
async fn llm_agent_model_aggregates_streaming_output_into_model_response() {
    let model = LlmAgentModel::new(StubStreamingModel);
    let response = model
        .complete(ModelRequest {
            system_prompt: Some("system".to_string()),
            messages: vec![Message::user("say hello")],
            tools: vec![ToolDefinition {
                name: "echo".to_string(),
                description: "Echo input".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            tool_choice: ToolChoice::Auto,
        })
        .await
        .unwrap();

    assert_eq!(response.message_id.as_deref(), Some("msg_stream"));
    assert_eq!(response.usage.total_tokens, 16);

    match response.output {
        ModelOutput::ToolCalls { text, calls } => {
            assert_eq!(text.as_deref(), Some("running echo"));
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "echo");
            assert_eq!(calls[0].arguments["text"], "hello");
        }
        ModelOutput::Text(_) => panic!("expected tool-call output"),
    }
}

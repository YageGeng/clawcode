use futures_util::{StreamExt, stream};
use kernel::{
    Result,
    model::{
        AgentModel, FactoryLlmAgentModel, LlmAgentModel, ModelOutput, ModelRequest, ResponseEvent,
        ResponseItem,
    },
};
use llm::{
    client::FinalCompletionResponse,
    completion::{
        AssistantContent, CompletionModel, CompletionResponse, Message, ToolDefinition,
        message::ToolChoice,
        request::{CompletionError, CompletionRequest},
    },
    one_or_many::OneOrMany,
    providers::{BoxLlmFuture, LlmCompletion},
    streaming::{RawStreamingChoice, StreamingCompletionResponse},
    usage::Usage,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct StubStreamingModel;

#[derive(Clone)]
struct StubFactoryModel;

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

impl LlmCompletion for StubFactoryModel {
    /// Returns a normalized text response for dynamic factory model tests.
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> BoxLlmFuture<'_, std::result::Result<CompletionResponse<serde_json::Value>, CompletionError>>
    {
        Box::pin(async {
            Ok(CompletionResponse {
                choice: OneOrMany::one(AssistantContent::text("factory complete")),
                usage: Usage::default(),
                raw_response: serde_json::json!({}),
                message_id: Some("msg_factory_complete".to_string()),
            })
        })
    }

    /// Returns a normalized stream for dynamic factory model tests.
    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> BoxLlmFuture<
        '_,
        std::result::Result<StreamingCompletionResponse<FinalCompletionResponse>, CompletionError>,
    > {
        Box::pin(async {
            let raw = stream::iter(vec![
                Ok(RawStreamingChoice::MessageId("msg_factory".to_string())),
                Ok(RawStreamingChoice::Message("factory hello".to_string())),
                Ok(RawStreamingChoice::FinalResponse(FinalCompletionResponse {
                    usage: Some(Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        total_tokens: 7,
                        cached_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                    }),
                })),
            ]);
            Ok(StreamingCompletionResponse::stream(Box::pin(raw)))
        })
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
            previous_response_id: None,
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

#[tokio::test]
async fn llm_agent_model_exposes_streaming_events_before_completion() {
    let model = LlmAgentModel::new(StubStreamingModel);
    let mut stream = model
        .stream(ModelRequest {
            system_prompt: Some("system".to_string()),
            messages: vec![Message::user("say hello")],
            tools: vec![ToolDefinition {
                name: "echo".to_string(),
                description: "Echo input".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            tool_choice: ToolChoice::Auto,
            previous_response_id: None,
        })
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }

    assert!(matches!(&events[0], ResponseEvent::Created));
    assert!(matches!(
        &events[1],
        ResponseEvent::OutputItemAdded(ResponseItem::Message { text }) if text.is_empty()
    ));
    assert!(matches!(
        &events[2],
        ResponseEvent::OutputTextDelta(text) if text == "running echo"
    ));
    assert!(matches!(
        &events[3],
        ResponseEvent::OutputItemAdded(ResponseItem::ToolCall {
            item_id,
            name,
            arguments,
            ..
        }) if item_id == "call_1" && name == "echo" && arguments.as_ref().is_some_and(|arguments| arguments["text"] == "hello")
    ));
    assert!(matches!(
        &events[4],
        ResponseEvent::OutputItemUpdated(ResponseItem::ToolCall {
            item_id,
            name,
            arguments,
            ..
        }) if item_id == "call_1" && name == "echo" && arguments.as_ref().is_some_and(|arguments| arguments["text"] == "hello")
    ));
    assert!(matches!(
        &events[5],
        ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
            item_id,
            name,
            arguments,
            ..
        }) if item_id == "call_1" && name == "echo" && arguments.as_ref().is_some_and(|arguments| arguments["text"] == "hello")
    ));
    assert!(matches!(
        &events[6],
        ResponseEvent::OutputItemDone(ResponseItem::Message { text })
            if text == "running echo"
    ));
    assert!(matches!(
        &events[7],
        ResponseEvent::Completed { usage, message_id }
            if usage.total_tokens == 16 && message_id.as_deref() == Some("msg_stream")
    ));
}

#[tokio::test]
async fn factory_llm_agent_model_adapts_dynamic_completion_model() {
    let model = FactoryLlmAgentModel::new(std::sync::Arc::new(StubFactoryModel));
    let response = model
        .complete(ModelRequest {
            system_prompt: Some("system".to_string()),
            messages: vec![Message::user("say hello")],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            previous_response_id: None,
        })
        .await
        .unwrap();

    assert_eq!(response.message_id.as_deref(), Some("msg_factory"));
    assert_eq!(response.usage.total_tokens, 7);
    assert_eq!(
        response.output,
        ModelOutput::Text("factory hello".to_string())
    );
}

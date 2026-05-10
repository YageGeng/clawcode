use http::Request;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{Level, enabled, info_span};

use crate::completion::{CompletionError, CompletionRequest, GetTokenUsage};
use crate::http_client::HttpClientExt;
use crate::json_utils::{self, merge};
use crate::providers::internal::openai_chat_completions_compatible::{
    self, CompatibleChoiceData, CompatibleChunk, CompatibleFinishReason, CompatibleStreamProfile,
    CompatibleToolCallChunk,
};
use crate::providers::openai::completion::{GenericCompletionModel, OpenAIRequestParams, Usage};
use crate::streaming;

// ================================================================
// OpenAI Completion Streaming API
// ================================================================
#[derive(Deserialize, Debug)]
pub(crate) struct StreamingFunction {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct StreamingToolCall {
    pub(crate) index: usize,
    pub(crate) id: Option<String>,
    pub(crate) function: StreamingFunction,
}

impl From<&StreamingToolCall> for CompatibleToolCallChunk {
    fn from(value: &StreamingToolCall) -> Self {
        Self {
            index: value.index,
            id: value.id.clone(),
            name: value.function.name.clone(),
            arguments: value.function.arguments.clone(),
        }
    }
}

#[derive(Deserialize, Debug)]
struct StreamingDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>, // This is not part of the official OpenAI API
    #[serde(default, deserialize_with = "json_utils::null_or_vec")]
    tool_calls: Vec<StreamingToolCall>,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    ToolCalls,
    Stop,
    ContentFilter,
    Length,
    #[serde(untagged)]
    Other(String), // This will handle the deprecated function_call
}

#[derive(Deserialize, Debug)]
struct StreamingChoice {
    delta: StreamingDelta,
    finish_reason: Option<FinishReason>,
}

#[derive(Deserialize, Debug)]
struct StreamingCompletionChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<StreamingChoice>,
    usage: Option<Usage>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StreamingCompletionResponse {
    pub usage: Usage,
}

impl GetTokenUsage for StreamingCompletionResponse {
    fn token_usage(&self) -> Option<crate::completion::Usage> {
        self.usage.token_usage()
    }
}

impl<Ext, H> GenericCompletionModel<Ext, H>
where
    crate::client::Client<Ext, H>: HttpClientExt + Clone + 'static,
    Ext: crate::client::Provider + Clone + 'static,
{
    pub(crate) async fn stream(
        &self,
        completion_request: CompletionRequest,
    ) -> Result<streaming::StreamingCompletionResponse<StreamingCompletionResponse>, CompletionError>
    {
        let request = super::CompletionRequest::try_from(OpenAIRequestParams {
            model: self.model.clone(),
            request: completion_request,
            strict_tools: self.strict_tools,
            tool_result_array_content: self.tool_result_array_content,
        })?;
        let request_messages = serde_json::to_string(&request.messages)?;
        let mut request_as_json = serde_json::to_value(request)?;

        request_as_json = merge(
            request_as_json,
            json!({"stream": true, "stream_options": {"include_usage": true}}),
        );

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "clawcode::completions",
                "OpenAI Chat Completions streaming completion request: {}",
                serde_json::to_string_pretty(&request_as_json)?
            );
        }

        let req_body = serde_json::to_vec(&request_as_json)?;

        let req = self
            .client
            .post("/chat/completions")?
            .body(req_body)
            .map_err(|e| CompletionError::HttpError(e.into()))?;

        let span = if tracing::Span::current().is_disabled() {
            info_span!(
                target: "clawcode::completions",
                "chat",
                gen_ai.operation.name = "chat",
                gen_ai.provider.name = "openai",
                gen_ai.request.model = self.model,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
                gen_ai.input.messages = request_messages,
                gen_ai.output.messages = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        let client = self.client.clone();

        tracing::Instrument::instrument(send_compatible_streaming_request(client, req), span).await
    }
}

#[derive(Clone, Copy)]
struct OpenAICompatibleProfile;

impl CompatibleStreamProfile for OpenAICompatibleProfile {
    type Usage = Usage;
    type Detail = ();
    type FinalResponse = StreamingCompletionResponse;

    fn normalize_chunk(
        &self,
        data: &str,
    ) -> Result<Option<CompatibleChunk<Self::Usage, Self::Detail>>, CompletionError> {
        let data = match serde_json::from_str::<StreamingCompletionChunk>(data) {
            Ok(data) => data,
            Err(error) => {
                tracing::error!(?error, message = data, "Failed to parse SSE message");
                return Ok(None);
            }
        };

        Ok(Some(
            openai_chat_completions_compatible::normalize_first_choice_chunk(
                data.id,
                data.model,
                data.usage,
                &data.choices,
                |choice| CompatibleChoiceData {
                    finish_reason: if choice.finish_reason == Some(FinishReason::ToolCalls) {
                        CompatibleFinishReason::ToolCalls
                    } else {
                        CompatibleFinishReason::Other
                    },
                    text: choice.delta.content.clone(),
                    reasoning: choice.delta.reasoning_content.clone(),
                    tool_calls: openai_chat_completions_compatible::tool_call_chunks(
                        &choice.delta.tool_calls,
                    ),
                    details: Vec::new(),
                },
            ),
        ))
    }

    fn build_final_response(&self, usage: Self::Usage) -> Self::FinalResponse {
        StreamingCompletionResponse { usage }
    }

    fn uses_distinct_tool_call_eviction(&self) -> bool {
        true
    }
}

pub async fn send_compatible_streaming_request<T>(
    http_client: T,
    req: Request<Vec<u8>>,
) -> Result<streaming::StreamingCompletionResponse<StreamingCompletionResponse>, CompletionError>
where
    T: HttpClientExt + Clone + 'static,
{
    openai_chat_completions_compatible::send_compatible_streaming_request(
        http_client,
        req,
        OpenAICompatibleProfile,
    )
    .await
}

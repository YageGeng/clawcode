use std::collections::HashMap;

use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use snafu::ResultExt;
use tracing::{Level, enabled, info_span};
use tracing_futures::Instrument;

use crate::completion::{
    ClientSnafu, CompletionError, CompletionRequest, ProviderSnafu, SerializeSnafu,
};
use crate::http_client::sse::{Event, GenericEventSource};
use crate::http_client::{HttpClientExt, HttpSnafu};
use crate::json_utils::{self, merge};
use crate::providers::deepseek::completion::{CompletionModel, Usage};
use crate::streaming::{self, RawStreamingChoice};
use crate::usage::GetTokenUsage;

// ================================================================
// DeepSeek Completion Streaming API
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

#[derive(Deserialize, Debug)]
struct StreamingDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning content delta from the deepseek-reasoner model.
    #[serde(default)]
    reasoning_content: Option<String>,
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
    Other(String),
}

#[derive(Deserialize, Debug)]
struct StreamingChoice {
    delta: StreamingDelta,
    finish_reason: Option<FinishReason>,
}

/// A single SSE chunk from the DeepSeek streaming API.
#[derive(Deserialize, Debug)]
struct StreamingCompletionChunk {
    choices: Vec<StreamingChoice>,
    usage: Option<Usage>,
}

/// Final streaming response, carrying DeepSeek-specific cache token statistics.
#[derive(Clone, Serialize, Deserialize)]
pub struct StreamingCompletionResponse {
    pub usage: Usage,
}

impl GetTokenUsage for StreamingCompletionResponse {
    fn token_usage(&self) -> Option<crate::usage::Usage> {
        let mut usage = crate::usage::Usage::new();
        usage.input_tokens = self.usage.prompt_tokens as u64;
        usage.output_tokens = self.usage.total_tokens as u64 - self.usage.prompt_tokens as u64;
        usage.total_tokens = self.usage.total_tokens as u64;
        usage.cached_input_tokens = self.usage.prompt_cache_hit_tokens as u64;
        usage.cache_creation_input_tokens = self.usage.prompt_cache_miss_tokens as u64;
        Some(usage)
    }
}

impl<T> CompletionModel<T>
where
    T: HttpClientExt + Clone + 'static,
{
    /// Sends a streaming completion request to the DeepSeek API.
    pub(crate) async fn stream(
        &self,
        completion_request: CompletionRequest,
    ) -> Result<streaming::StreamingCompletionResponse<StreamingCompletionResponse>, CompletionError>
    {
        let request = super::CompletionRequest::try_from((
            super::DeepSeekRequestParams {
                model: self.model.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                thinking_enabled: self.thinking_enabled,
            },
            completion_request,
        ))?;
        let request_messages = serde_json::to_string(&request.messages)
            .expect("Converting to JSON from a Rust struct shouldn't fail");
        let mut request_as_json = serde_json::to_value(request).expect("this should never fail");

        request_as_json = merge(
            request_as_json,
            json!({"stream": true, "stream_options": {"include_usage": true}}),
        );

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "DeepSeek Chat Completions streaming completion request: {}",
                serde_json::to_string_pretty(&request_as_json).context(SerializeSnafu{stage:"trace-deepseek"})?
            );
        }

        let req_body = serde_json::to_vec(&request_as_json).context(SerializeSnafu {
            stage: "serialize-deepseek",
        })?;

        let req = self
            .client
            .post("/chat/completions")
            .context(ClientSnafu {
                stage: "deepseek-post",
            })?
            .body(req_body)
            .context(HttpSnafu {
                stage: "deepseek-completion-http",
            })
            .context(ClientSnafu {
                stage: "deepseek-completion-body",
            })?;

        let span = if tracing::Span::current().is_disabled() {
            info_span!(
                target: "rig::completions",
                "chat",
                gen_ai.operation.name = "chat",
                gen_ai.provider.name = "deepseek",
                gen_ai.request.model = self.model,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = self.model,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cached_tokens = tracing::field::Empty,
                gen_ai.input.messages = request_messages,
                gen_ai.output.messages = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };

        let client = self.client.clone();

        tracing::Instrument::instrument(send_deepseek_streaming_request(client, req), span).await
    }
}

/// Sends a DeepSeek streaming request and processes SSE events into a streaming response.
pub async fn send_deepseek_streaming_request<T>(
    http_client: T,
    req: http::Request<Vec<u8>>,
) -> Result<streaming::StreamingCompletionResponse<StreamingCompletionResponse>, CompletionError>
where
    T: HttpClientExt + Clone + 'static,
{
    let span = tracing::Span::current();
    let mut event_source = GenericEventSource::new(http_client, req);

    let stream = stream! {
        let span = tracing::Span::current();

        let mut tool_calls: HashMap<usize, streaming::RawStreamingToolCall> = HashMap::new();
        let mut final_usage = None;

        while let Some(event_result) = event_source.next().await {
            match event_result {
                Ok(Event::Open) => {
                    tracing::trace!("SSE connection opened");
                    continue;
                }

                Ok(Event::Message(message)) => {
                    if message.data.trim().is_empty() || message.data == "[DONE]" {
                        continue;
                    }

                    let data = match serde_json::from_str::<StreamingCompletionChunk>(&message.data) {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(?error, message = message.data, "Failed to parse SSE message");
                            continue;
                        }
                    };

                    if let Some(usage) = data.usage {
                        final_usage = Some(usage);
                    }

                    let Some(choice) = data.choices.first() else {
                        tracing::debug!("There is no choice");
                        continue;
                    };
                    let delta = &choice.delta;

                    if !delta.tool_calls.is_empty() {
                        for tool_call in &delta.tool_calls {
                            let index = tool_call.index;

                            // Detect a new tool call at the same index with a different id (some proxy gateways send them this way).
                            if let Some(new_id) = &tool_call.id
                                && !new_id.is_empty()
                                && let Some(new_name) = &tool_call.function.name
                                && !new_name.is_empty()
                                && let Some(existing) = tool_calls.get(&index)
                                && !existing.id.is_empty()
                                && existing.id != *new_id
                                && !existing.name.is_empty()
                                && existing.name != *new_name
                            {
                                let evicted = tool_calls.remove(&index).expect("checked above");
                                yield Ok(streaming::RawStreamingChoice::ToolCall(evicted));
                            }

                            let existing_tool_call = tool_calls.entry(index).or_insert_with(streaming::RawStreamingToolCall::empty);

                            if let Some(id) = &tool_call.id && !id.is_empty() {
                                existing_tool_call.id = id.clone();
                            }

                            if let Some(name) = &tool_call.function.name && !name.is_empty() {
                                existing_tool_call.name = name.clone();
                                yield Ok(streaming::RawStreamingChoice::ToolCallDelta {
                                    id: existing_tool_call.id.clone(),
                                    internal_call_id: existing_tool_call.internal_call_id.clone(),
                                    content: streaming::ToolCallDeltaContent::Name(name.clone()),
                                });
                            }

                            if let Some(chunk) = &tool_call.function.arguments && !chunk.is_empty() {
                                let current_args = match &existing_tool_call.arguments {
                                    serde_json::Value::Null => String::new(),
                                    serde_json::Value::String(s) => s.clone(),
                                    v => v.to_string(),
                                };

                                let combined = format!("{current_args}{chunk}");

                                if combined.trim_start().starts_with('{') && combined.trim_end().ends_with('}') {
                                    match serde_json::from_str(&combined) {
                                        Ok(parsed) => existing_tool_call.arguments = parsed,
                                        Err(_) => existing_tool_call.arguments = serde_json::Value::String(combined),
                                    }
                                } else {
                                    existing_tool_call.arguments = serde_json::Value::String(combined);
                                }

                                yield Ok(streaming::RawStreamingChoice::ToolCallDelta {
                                    id: existing_tool_call.id.clone(),
                                    internal_call_id: existing_tool_call.internal_call_id.clone(),
                                    content: streaming::ToolCallDeltaContent::Delta(chunk.clone()),
                                });
                            }
                        }
                    }

                    // Reasoning content from the deepseek-reasoner model.
                    if let Some(reasoning) = &delta.reasoning_content && !reasoning.is_empty() {
                        yield Ok(streaming::RawStreamingChoice::ReasoningDelta {
                            id: None,
                            reasoning: reasoning.clone(),
                        });
                    }

                    if let Some(content) = &delta.content && !content.is_empty() {
                        yield Ok(streaming::RawStreamingChoice::Message(content.clone()));
                    }

                    if let Some(finish_reason) = &choice.finish_reason && *finish_reason == FinishReason::ToolCalls {
                        for (_idx, tool_call) in tool_calls.into_iter() {
                            yield Ok(streaming::RawStreamingChoice::ToolCall(tool_call));
                        }
                        tool_calls = HashMap::new();
                    }
                }
                Err(crate::http_client::Error::StreamEnded) => {
                    break;
                }
                Err(error) => {
                    tracing::error!(?error, "SSE error");
                    yield Err(ProviderSnafu{ msg: error.to_string() }.build());
                    break;
                }
            }
        }

        event_source.close();

        for (_idx, tool_call) in tool_calls.into_iter() {
            yield Ok(streaming::RawStreamingChoice::ToolCall(tool_call));
        }

        let final_usage = final_usage.unwrap_or_default();
        if !span.is_disabled() {
            span.record("gen_ai.usage.input_tokens", final_usage.prompt_tokens);
            span.record("gen_ai.usage.output_tokens", final_usage.total_tokens - final_usage.prompt_tokens);
            span.record("gen_ai.usage.cached_tokens", final_usage.prompt_cache_hit_tokens);
        }

        yield Ok(RawStreamingChoice::FinalResponse(StreamingCompletionResponse {
            usage: final_usage
        }));
    }.instrument(span);

    Ok(streaming::StreamingCompletionResponse::stream(Box::pin(
        stream,
    )))
}

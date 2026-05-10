//! The streaming module for the OpenAI Responses API.
//! Please see the `openai_streaming` or `openai_streaming_with_tools` example for more practical usage.
use crate::completion::{self, CompletionError, GetTokenUsage};
use crate::http_client::HttpClientExt;
use crate::http_client::sse::{Event, GenericEventSource};
use crate::message::ReasoningContent;
use crate::providers::openai::responses_api::{ReasoningSummary, ResponsesUsage};
use crate::streaming;
use crate::streaming::RawStreamingChoice;
use crate::wasm_compat::WasmCompatSend;
use async_stream::stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{Level, debug, enabled, info_span};
use tracing_futures::Instrument as _;

use super::{CompletionResponse, GenericResponsesCompletionModel, Output};

type StreamingRawChoice = RawStreamingChoice<StreamingCompletionResponse>;

// ================================================================
// OpenAI Responses Streaming API
// ================================================================

/// A streaming completion chunk.
/// Streaming chunks can come in one of two forms:
/// - A response chunk (where the completed response will have the total token usage)
/// - An item chunk commonly referred to as a delta. In the completions API this would be referred to as the message delta.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum StreamingCompletionChunk {
    Response(Box<ResponseChunk>),
    Delta(ItemChunk),
}

/// The final streaming response from the OpenAI Responses API.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StreamingCompletionResponse {
    /// Token usage
    pub usage: ResponsesUsage,
}

pub(crate) fn reasoning_choices_from_done_item(
    id: &str,
    summary: &[ReasoningSummary],
    encrypted_content: Option<&str>,
) -> Vec<RawStreamingChoice<StreamingCompletionResponse>> {
    let mut choices = summary
        .iter()
        .map(|reasoning_summary| match reasoning_summary {
            ReasoningSummary::SummaryText { text } => RawStreamingChoice::Reasoning {
                id: Some(id.to_owned()),
                content: ReasoningContent::Summary(text.to_owned()),
            },
        })
        .collect::<Vec<_>>();

    if let Some(encrypted_content) = encrypted_content {
        choices.push(RawStreamingChoice::Reasoning {
            id: Some(id.to_owned()),
            content: ReasoningContent::Encrypted(encrypted_content.to_owned()),
        });
    }

    choices
}

impl GetTokenUsage for StreamingCompletionResponse {
    fn token_usage(&self) -> Option<crate::completion::Usage> {
        self.usage.token_usage()
    }
}

/// A response chunk from OpenAI's response API.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResponseChunk {
    /// The response chunk type
    #[serde(rename = "type")]
    pub kind: ResponseChunkKind,
    /// The response itself
    pub response: CompletionResponse,
    /// The item sequence
    pub sequence_number: u64,
}

/// Response chunk type.
/// Renames are used to ensure that this type gets (de)serialized properly.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ResponseChunkKind {
    #[serde(rename = "response.created")]
    ResponseCreated,
    #[serde(rename = "response.in_progress")]
    ResponseInProgress,
    #[serde(rename = "response.completed")]
    ResponseCompleted,
    #[serde(rename = "response.failed")]
    ResponseFailed,
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete,
}

fn response_chunk_error_message(
    kind: &ResponseChunkKind,
    response: &CompletionResponse,
    provider_name: &str,
) -> Option<String> {
    match kind {
        ResponseChunkKind::ResponseFailed => Some(response_error_message(
            response.error.as_ref(),
            &format!("{provider_name} response stream returned a failed response"),
        )),
        ResponseChunkKind::ResponseIncomplete => {
            let reason = response
                .incomplete_details
                .as_ref()
                .map(|details| details.reason.as_str())
                .unwrap_or("unknown reason");

            Some(format!(
                "{provider_name} response stream was incomplete: {reason}"
            ))
        }
        _ => None,
    }
}

fn response_error_message(error: Option<&super::ResponseError>, fallback: &str) -> String {
    if let Some(error) = error {
        if error.code.is_empty() {
            error.message.clone()
        } else {
            format!("{}: {}", error.code, error.message)
        }
    } else {
        fallback.to_string()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ResponsesStreamOptions {
    Strict,
    StrictWithImmediateToolCalls,
}

impl ResponsesStreamOptions {
    pub(crate) const fn strict() -> Self {
        Self::Strict
    }

    pub(crate) const fn strict_with_immediate_tool_calls() -> Self {
        Self::StrictWithImmediateToolCalls
    }

    const fn errors_on_terminal_response(self) -> bool {
        true
    }

    const fn emits_completed_tool_calls_immediately(self) -> bool {
        matches!(self, Self::StrictWithImmediateToolCalls)
    }
}

pub(crate) fn parse_sse_completion_body(
    body: &str,
    provider_name: &str,
) -> Result<CompletionResponse, CompletionError> {
    let mut completed = None;
    let mut provider_error = None;

    for line in body.lines() {
        let data = line
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or_default();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        if let Ok(chunk) = serde_json::from_str::<StreamingCompletionChunk>(data) {
            if let StreamingCompletionChunk::Response(chunk) = chunk {
                let ResponseChunk { kind, response, .. } = *chunk;
                match kind {
                    ResponseChunkKind::ResponseCompleted => {
                        completed = Some(response);
                        break;
                    }
                    ResponseChunkKind::ResponseFailed | ResponseChunkKind::ResponseIncomplete => {
                        provider_error =
                            response_chunk_error_message(&kind, &response, provider_name);
                    }
                    _ => {}
                }
            }
            continue;
        }

        let value = match serde_json::from_str::<serde_json::Value>(data) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("response.completed") => {
                if let Some(response) = value.get("response") {
                    completed = Some(serde_json::from_value(response.clone())?);
                    break;
                }
            }
            Some("response.failed") | Some("response.incomplete") => {
                provider_error = value
                    .get("response")
                    .cloned()
                    .and_then(|response| {
                        serde_json::from_value::<CompletionResponse>(response).ok()
                    })
                    .and_then(|response| {
                        let kind = if value.get("type").and_then(serde_json::Value::as_str)
                            == Some("response.failed")
                        {
                            ResponseChunkKind::ResponseFailed
                        } else {
                            ResponseChunkKind::ResponseIncomplete
                        };
                        response_chunk_error_message(&kind, &response, provider_name)
                    })
                    .or_else(|| {
                        value
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .or_else(|| Some(data.to_string()));
            }
            Some("error") => {
                provider_error = value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| Some(data.to_string()));
            }
            _ => {}
        }
    }

    completed.ok_or_else(|| {
        CompletionError::ProviderError(
            provider_error.unwrap_or_else(|| {
                format!("{provider_name} stream did not yield response.completed")
            }),
        )
    })
}

struct RawChoiceAccumulator {
    final_usage: ResponsesUsage,
    tool_calls: Vec<StreamingRawChoice>,
    tool_call_internal_ids: std::collections::HashMap<String, String>,
}

impl RawChoiceAccumulator {
    fn new(initial_usage: ResponsesUsage) -> Self {
        Self {
            final_usage: initial_usage,
            tool_calls: Vec::new(),
            tool_call_internal_ids: std::collections::HashMap::new(),
        }
    }

    fn decode_item_chunk(
        &mut self,
        item: ItemChunkKind,
        options: ResponsesStreamOptions,
    ) -> Vec<StreamingRawChoice> {
        let mut immediate = Vec::new();

        match item {
            ItemChunkKind::OutputItemAdded(StreamingItemDoneOutput {
                item: Output::FunctionCall(func),
                ..
            }) => {
                let internal_call_id = self
                    .tool_call_internal_ids
                    .entry(func.id.clone())
                    .or_insert_with(|| nanoid::nanoid!())
                    .clone();
                immediate.push(streaming::RawStreamingChoice::ToolCallDelta {
                    id: func.id,
                    internal_call_id,
                    content: streaming::ToolCallDeltaContent::Name(func.name),
                });
            }
            ItemChunkKind::OutputItemDone(message) => {
                self.push_output_item_done(
                    message.item,
                    &mut immediate,
                    options.emits_completed_tool_calls_immediately(),
                );
            }
            ItemChunkKind::OutputTextDelta(delta) => {
                immediate.push(streaming::RawStreamingChoice::Message(delta.delta));
            }
            ItemChunkKind::ReasoningSummaryTextDelta(delta) => {
                immediate.push(streaming::RawStreamingChoice::ReasoningDelta {
                    id: None,
                    reasoning: delta.delta,
                });
            }
            ItemChunkKind::RefusalDelta(delta) => {
                immediate.push(streaming::RawStreamingChoice::Message(delta.delta));
            }
            ItemChunkKind::FunctionCallArgsDelta(delta) => {
                let internal_call_id = self
                    .tool_call_internal_ids
                    .entry(delta.item_id.clone())
                    .or_insert_with(|| nanoid::nanoid!())
                    .clone();
                immediate.push(streaming::RawStreamingChoice::ToolCallDelta {
                    id: delta.item_id,
                    internal_call_id,
                    content: streaming::ToolCallDeltaContent::Delta(delta.delta),
                });
            }
            _ => {}
        }

        immediate
    }

    fn record_response_chunk(
        &mut self,
        kind: ResponseChunkKind,
        response: CompletionResponse,
        provider_name: &str,
        options: ResponsesStreamOptions,
    ) -> Result<(), CompletionError> {
        match kind {
            ResponseChunkKind::ResponseCompleted => {
                if let Some(usage) = response.usage {
                    self.final_usage = usage;
                }
                Ok(())
            }
            ResponseChunkKind::ResponseFailed | ResponseChunkKind::ResponseIncomplete
                if options.errors_on_terminal_response() =>
            {
                let error_message = response_chunk_error_message(&kind, &response, provider_name)
                    .unwrap_or_else(|| {
                        format!(
                            "{provider_name} returned terminal response {:?} without an error message",
                            kind
                        )
                    });
                Err(CompletionError::ProviderError(error_message))
            }
            _ => Ok(()),
        }
    }

    fn push_output_item_done(
        &mut self,
        item: Output,
        immediate: &mut Vec<StreamingRawChoice>,
        emit_completed_tool_calls_immediately: bool,
    ) {
        match item {
            Output::FunctionCall(func) => {
                let internal_call_id = self
                    .tool_call_internal_ids
                    .entry(func.id.clone())
                    .or_insert_with(|| nanoid::nanoid!())
                    .clone();
                let tool_call =
                    streaming::RawStreamingToolCall::new(func.id, func.name, func.arguments)
                        .with_internal_call_id(internal_call_id)
                        .with_call_id(func.call_id);

                if emit_completed_tool_calls_immediately {
                    immediate.push(streaming::RawStreamingChoice::ToolCall(tool_call));
                } else {
                    self.tool_calls
                        .push(streaming::RawStreamingChoice::ToolCall(tool_call));
                }
            }
            Output::Reasoning {
                id,
                summary,
                encrypted_content,
                ..
            } => {
                immediate.extend(reasoning_choices_from_done_item(
                    &id,
                    &summary,
                    encrypted_content.as_deref(),
                ));
            }
            Output::Message(message) => {
                immediate.push(streaming::RawStreamingChoice::MessageId(message.id));
            }
            Output::Unknown => {}
        }
    }

    fn finish(mut self) -> Vec<StreamingRawChoice> {
        let mut choices = Vec::new();
        choices.append(&mut self.tool_calls);
        choices.push(RawStreamingChoice::FinalResponse(
            StreamingCompletionResponse {
                usage: self.final_usage,
            },
        ));
        choices
    }
}

pub(crate) fn raw_choices_from_sse_body(
    body: &str,
    initial_usage: ResponsesUsage,
    provider_name: &str,
) -> Result<Vec<StreamingRawChoice>, CompletionError> {
    let mut raw_choices = Vec::new();
    let mut accumulator = RawChoiceAccumulator::new(initial_usage);
    let options = ResponsesStreamOptions::strict();

    for line in body.lines() {
        let data = line
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or_default();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        if let Ok(chunk) = serde_json::from_str::<StreamingCompletionChunk>(data) {
            match chunk {
                StreamingCompletionChunk::Delta(chunk) => {
                    raw_choices.extend(accumulator.decode_item_chunk(chunk.data, options));
                }
                StreamingCompletionChunk::Response(chunk) => {
                    let ResponseChunk { kind, response, .. } = *chunk;
                    accumulator.record_response_chunk(kind, response, provider_name, options)?;
                }
            }
            continue;
        }

        let value = match serde_json::from_str::<serde_json::Value>(data) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("response.output_text.delta") | Some("response.refusal.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    raw_choices.push(streaming::RawStreamingChoice::Message(delta.to_owned()));
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    raw_choices.push(streaming::RawStreamingChoice::ReasoningDelta {
                        id: None,
                        reasoning: delta.to_owned(),
                    });
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = value
                    .get("item")
                    .cloned()
                    .and_then(|item| serde_json::from_value::<Output>(item).ok())
                    && let Output::FunctionCall(func) = item
                {
                    let internal_call_id = accumulator
                        .tool_call_internal_ids
                        .entry(func.id.clone())
                        .or_insert_with(|| nanoid::nanoid!())
                        .clone();
                    raw_choices.push(streaming::RawStreamingChoice::ToolCallDelta {
                        id: func.id,
                        internal_call_id,
                        content: streaming::ToolCallDeltaContent::Name(func.name),
                    });
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value
                    .get("item")
                    .cloned()
                    .and_then(|item| serde_json::from_value::<Output>(item).ok())
                {
                    accumulator.push_output_item_done(item, &mut raw_choices, false);
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let (Some(item_id), Some(delta)) = (
                    value.get("item_id").and_then(serde_json::Value::as_str),
                    value.get("delta").and_then(serde_json::Value::as_str),
                ) {
                    let internal_call_id = accumulator
                        .tool_call_internal_ids
                        .entry(item_id.to_owned())
                        .or_insert_with(|| nanoid::nanoid!())
                        .clone();
                    raw_choices.push(streaming::RawStreamingChoice::ToolCallDelta {
                        id: item_id.to_owned(),
                        internal_call_id,
                        content: streaming::ToolCallDeltaContent::Delta(delta.to_owned()),
                    });
                }
            }
            Some("response.completed") | Some("response.failed") | Some("response.incomplete") => {
                if let Some(response) = value.get("response").cloned() {
                    let response = serde_json::from_value::<CompletionResponse>(response)?;
                    let Some(kind) = (match value.get("type").and_then(serde_json::Value::as_str) {
                        Some("response.completed") => Some(ResponseChunkKind::ResponseCompleted),
                        Some("response.failed") => Some(ResponseChunkKind::ResponseFailed),
                        Some("response.incomplete") => Some(ResponseChunkKind::ResponseIncomplete),
                        _ => None,
                    }) else {
                        continue;
                    };
                    accumulator.record_response_chunk(kind, response, provider_name, options)?;
                }
            }
            Some("error") => {
                let message = value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(data);
                return Err(CompletionError::ProviderError(message.to_owned()));
            }
            _ => {}
        }
    }

    raw_choices.extend(accumulator.finish());
    Ok(raw_choices)
}

pub(crate) async fn completion_response_from_sse_body(
    body: &str,
    raw_response: CompletionResponse,
    provider_name: &str,
) -> Result<completion::CompletionResponse<CompletionResponse>, CompletionError> {
    let raw_choices = raw_choices_from_sse_body(
        body,
        raw_response
            .usage
            .clone()
            .unwrap_or_else(ResponsesUsage::new),
        provider_name,
    )?;
    let stream = futures::stream::iter(
        raw_choices
            .into_iter()
            .map(Ok::<_, CompletionError>)
            .collect::<Vec<_>>(),
    );
    let mut stream = crate::streaming::StreamingCompletionResponse::stream(Box::pin(stream));

    while let Some(item) = stream.next().await {
        item?;
    }

    if choice_is_empty(&stream.choice) {
        return Err(CompletionError::ResponseError(
            "Response contained no parts".to_owned(),
        ));
    }

    Ok(completion::CompletionResponse {
        usage: stream
            .response
            .as_ref()
            .and_then(GetTokenUsage::token_usage)
            .unwrap_or_else(|| usage_from_raw_response(&raw_response)),
        message_id: stream
            .message_id
            .clone()
            .or_else(|| message_id_from_response(&raw_response)),
        choice: stream.choice,
        raw_response,
    })
}

fn choice_is_empty(choice: &crate::OneOrMany<completion::AssistantContent>) -> bool {
    choice.iter().all(|content| match content {
        completion::AssistantContent::Text(text) => text.text.trim().is_empty(),
        completion::AssistantContent::Reasoning(reasoning) => reasoning.content.is_empty(),
        completion::AssistantContent::Image(_) => false,
        completion::AssistantContent::ToolCall(_) => false,
    })
}

fn message_id_from_response(response: &CompletionResponse) -> Option<String> {
    response.output.iter().find_map(|item| match item {
        Output::Message(message) => Some(message.id.clone()),
        _ => None,
    })
}

fn usage_from_raw_response(response: &CompletionResponse) -> completion::Usage {
    response
        .usage
        .as_ref()
        .and_then(GetTokenUsage::token_usage)
        .unwrap_or_default()
}

pub(crate) fn stream_from_event_source<HttpClient, RequestBody>(
    event_source: GenericEventSource<HttpClient, RequestBody>,
    span: tracing::Span,
    provider_name: &'static str,
) -> streaming::StreamingCompletionResponse<StreamingCompletionResponse>
where
    HttpClient: HttpClientExt + Clone + 'static,
    RequestBody: Into<bytes::Bytes> + Clone + WasmCompatSend + 'static,
{
    stream_from_event_source_with_options(
        event_source,
        span,
        provider_name,
        ResponsesStreamOptions::strict(),
    )
}

pub(crate) fn stream_from_event_source_with_options<HttpClient, RequestBody>(
    mut event_source: GenericEventSource<HttpClient, RequestBody>,
    span: tracing::Span,
    provider_name: &'static str,
    options: ResponsesStreamOptions,
) -> streaming::StreamingCompletionResponse<StreamingCompletionResponse>
where
    HttpClient: HttpClientExt + Clone + 'static,
    RequestBody: Into<bytes::Bytes> + Clone + WasmCompatSend + 'static,
{
    let stream = stream! {
        let mut accumulator = RawChoiceAccumulator::new(ResponsesUsage::new());
        let span = tracing::Span::current();

        let mut terminated_with_error = false;

        while let Some(event_result) = event_source.next().await {
            match event_result {
                Ok(Event::Open) => {
                    tracing::trace!("SSE connection opened");
                    continue;
                }
                Ok(Event::Message(evt)) => {
                    if evt.data.trim().is_empty() || evt.data == "[DONE]" {
                        continue;
                    }

                    let data = serde_json::from_str::<StreamingCompletionChunk>(&evt.data);

                    let Ok(data) = data else {
                        let Err(err) = data else {
                            continue;
                        };
                        debug!(
                            "Couldn't deserialize SSE data as StreamingCompletionChunk: {:?}",
                            err
                        );
                        continue;
                    };

                    match data {
                        StreamingCompletionChunk::Delta(chunk) => {
                            for choice in accumulator.decode_item_chunk(chunk.data, options) {
                                yield Ok(choice);
                            }
                        }
                        StreamingCompletionChunk::Response(chunk) => {
                            let ResponseChunk { kind, response, .. } = *chunk;
                            if matches!(kind, ResponseChunkKind::ResponseCompleted) {
                                span.record("gen_ai.response.id", response.id.as_str());
                                span.record("gen_ai.response.model", response.model.as_str());
                            }
                            if let Err(error) =
                                accumulator.record_response_chunk(kind, response, provider_name, options)
                            {
                                terminated_with_error = true;
                                yield Err(error);
                                break;
                            }
                        }
                    }
                }
                Err(crate::http_client::Error::StreamEnded) => {
                    event_source.close();
                }
                Err(error) => {
                    tracing::error!(?error, "SSE error");
                    terminated_with_error = true;
                    yield Err(CompletionError::ProviderError(error.to_string()));
                    break;
                }
            }
        }

        event_source.close();

        if terminated_with_error {
            return;
        }

        let final_usage = accumulator.final_usage.clone();

        for tool_call in accumulator.finish() {
            yield Ok(tool_call)
        }

        span.record("gen_ai.usage.input_tokens", final_usage.input_tokens);
        span.record("gen_ai.usage.output_tokens", final_usage.output_tokens);
        let cached_tokens = final_usage
            .input_tokens_details
            .as_ref()
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        span.record("gen_ai.usage.cache_read.input_tokens", cached_tokens);

    }
    .instrument(span);

    streaming::StreamingCompletionResponse::stream(Box::pin(stream))
}

/// An item message chunk from OpenAI's Responses API.
/// See
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ItemChunk {
    /// Item ID. Optional.
    pub item_id: Option<String>,
    /// The output index of the item from a given streamed response.
    pub output_index: u64,
    /// The item type chunk, as well as the inner data.
    #[serde(flatten)]
    pub data: ItemChunkKind,
}

/// The item chunk type from OpenAI's Responses API.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ItemChunkKind {
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded(StreamingItemDoneOutput),
    #[serde(rename = "response.output_item.done")]
    OutputItemDone(StreamingItemDoneOutput),
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded(ContentPartChunk),
    #[serde(rename = "response.content_part.done")]
    ContentPartDone(ContentPartChunk),
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta(DeltaTextChunk),
    #[serde(rename = "response.output_text.done")]
    OutputTextDone(OutputTextChunk),
    #[serde(rename = "response.refusal.delta")]
    RefusalDelta(DeltaTextChunk),
    #[serde(rename = "response.refusal.done")]
    RefusalDone(RefusalTextChunk),
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgsDelta(DeltaTextChunkWithItemId),
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgsDone(ArgsTextChunk),
    #[serde(rename = "response.reasoning_summary_part.added")]
    ReasoningSummaryPartAdded(SummaryPartChunk),
    #[serde(rename = "response.reasoning_summary_part.done")]
    ReasoningSummaryPartDone(SummaryPartChunk),
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta(SummaryTextChunk),
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryTextDone(SummaryTextChunk),
    /// Catch-all for unknown item chunk types (e.g., `web_search_call` events).
    /// This prevents unknown streaming events from breaking deserialization.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StreamingItemDoneOutput {
    pub sequence_number: u64,
    pub item: Output,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContentPartChunk {
    pub content_index: u64,
    pub sequence_number: u64,
    pub part: ContentPartChunkPart,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPartChunkPart {
    OutputText { text: String },
    SummaryText { text: String },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeltaTextChunk {
    pub content_index: u64,
    pub sequence_number: u64,
    pub delta: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeltaTextChunkWithItemId {
    pub item_id: String,
    pub content_index: u64,
    pub sequence_number: u64,
    pub delta: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OutputTextChunk {
    pub content_index: u64,
    pub sequence_number: u64,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RefusalTextChunk {
    pub content_index: u64,
    pub sequence_number: u64,
    pub refusal: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArgsTextChunk {
    pub content_index: u64,
    pub sequence_number: u64,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SummaryPartChunk {
    pub summary_index: u64,
    pub sequence_number: u64,
    pub part: SummaryPartChunkPart,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SummaryTextChunk {
    pub summary_index: u64,
    pub sequence_number: u64,
    pub delta: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SummaryPartChunkPart {
    SummaryText { text: String },
}

impl<Ext, H> GenericResponsesCompletionModel<Ext, H>
where
    crate::client::Client<Ext, H>:
        HttpClientExt + Clone + std::fmt::Debug + WasmCompatSend + 'static,
    Ext: crate::client::Provider + Clone + 'static,
    H: Clone + Default + std::fmt::Debug + WasmCompatSend + 'static,
{
    pub(crate) async fn stream(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<streaming::StreamingCompletionResponse<StreamingCompletionResponse>, CompletionError>
    {
        let mut request = self.create_completion_request(completion_request)?;
        request.stream = Some(true);

        if enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "OpenAI Responses streaming completion request: {}",
                serde_json::to_string_pretty(&request)?
            );
        }

        let body = serde_json::to_vec(&request)?;

        let req = self
            .client
            .post("/responses")?
            .body(body)
            .map_err(|e| CompletionError::HttpError(e.into()))?;

        // let request_builder = self.client.post_reqwest("/responses").json(&request);

        let span = if tracing::Span::current().is_disabled() {
            info_span!(
                target: "rig::completions",
                "chat_streaming",
                gen_ai.operation.name = "chat_streaming",
                gen_ai.provider.name = tracing::field::Empty,
                gen_ai.request.model = tracing::field::Empty,
                gen_ai.response.id = tracing::field::Empty,
                gen_ai.response.model = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            )
        } else {
            tracing::Span::current()
        };
        span.record("gen_ai.provider.name", "openai");
        span.record("gen_ai.request.model", &self.model);
        let client = self.client.clone();
        let event_source = GenericEventSource::new(client, req);

        Ok(stream_from_event_source(event_source, span, "OpenAI"))
    }
}

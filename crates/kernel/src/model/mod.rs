use async_trait::async_trait;
use futures_util::{Stream, StreamExt, stream};
use llm::completion::message::ToolChoice;
use llm::completion::{
    CompletionModel, Message, ToolDefinition,
    message::{Reasoning, ReasoningContent},
};
use llm::streaming::{StreamedAssistantContent, ToolCallDeltaContent};
use llm::usage::GetTokenUsage;
use snafu::{OptionExt, ResultExt};
use std::collections::HashSet;
use std::pin::Pin;
use tokio::sync::mpsc;

use crate::{
    Result,
    error::{MissingPromptSnafu, ModelSnafu},
    tools::ToolCallRequest,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    /// Provider response identifier used to continue a prior streamed exchange.
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelOutput {
    Text(String),
    ToolCalls {
        text: Option<String>,
        calls: Vec<ToolCallRequest>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub output: ModelOutput,
    pub usage: llm::usage::Usage,
    pub message_id: Option<String>,
}

pub type ResponseEventStream = Pin<Box<dyn Stream<Item = Result<ResponseEvent>> + Send>>;

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseItem {
    Message {
        text: String,
    },
    ToolCall {
        item_id: String,
        call_id: Option<String>,
        name: String,
        arguments: Option<serde_json::Value>,
        arguments_text: String,
    },
    Reasoning {
        id: Option<String>,
        summary: Vec<String>,
        content: Vec<String>,
        encrypted_content: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEvent {
    Created,
    OutputItemAdded(ResponseItem),
    OutputItemUpdated(ResponseItem),
    OutputTextDelta(String),
    ToolCallNameDelta {
        item_id: String,
        call_id: Option<String>,
        delta: String,
    },
    ToolCallArgumentsDelta {
        item_id: String,
        call_id: Option<String>,
        delta: String,
    },
    ReasoningSummaryDelta {
        id: Option<String>,
        delta: String,
        summary_index: i64,
    },
    ReasoningContentDelta {
        id: Option<String>,
        delta: String,
        content_index: i64,
    },
    OutputItemDone(ResponseItem),
    Completed {
        message_id: Option<String>,
        usage: llm::usage::Usage,
    },
}

impl ModelResponse {
    /// Builds a text-only response for runtime tests and adapters.
    pub fn text(text: impl Into<String>, usage: llm::usage::Usage) -> Self {
        Self {
            output: ModelOutput::Text(text.into()),
            usage,
            message_id: None,
        }
    }

    /// Builds a tool-call response for runtime tests and adapters.
    pub fn tool_calls(
        text: Option<String>,
        calls: Vec<ToolCallRequest>,
        usage: llm::usage::Usage,
    ) -> Self {
        Self {
            output: ModelOutput::ToolCalls { text, calls },
            usage,
            message_id: None,
        }
    }
}

/// Incrementally rebuilds the runtime model response while response events stream in.
#[derive(Debug, Default)]
pub struct ModelResponseBuilder {
    text: String,
    calls: Vec<ToolCallRequest>,
    usage: llm::usage::Usage,
    message_id: Option<String>,
}

impl ModelResponseBuilder {
    /// Creates an empty builder for one in-flight model turn.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one response event to the aggregated runtime response state.
    pub fn apply(&mut self, event: &ResponseEvent) {
        match event {
            ResponseEvent::OutputTextDelta(text) => self.text.push_str(text),
            ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
                item_id,
                call_id,
                name,
                arguments: Some(arguments),
                ..
            }) => {
                self.calls.push(ToolCallRequest {
                    id: item_id.clone(),
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
            ResponseEvent::Completed { usage, message_id } => {
                self.usage = *usage;
                self.message_id = message_id.clone();
            }
            ResponseEvent::Created
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::OutputItemUpdated(_)
            | ResponseEvent::ToolCallNameDelta { .. }
            | ResponseEvent::ToolCallArgumentsDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
            | ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
                arguments: None, ..
            })
            | ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. }) => {}
        }
    }

    /// Finalizes the aggregated state into the runtime response shape used by the loop.
    pub fn build(self) -> ModelResponse {
        let output = if self.calls.is_empty() {
            ModelOutput::Text(self.text)
        } else {
            ModelOutput::ToolCalls {
                text: (!self.text.is_empty()).then_some(self.text),
                calls: self.calls,
            }
        };

        ModelResponse {
            output,
            usage: self.usage,
            message_id: self.message_id,
        }
    }
}

#[derive(Debug, Default)]
struct ResponseEventMapper {
    message_text: String,
    message_started: bool,
    pending_reasoning: Option<PendingReasoning>,
    started_tool_calls: HashSet<String>,
    pending_tool_calls: std::collections::HashMap<String, PendingToolCall>,
}

#[derive(Debug, Clone)]
struct PendingReasoning {
    id: Option<String>,
    summary: Vec<String>,
    content: Vec<String>,
    encrypted_content: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    item_id: String,
    call_id: Option<String>,
    name: String,
    arguments_text: String,
}

impl PendingReasoning {
    /// Creates a pending reasoning accumulator for one response item.
    fn new(id: Option<String>) -> Self {
        Self {
            id,
            summary: Vec::new(),
            content: Vec::new(),
            encrypted_content: None,
        }
    }
}

impl PendingToolCall {
    /// Creates a pending tool call snapshot that can be updated from streaming deltas.
    fn new(item_id: String, call_id: Option<String>) -> Self {
        Self {
            item_id,
            call_id,
            name: String::new(),
            arguments_text: String::new(),
        }
    }

    /// Converts the pending snapshot into a response item with best-effort parsed arguments.
    fn to_response_item(&self) -> ResponseItem {
        ResponseItem::ToolCall {
            item_id: self.item_id.clone(),
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            arguments: serde_json::from_str(&self.arguments_text).ok(),
            arguments_text: self.arguments_text.clone(),
        }
    }
}

#[async_trait(?Send)]
pub trait AgentModel: Send + Sync {
    /// Runs one normalized model request and returns runtime-friendly output.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse>;

    /// Starts a kernel-owned response event stream for one model turn.
    async fn stream(&self, request: ModelRequest) -> Result<ResponseEventStream> {
        let response = self.complete(request).await?;
        Ok(Box::pin(stream::iter(
            response_events_from_model_response(response)
                .into_iter()
                .map(Ok),
        )))
    }
}

#[derive(Clone)]
pub struct LlmAgentModel<M> {
    inner: M,
}

/// Kernel model adapter backed by a config-driven LLM factory model.
pub type FactoryLlmAgentModel = LlmAgentModel<llm::providers::LlmCompletionModel>;

impl<M> LlmAgentModel<M> {
    /// Wraps a concrete `llm` completion model for runtime use.
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

#[async_trait(?Send)]
impl<M> AgentModel for LlmAgentModel<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::StreamingResponse: Send,
{
    /// Consumes the response event stream into the compact response used by compatibility callers.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let mut stream = self.stream(request).await?;
        let mut response = ModelResponseBuilder::new();

        while let Some(event) = stream.next().await {
            response.apply(&event?);
        }

        Ok(response.build())
    }

    /// Adapts `llm` streaming content into kernel-owned response events without touching `llm`.
    async fn stream(&self, request: ModelRequest) -> Result<ResponseEventStream> {
        let ModelRequest {
            system_prompt,
            messages,
            tools,
            tool_choice,
            previous_response_id,
        } = request;
        let mut messages = messages;
        let prompt = messages.pop().context(MissingPromptSnafu {
            stage: "agent-model-pop-prompt".to_string(),
        })?;

        let mut builder = self
            .inner
            .completion_request(prompt)
            .messages(messages)
            .tools(tools)
            .tool_choice(tool_choice);

        if let Some(system_prompt) = system_prompt {
            builder = builder.preamble(system_prompt);
        }
        if let Some(previous_response_id) = previous_response_id {
            // Responses-compatible providers will forward this field, while others safely ignore it.
            builder = builder.additional_params(serde_json::json!({
                "previous_response_id": previous_response_id,
            }));
        }

        let mut stream = builder.stream().await.context(ModelSnafu {
            stage: "agent-model-stream".to_string(),
        })?;
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut mapper = ResponseEventMapper::default();
            if tx.send(Ok(ResponseEvent::Created)).await.is_err() {
                return;
            }

            while let Some(item) = stream.next().await {
                let events = match mapper.map_stream_item(item) {
                    Ok(events) => events,
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                };

                for event in events {
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }

            let usage = stream
                .response
                .as_ref()
                .and_then(|response| response.token_usage())
                .unwrap_or_default();
            let events = mapper.finish(usage, stream.message_id.clone());

            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });

        Ok(Box::pin(stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

impl ResponseEventMapper {
    /// Converts one upstream stream item into response events or a kernel model error.
    #[allow(clippy::result_large_err)]
    fn map_stream_item<R>(
        &mut self,
        item: std::result::Result<StreamedAssistantContent<R>, llm::completion::CompletionError>,
    ) -> Result<Vec<ResponseEvent>>
    where
        R: Clone + Unpin,
    {
        item.map(|content| self.map_streamed_content(content))
            .map_err(|source| crate::Error::Model {
                source,
                stage: "agent-model-stream-next".to_string(),
            })
    }

    /// Converts one streamed provider item into kernel response events.
    fn map_streamed_content<R>(
        &mut self,
        content: StreamedAssistantContent<R>,
    ) -> Vec<ResponseEvent>
    where
        R: Clone + Unpin,
    {
        match content {
            StreamedAssistantContent::Text(text) => self.handle_text_delta(text.text),
            StreamedAssistantContent::ToolCallDelta {
                id,
                internal_call_id,
                content,
            } => self.handle_tool_call_delta(id, internal_call_id, content),
            StreamedAssistantContent::Reasoning(reasoning) => self.handle_reasoning(reasoning),
            StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                self.handle_reasoning_delta(id, reasoning)
            }
            StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id: _,
            } => self.handle_tool_call(tool_call),
            StreamedAssistantContent::Final(_) => Vec::new(),
        }
    }

    /// Emits a message item-added marker before the first visible assistant text chunk.
    fn handle_text_delta(&mut self, text: String) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        if !self.message_started {
            self.message_started = true;
            events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                text: String::new(),
            }));
        }
        self.message_text.push_str(&text);
        events.push(ResponseEvent::OutputTextDelta(text));
        events
    }

    /// Converts a full reasoning block into response deltas and a completed reasoning item.
    fn handle_reasoning(&mut self, reasoning: Reasoning) -> Vec<ResponseEvent> {
        let mut events = self.flush_reasoning_if_needed(reasoning.id.clone());
        let mut pending = PendingReasoning::new(reasoning.id.clone());
        events.push(ResponseEvent::OutputItemAdded(ResponseItem::Reasoning {
            id: pending.id.clone(),
            summary: Vec::new(),
            content: Vec::new(),
            encrypted_content: None,
        }));

        for content in reasoning.content {
            match content {
                ReasoningContent::Summary(text) => {
                    let index = pending.summary.len() as i64;
                    pending.summary.push(text.clone());
                    events.push(ResponseEvent::ReasoningSummaryDelta {
                        id: pending.id.clone(),
                        delta: text,
                        summary_index: index,
                    });
                }
                ReasoningContent::Text { text, .. } | ReasoningContent::Redacted { data: text } => {
                    let index = pending.content.len() as i64;
                    pending.content.push(text.clone());
                    events.push(ResponseEvent::ReasoningContentDelta {
                        id: pending.id.clone(),
                        delta: text,
                        content_index: index,
                    });
                }
                ReasoningContent::Encrypted(data) => {
                    pending.encrypted_content.get_or_insert(data);
                }
                _ => {}
            }
        }

        self.pending_reasoning = Some(pending);
        if let Some(item) = self.take_pending_reasoning_item() {
            events.push(ResponseEvent::OutputItemDone(item));
        }
        events
    }

    /// Converts reasoning text deltas into response events while preserving one in-flight item.
    fn handle_reasoning_delta(&mut self, id: Option<String>, text: String) -> Vec<ResponseEvent> {
        let mut events = self.flush_reasoning_if_needed(id.clone());
        let started_new = self.pending_reasoning.is_none();
        let pending = self
            .pending_reasoning
            .get_or_insert_with(|| PendingReasoning::new(id.clone()));

        if started_new {
            events.push(ResponseEvent::OutputItemAdded(ResponseItem::Reasoning {
                id: pending.id.clone(),
                summary: Vec::new(),
                content: Vec::new(),
                encrypted_content: None,
            }));
        }

        if pending.content.is_empty() {
            pending.content.push(String::new());
        }

        // `llm` only gives text chunks here, so we keep appending to the same content slot.
        pending.content[0].push_str(&text);
        events.push(ResponseEvent::ReasoningContentDelta {
            id,
            delta: text,
            content_index: 0,
        });
        events
    }

    /// Flushes trailing response items before the terminal completion event so the loop does not drop them.
    fn finish(
        &mut self,
        usage: llm::usage::Usage,
        message_id: Option<String>,
    ) -> Vec<ResponseEvent> {
        let mut events = Vec::new();

        if !self.message_text.is_empty() {
            events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                text: std::mem::take(&mut self.message_text),
            }));
            self.message_started = false;
        }

        if let Some(item) = self.take_pending_reasoning_item() {
            events.push(ResponseEvent::OutputItemDone(item));
        }

        events.push(ResponseEvent::Completed { message_id, usage });
        events
    }

    /// Flushes the previous reasoning item when the stream switches to a different reasoning id.
    fn flush_reasoning_if_needed(&mut self, next_id: Option<String>) -> Vec<ResponseEvent> {
        let should_flush = self
            .pending_reasoning
            .as_ref()
            .is_some_and(|pending| pending.id != next_id);

        if should_flush {
            self.take_pending_reasoning_item()
                .map(|item| vec![ResponseEvent::OutputItemDone(item)])
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Converts the pending reasoning accumulator into a completed response item.
    fn take_pending_reasoning_item(&mut self) -> Option<ResponseItem> {
        self.pending_reasoning
            .take()
            .map(|pending| ResponseItem::Reasoning {
                id: pending.id,
                summary: pending.summary,
                content: pending.content,
                encrypted_content: pending.encrypted_content,
            })
    }

    /// Emits tool-call added and delta events once per tool call item.
    fn handle_tool_call_delta(
        &mut self,
        item_id: String,
        internal_call_id: String,
        content: ToolCallDeltaContent,
    ) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        if self.started_tool_calls.insert(item_id.clone()) {
            events.push(ResponseEvent::OutputItemAdded(ResponseItem::ToolCall {
                item_id: item_id.clone(),
                call_id: Some(internal_call_id.clone()),
                name: String::new(),
                arguments: None,
                arguments_text: String::new(),
            }));
        }
        let snapshot = self
            .pending_tool_calls
            .entry(item_id.clone())
            .or_insert_with(|| {
                PendingToolCall::new(item_id.clone(), Some(internal_call_id.clone()))
            });

        match &content {
            ToolCallDeltaContent::Name(delta) => snapshot.name.push_str(delta),
            ToolCallDeltaContent::Delta(delta) => snapshot.arguments_text.push_str(delta),
        }

        events.extend(map_tool_call_delta(item_id, internal_call_id, content));
        events.push(ResponseEvent::OutputItemUpdated(
            snapshot.to_response_item(),
        ));
        events
    }

    /// Emits a completed tool-call item and backfills an item-added event when no deltas preceded it.
    fn handle_tool_call(
        &mut self,
        tool_call: llm::completion::message::ToolCall,
    ) -> Vec<ResponseEvent> {
        let item_id = tool_call.id;
        let call_id = tool_call.call_id;
        let name = tool_call.function.name;
        let arguments = tool_call.function.arguments;
        let arguments_text = arguments.to_string();
        let mut events = Vec::new();
        let item = ResponseItem::ToolCall {
            item_id: item_id.clone(),
            call_id: call_id.clone(),
            name: name.clone(),
            arguments: Some(arguments.clone()),
            arguments_text: arguments_text.clone(),
        };
        if self.started_tool_calls.insert(item_id.clone()) {
            events.push(ResponseEvent::OutputItemAdded(item.clone()));
        }
        self.pending_tool_calls.remove(&item_id);
        events.push(ResponseEvent::OutputItemUpdated(item));
        events.push(ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
            item_id,
            call_id,
            name,
            arguments: Some(arguments),
            arguments_text,
        }));
        events
    }
}

/// Converts an aggregated runtime response back into response events for non-streaming adapters.
fn response_events_from_model_response(response: ModelResponse) -> Vec<ResponseEvent> {
    let mut events = vec![ResponseEvent::Created];

    match response.output {
        ModelOutput::Text(text) => {
            if !text.is_empty() {
                events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                    text: String::new(),
                }));
                events.push(ResponseEvent::OutputTextDelta(text.clone()));
                events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                    text,
                }));
            }
        }
        ModelOutput::ToolCalls { text, calls } => {
            if let Some(text) = text
                && !text.is_empty()
            {
                events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                    text: String::new(),
                }));
                events.push(ResponseEvent::OutputTextDelta(text.clone()));
                events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                    text,
                }));
            }

            for call in calls {
                let arguments_text = call.arguments.to_string();
                let item = ResponseItem::ToolCall {
                    item_id: call.id.clone(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: Some(call.arguments.clone()),
                    arguments_text: arguments_text.clone(),
                };
                events.push(ResponseEvent::OutputItemAdded(item.clone()));
                events.push(ResponseEvent::OutputItemUpdated(item));
                events.push(ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
                    item_id: call.id,
                    call_id: call.call_id,
                    name: call.name,
                    arguments: Some(call.arguments),
                    arguments_text,
                }));
            }
        }
    }

    events.push(ResponseEvent::Completed {
        usage: response.usage,
        message_id: response.message_id,
    });
    events
}

/// Converts `llm` tool call delta payloads into kernel response events without mutating the `llm` layer.
fn map_tool_call_delta(
    item_id: String,
    internal_call_id: String,
    content: ToolCallDeltaContent,
) -> Vec<ResponseEvent> {
    match content {
        ToolCallDeltaContent::Name(delta) => vec![ResponseEvent::ToolCallNameDelta {
            item_id,
            call_id: Some(internal_call_id),
            delta,
        }],
        ToolCallDeltaContent::Delta(delta) => vec![ResponseEvent::ToolCallArgumentsDelta {
            item_id,
            call_id: Some(internal_call_id),
            delta,
        }],
    }
}

//! Streaming completion primitives shared by the remaining provider clients.

use crate::completion::{CompletionError, CompletionResponse, GetTokenUsage, Usage};
use crate::message::{AssistantContent, Reasoning, ReasoningContent, Text, ToolCall, ToolFunction};
use crate::one_or_many::OneOrMany;
use futures::stream::{AbortHandle, Abortable};
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll};
use tokio::sync::watch;

/// Re-exports the [`ToolCallDeltaContent`] enum from the protocol crate.
pub use protocol::ToolCallDeltaContent;

/// Control handle for pausing and resuming a streaming response.
pub struct PauseControl {
    pub(crate) paused_tx: watch::Sender<bool>,
    pub(crate) paused_rx: watch::Receiver<bool>,
}

impl PauseControl {
    /// Create a new pause controller in the running state.
    pub fn new() -> Self {
        let (paused_tx, paused_rx) = watch::channel(false);
        Self {
            paused_tx,
            paused_rx,
        }
    }

    /// Pause polling of the public stream until `resume()` is called.
    pub fn pause(&self) {
        let _ = self.paused_tx.send(true);
    }

    /// Resume polling after a pause.
    pub fn resume(&self) {
        let _ = self.paused_tx.send(false);
    }

    /// Returns whether the stream is currently paused.
    pub fn is_paused(&self) -> bool {
        *self.paused_rx.borrow()
    }
}

impl Default for PauseControl {
    fn default() -> Self {
        Self::new()
    }
}

/// A raw streaming choice emitted by a provider parser.
#[derive(Debug, Clone)]
pub enum RawStreamingChoice<R>
where
    R: Clone,
{
    /// A text chunk from a message response.
    Message(String),
    /// A tool call response (in its entirety).
    ToolCall(RawStreamingToolCall),
    /// A partial tool call response.
    ToolCallDelta {
        /// Provider-supplied tool call ID.
        id: String,
        /// Rig-generated unique identifier for this tool call.
        internal_call_id: String,
        content: ToolCallDeltaContent,
    },
    /// A full reasoning block.
    Reasoning {
        /// Provider-supplied reasoning block ID, when present.
        id: Option<String>,
        /// Complete reasoning content block.
        content: ReasoningContent,
    },
    /// A partial reasoning block.
    ReasoningDelta {
        /// Provider-supplied reasoning block ID, when present.
        id: Option<String>,
        /// Partial reasoning text.
        reasoning: String,
    },
    /// The final provider response object.
    FinalResponse(R),
    /// Provider-assigned message ID (for example an OpenAI Responses message ID).
    MessageId(String),
}

/// Describes a fully materialized streaming tool call.
#[derive(Debug, Clone)]
pub struct RawStreamingToolCall {
    /// Provider-supplied tool call ID.
    pub id: String,
    /// Rig-generated unique identifier for this tool call.
    pub internal_call_id: String,
    /// Provider-specific call ID used by some APIs for tool result correlation.
    pub call_id: Option<String>,
    /// Tool/function name.
    pub name: String,
    /// Parsed tool arguments.
    pub arguments: serde_json::Value,
    /// Optional provider signature associated with the tool call.
    pub signature: Option<String>,
    /// Additional provider-specific tool call metadata.
    pub additional_params: Option<serde_json::Value>,
}

impl RawStreamingToolCall {
    /// Create an empty tool-call accumulator.
    pub fn empty() -> Self {
        Self {
            id: String::new(),
            internal_call_id: nanoid::nanoid!(),
            call_id: None,
            name: String::new(),
            arguments: serde_json::Value::Null,
            signature: None,
            additional_params: None,
        }
    }

    /// Create a tool call with a generated internal ID.
    pub fn new(id: String, name: String, arguments: serde_json::Value) -> Self {
        Self {
            id,
            internal_call_id: nanoid::nanoid!(),
            call_id: None,
            name,
            arguments,
            signature: None,
            additional_params: None,
        }
    }

    /// Override the generated internal call ID.
    pub fn with_internal_call_id(mut self, internal_call_id: String) -> Self {
        self.internal_call_id = internal_call_id;
        self
    }

    /// Attach a provider-specific call ID.
    pub fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }

    /// Attach or clear a provider signature.
    pub fn with_signature(mut self, signature: Option<String>) -> Self {
        self.signature = signature;
        self
    }

    /// Attach provider-specific metadata.
    pub fn with_additional_params(mut self, additional_params: Option<serde_json::Value>) -> Self {
        self.additional_params = additional_params;
        self
    }
}

impl From<RawStreamingToolCall> for ToolCall {
    fn from(tool_call: RawStreamingToolCall) -> Self {
        ToolCall {
            id: tool_call.id,
            call_id: tool_call.call_id,
            function: ToolFunction {
                name: tool_call.name,
                arguments: tool_call.arguments,
            },
            signature: tool_call.signature,
            additional_params: tool_call.additional_params,
        }
    }
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
/// Provider stream of raw completion chunks on native targets.
pub type StreamingResult<R> =
    Pin<Box<dyn Stream<Item = Result<RawStreamingChoice<R>, CompletionError>> + Send>>;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
/// Provider stream of raw completion chunks on wasm targets.
pub type StreamingResult<R> =
    Pin<Box<dyn Stream<Item = Result<RawStreamingChoice<R>, CompletionError>>>>;

/// The response from a streaming completion request.
pub struct StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    pub(crate) inner: Abortable<StreamingResult<R>>,
    pub(crate) abort_handle: AbortHandle,
    pub(crate) pause_control: PauseControl,
    assistant_items: Vec<AssistantContent>,
    text_item_index: Option<usize>,
    reasoning_item_index: Option<usize>,
    /// The final aggregated message from the stream.
    pub choice: OneOrMany<AssistantContent>,
    /// The final response from the stream.
    pub response: Option<R>,
    pub final_response_yielded: AtomicBool,
    /// Provider-assigned message ID (for example an OpenAI Responses message ID).
    pub message_id: Option<String>,
}

impl<R> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    /// Wrap a provider stream and initialize aggregation state.
    pub fn stream(inner: StreamingResult<R>) -> StreamingCompletionResponse<R> {
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let abortable_stream = Abortable::new(inner, abort_registration);
        let pause_control = PauseControl::new();
        Self {
            inner: abortable_stream,
            abort_handle,
            pause_control,
            assistant_items: vec![],
            text_item_index: None,
            reasoning_item_index: None,
            choice: OneOrMany::one(AssistantContent::text("")),
            response: None,
            final_response_yielded: AtomicBool::new(false),
            message_id: None,
        }
    }

    /// Cancel the stream. Cancellation is surfaced as normal stream termination.
    pub fn cancel(&self) {
        self.abort_handle.abort();
    }

    /// Pause stream polling.
    pub fn pause(&self) {
        self.pause_control.pause();
    }

    /// Resume stream polling after a pause.
    pub fn resume(&self) {
        self.pause_control.resume();
    }

    /// Returns whether the stream is currently paused.
    pub fn is_paused(&self) -> bool {
        self.pause_control.is_paused()
    }

    fn append_text_chunk(&mut self, text: &str) {
        if let Some(index) = self.text_item_index
            && let Some(AssistantContent::Text(existing_text)) = self.assistant_items.get_mut(index)
        {
            existing_text.text.push_str(text);
            return;
        }

        self.assistant_items
            .push(AssistantContent::text(text.to_owned()));
        self.text_item_index = Some(self.assistant_items.len() - 1);
    }

    /// Accumulate reasoning deltas into the aggregated assistant output.
    fn append_reasoning_chunk(&mut self, id: &Option<String>, text: &str) {
        if let Some(index) = self.reasoning_item_index
            && let Some(AssistantContent::Reasoning(existing)) = self.assistant_items.get_mut(index)
            && let Some(ReasoningContent::Text {
                text: existing_text,
                ..
            }) = existing.content.last_mut()
        {
            existing_text.push_str(text);
            return;
        }

        self.assistant_items
            .push(AssistantContent::Reasoning(Reasoning {
                id: id.clone(),
                content: vec![ReasoningContent::Text {
                    text: text.to_string(),
                    signature: None,
                }],
            }));
        self.reasoning_item_index = Some(self.assistant_items.len() - 1);
    }
}

impl<R> From<StreamingCompletionResponse<R>> for CompletionResponse<Option<R>>
where
    R: Clone + Unpin + GetTokenUsage,
{
    fn from(value: StreamingCompletionResponse<R>) -> CompletionResponse<Option<R>> {
        CompletionResponse {
            choice: value.choice,
            usage: Usage::new(),
            raw_response: value.response,
            message_id: value.message_id,
        }
    }
}

impl<R> Stream for StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    type Item = Result<StreamedAssistantContent<R>, CompletionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let stream = self.get_mut();

        if stream.is_paused() {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        match Pin::new(&mut stream.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                if stream.assistant_items.is_empty() {
                    stream.assistant_items.push(AssistantContent::text(""));
                }

                if let Some(choice) =
                    OneOrMany::from_iter_optional(std::mem::take(&mut stream.assistant_items))
                {
                    stream.choice = choice;
                }

                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                if matches!(err, CompletionError::ProviderError(ref e) if e.to_string().contains("aborted"))
                {
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(Some(Ok(choice))) => match choice {
                RawStreamingChoice::Message(text) => {
                    stream.reasoning_item_index = None;
                    stream.append_text_chunk(&text);
                    Poll::Ready(Some(Ok(StreamedAssistantContent::text(&text))))
                }
                RawStreamingChoice::ToolCallDelta {
                    id,
                    internal_call_id,
                    content,
                } => Poll::Ready(Some(Ok(StreamedAssistantContent::ToolCallDelta {
                    id,
                    internal_call_id,
                    content,
                }))),
                RawStreamingChoice::Reasoning { id, content } => {
                    let reasoning = Reasoning {
                        id,
                        content: vec![content],
                    };
                    stream.text_item_index = None;
                    stream.reasoning_item_index = None;
                    stream
                        .assistant_items
                        .push(AssistantContent::Reasoning(reasoning.clone()));
                    Poll::Ready(Some(Ok(StreamedAssistantContent::Reasoning(reasoning))))
                }
                RawStreamingChoice::ReasoningDelta { id, reasoning } => {
                    stream.text_item_index = None;
                    stream.append_reasoning_chunk(&id, &reasoning);
                    Poll::Ready(Some(Ok(StreamedAssistantContent::ReasoningDelta {
                        id,
                        reasoning,
                    })))
                }
                RawStreamingChoice::ToolCall(raw_tool_call) => {
                    let internal_call_id = raw_tool_call.internal_call_id.clone();
                    let tool_call: ToolCall = raw_tool_call.into();
                    stream.text_item_index = None;
                    stream.reasoning_item_index = None;
                    stream
                        .assistant_items
                        .push(AssistantContent::ToolCall(tool_call.clone()));
                    Poll::Ready(Some(Ok(StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    })))
                }
                RawStreamingChoice::FinalResponse(response) => {
                    if stream
                        .final_response_yielded
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        stream.poll_next_unpin(cx)
                    } else {
                        stream.response = Some(response.clone());
                        stream
                            .final_response_yielded
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        Poll::Ready(Some(Ok(StreamedAssistantContent::final_response(response))))
                    }
                }
                RawStreamingChoice::MessageId(id) => {
                    stream.message_id = Some(id);
                    stream.poll_next_unpin(cx)
                }
            },
        }
    }
}

/// Describes a streamed provider response item.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum StreamedAssistantContent<R> {
    /// Text delta emitted by the assistant.
    Text(Text),
    /// Complete tool call emitted by the assistant.
    ToolCall {
        tool_call: ToolCall,
        /// Rig-generated unique identifier for this tool call.
        internal_call_id: String,
    },
    /// Partial tool call data emitted by the assistant.
    ToolCallDelta {
        /// Provider-supplied tool call ID.
        id: String,
        /// Rig-generated unique identifier for this tool call.
        internal_call_id: String,
        content: ToolCallDeltaContent,
    },
    /// Complete reasoning block emitted by the assistant.
    Reasoning(Reasoning),
    /// Partial reasoning text emitted by the assistant.
    ReasoningDelta {
        /// Provider-supplied reasoning block ID, when present.
        id: Option<String>,
        /// Partial reasoning text.
        reasoning: String,
    },
    /// Final provider response object.
    Final(R),
}

impl<R> StreamedAssistantContent<R>
where
    R: Clone + Unpin,
{
    /// Create a text stream item.
    pub fn text(text: &str) -> Self {
        Self::Text(Text {
            text: text.to_string(),
        })
    }

    /// Create a final response stream item.
    pub fn final_response(res: R) -> Self {
        Self::Final(res)
    }
}

use std::{
    pin::Pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll},
};

use futures::{
    Stream, StreamExt,
    stream::{AbortHandle, Abortable},
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{
    completion::{
        AssistantContent, CompletionResponse,
        message::{Reasoning, ReasoningContent, Text, ToolCall, ToolFunction},
        request::CompletionError,
    },
    one_or_many::OneOrMany,
    usage::{GetTokenUsage, Usage},
};

/// Control for pausing and resuming a streaming response
pub struct PauseControl {
    pub(crate) paused_tx: watch::Sender<bool>,
    pub(crate) paused_rx: watch::Receiver<bool>,
}

impl PauseControl {
    pub fn new() -> Self {
        let (paused_tx, paused_rx) = watch::channel(false);
        Self {
            paused_tx,
            paused_rx,
        }
    }

    pub fn pause(&self) {
        self.paused_tx.send(true).unwrap();
    }

    pub fn resume(&self) {
        self.paused_tx.send(false).unwrap();
    }

    pub fn is_paused(&self) -> bool {
        *self.paused_rx.borrow()
    }
}

impl Default for PauseControl {
    fn default() -> Self {
        Self::new()
    }
}

pub type StreamingResult<R> =
    Pin<Box<dyn Stream<Item = Result<RawStreamingChoice<R>, CompletionError>> + Send>>;

/// Enum representing a streaming chunk from the model
#[derive(Debug, Clone)]
pub enum RawStreamingChoice<R>
where
    R: Clone,
{
    /// A text chunk from a message response
    Message(String),

    /// A tool call response (in its entirety)
    ToolCall(RawStreamingToolCall),
    /// A tool call partial/delta
    ToolCallDelta {
        /// Provider-supplied tool call ID.
        id: String,
        /// Rig-generated unique identifier for this tool call.
        internal_call_id: String,
        content: ToolCallDeltaContent,
    },
    /// A reasoning (in its entirety)
    Reasoning {
        id: Option<String>,
        content: ReasoningContent,
    },
    /// A reasoning partial/delta
    ReasoningDelta {
        id: Option<String>,
        reasoning: String,
    },

    /// The final response object, must be yielded if you want the
    /// `response` field to be populated on the `StreamingCompletionResponse`
    FinalResponse(R),

    /// Provider-assigned message ID (e.g. OpenAI Responses API `msg_` ID).
    /// Captured silently into `StreamingCompletionResponse::message_id`.
    MessageId(String),
}

/// Describes a streaming tool call response (in its entirety)
#[derive(Debug, Clone)]
pub struct RawStreamingToolCall {
    /// Provider-supplied tool call ID.
    pub id: String,
    /// Rig-generated unique identifier for this tool call.
    pub internal_call_id: String,
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
    pub signature: Option<String>,
    pub additional_params: Option<serde_json::Value>,
}

impl RawStreamingToolCall {
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

    pub fn with_internal_call_id(mut self, internal_call_id: String) -> Self {
        self.internal_call_id = internal_call_id;
        self
    }

    pub fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }

    pub fn with_signature(mut self, signature: Option<String>) -> Self {
        self.signature = signature;
        self
    }

    pub fn with_additional_params(mut self, additional_params: Option<serde_json::Value>) -> Self {
        self.additional_params = additional_params;
        self
    }
}

/// The content of a tool call delta - either the tool name or argument data
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum ToolCallDeltaContent {
    Name(String),
    Delta(String),
}

/// The response from a streaming completion request;
/// message and response are populated at the end of the
/// `inner` stream.
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
    /// The final aggregated message from the stream
    /// contains all text and tool calls generated
    pub choice: OneOrMany<AssistantContent>,
    /// The final response from the stream, may be `None`
    /// if the provider didn't yield it during the stream
    pub response: Option<R>,
    pub final_response_yielded: AtomicBool,
    /// Provider-assigned message ID (e.g. OpenAI Responses API `msg_` ID).
    pub message_id: Option<String>,
}

impl<R> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
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

    pub fn cancel(&self) {
        self.abort_handle.abort();
    }

    pub fn pause(&self) {
        self.pause_control.pause();
    }

    pub fn resume(&self) {
        self.pause_control.resume();
    }

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

    /// Accumulate streaming reasoning delta text into assistant_items.
    /// Providers that only emit ReasoningDelta (not full Reasoning blocks)
    /// need this so the aggregated response includes reasoning content.
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
            usage: Usage::new(), // Usage is not tracked in streaming responses
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
                // This is run at the end of the inner stream to collect all tokens into
                // a single unified `Message`.
                if stream.assistant_items.is_empty() {
                    stream.assistant_items.push(AssistantContent::text(""));
                }

                stream.choice = OneOrMany::many(std::mem::take(&mut stream.assistant_items))
                    .expect("There should be at least one assistant message");

                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                if matches!(err, CompletionError::Provider{msg: ref id } if id.contains("aborted"))
                {
                    return Poll::Ready(None); // Treat cancellation as stream termination
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
                    // Full reasoning block supersedes any delta accumulation
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
                        // Set the final response field and return the next item in the stream
                        stream.response = Some(response.clone());
                        stream
                            .final_response_yielded
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        let final_response = StreamedAssistantContent::final_response(response);
                        Poll::Ready(Some(Ok(final_response)))
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

/// Describes responses from a streamed provider response which is either text, a tool call or a final usage response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum StreamedAssistantContent<R> {
    Text(Text),
    ToolCall {
        tool_call: ToolCall,
        /// Rig-generated unique identifier for this tool call.
        /// Use this to correlate with ToolCallDelta events.
        internal_call_id: String,
    },
    ToolCallDelta {
        /// Provider-supplied tool call ID.
        id: String,
        /// Rig-generated unique identifier for this tool call.
        internal_call_id: String,
        content: ToolCallDeltaContent,
    },
    Reasoning(Reasoning),
    ReasoningDelta {
        id: Option<String>,
        reasoning: String,
    },
    Final(R),
}

impl<R> StreamedAssistantContent<R>
where
    R: Clone + Unpin,
{
    pub fn text(text: &str) -> Self {
        Self::Text(Text {
            text: text.to_string(),
        })
    }

    pub fn final_response(res: R) -> Self {
        Self::Final(res)
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

//! Unified LLM event and completion types shared by the factory and its callers.

use serde::Serialize;

use crate::completion::{CompletionError, GetTokenUsage, Usage};
use crate::message::{AssistantContent, Reasoning, Text, ToolCall};
use crate::one_or_many::OneOrMany;
use crate::streaming::{StreamedAssistantContent, ToolCallDeltaContent};

// ── DynLlmStream ────────────────────────────────────────────────────────────

/// Boxed, provider-agnostic streaming handle.
///
/// Built on top of [`WasmCompatStream`](crate::wasm_compat::WasmCompatStream)
/// so that `Send` is included on native targets and dropped on wasm32 with
/// the `wasm` feature.
pub type DynLlmStream =
    crate::wasm_compat::WasmCompatStream<Result<LlmStreamEvent, CompletionError>>;

// ── LlmStreamEvent ──────────────────────────────────────────────────────────

/// Unified streaming event emitted by any LLM provider.
///
/// Provider-agnostic variants (Text, ToolCall, …) are strongly typed.
/// [`LlmStreamEvent::Final`] carries the provider-specific raw response
/// serialized as `serde_json::Value` plus optional token usage.
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
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
        /// Whether this delta is canonical provider reasoning that can be retained
        /// for future requests. OpenAI summary deltas are display-only previews;
        /// only completed reasoning items with `reasoning.encrypted_content` are
        /// valid for stateless multi-turn replay when `store=false`.
        /// See: https://developers.openai.com/api/reference/resources/responses/methods/create
        replayable: bool,
    },
    /// Provider-specific raw response with optional token usage.
    Final {
        /// Serialized provider response. Use `serde_json::from_value` to
        /// deserialize back to the provider-specific type.
        raw: serde_json::Value,
        /// Token usage extracted from the final response, when available.
        usage: Option<Usage>,
    },
}

/// Convert a provider-specific `StreamedAssistantContent<T>` into the
/// unified `LlmStreamEvent`. `T` must be serializable and implement
/// [`GetTokenUsage`] so that the [`Final`](LlmStreamEvent::Final) variant
/// can carry both the raw JSON and the usage breakdown.
///
/// # Errors
///
/// Returns [`CompletionError::JsonError`] when serializing the `Final`
/// response fails.
impl<T> TryFrom<StreamedAssistantContent<T>> for LlmStreamEvent
where
    T: GetTokenUsage + Serialize,
{
    type Error = CompletionError;

    fn try_from(item: StreamedAssistantContent<T>) -> Result<Self, Self::Error> {
        match item {
            StreamedAssistantContent::Text(t) => Ok(LlmStreamEvent::Text(t)),
            StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id,
            } => Ok(LlmStreamEvent::ToolCall {
                tool_call,
                internal_call_id,
            }),
            StreamedAssistantContent::ToolCallDelta {
                id,
                internal_call_id,
                content,
            } => Ok(LlmStreamEvent::ToolCallDelta {
                id,
                internal_call_id,
                content,
            }),
            StreamedAssistantContent::Reasoning(r) => Ok(LlmStreamEvent::Reasoning(r)),
            StreamedAssistantContent::ReasoningDelta {
                id,
                reasoning,
                replayable,
            } => Ok(LlmStreamEvent::ReasoningDelta {
                id,
                reasoning,
                replayable,
            }),
            StreamedAssistantContent::Final(r) => Ok(LlmStreamEvent::Final {
                raw: serde_json::to_value(&r).map_err(CompletionError::JsonError)?,
                usage: r.token_usage(),
            }),
        }
    }
}

// ── LlmCompletion ───────────────────────────────────────────────────────────

/// Unified non-streaming result returned by dynamic LLM handles.
#[derive(Debug)]
pub struct LlmCompletion {
    /// Aggregated assistant content (text, tool calls, reasoning).
    pub choice: OneOrMany<AssistantContent>,
    /// Token usage reported by the provider.
    pub usage: Usage,
    /// Serialized provider-specific raw response.
    pub raw_response: serde_json::Value,
    /// Provider-assigned message ID, when available.
    pub message_id: Option<String>,
}

//! Unified LLM event and completion types shared by the factory and its callers.

use crate::completion::{
    CompletionError, GetFinishReason, GetTokenUsage, ProviderFinishReason,
    Usage,
};
use crate::message::{AssistantContent, Reasoning, Text, ToolCall};
use crate::one_or_many::OneOrMany;
use crate::streaming::{StreamedAssistantContent, ToolCallDeltaContent};

// ── DynLlmStream ────────────────────────────────────────────────────────────

/// Boxed, provider-agnostic streaming handle.
///
/// Built on top of [`WasmCompatStream`](crate::wasm_compat::WasmCompatStream)
/// so that `Send` is included on native targets and dropped on wasm32 with
/// the `wasm` feature.
pub type DynLlmStream = crate::wasm_compat::WasmCompatStream<
    Result<LlmStreamEvent, CompletionError>,
>;

// ── LlmStreamEvent ──────────────────────────────────────────────────────────

/// Unified streaming event emitted by any LLM provider.
///
/// Provider-agnostic variants (Text, ToolCall, …) are strongly typed.
/// [`LlmStreamEvent::Final`] carries only normalized public terminal data.
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
    /// Provider terminal response with normalized reason and token usage.
    Final(ProviderFinal),
}

/// Provider terminal data retained at the public factory boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFinal {
    /// Provider-neutral terminal reason.
    pub finish_reason: ProviderFinishReason,
    /// Provider-native terminal reason retained for diagnostics.
    pub raw_finish_reason: Option<String>,
    /// Complete token usage extracted from the final response.
    pub usage: Option<Usage>,
}

/// Convert a provider-specific `StreamedAssistantContent<T>` into the
/// unified `LlmStreamEvent`. `T` exposes terminal reason and token usage through
/// traits so provider-private response JSON never crosses the factory boundary.
impl<T> TryFrom<StreamedAssistantContent<T>> for LlmStreamEvent
where
    T: GetFinishReason + GetTokenUsage,
{
    type Error = CompletionError;

    fn try_from(
        item: StreamedAssistantContent<T>,
    ) -> Result<Self, Self::Error> {
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
            StreamedAssistantContent::Reasoning(r) => {
                Ok(LlmStreamEvent::Reasoning(r))
            }
            StreamedAssistantContent::ReasoningDelta {
                id,
                reasoning,
                replayable,
            } => Ok(LlmStreamEvent::ReasoningDelta {
                id,
                reasoning,
                replayable,
            }),
            StreamedAssistantContent::Final(response) => {
                Ok(LlmStreamEvent::Final(ProviderFinal {
                    finish_reason: response.finish_reason(),
                    raw_finish_reason: response.raw_finish_reason(),
                    usage: response.token_usage(),
                }))
            }
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

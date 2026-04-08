use async_trait::async_trait;
use futures_util::StreamExt;
use llm::completion::message::ToolChoice;
use llm::completion::{
    AssistantContent, CompletionModel, CompletionResponse, Message, ToolDefinition,
};
use snafu::{OptionExt, ResultExt};

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

#[async_trait(?Send)]
pub trait AgentModel: Send + Sync {
    /// Runs one normalized model request and returns runtime-friendly output.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse>;
}

#[derive(Clone)]
pub struct LlmAgentModel<M> {
    inner: M,
}

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
{
    /// Converts runtime requests into `llm` completion requests and normalizes the response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let mut messages = request.messages;
        let prompt = messages.pop().context(MissingPromptSnafu {
            stage: "agent-model-pop-prompt".to_string(),
        })?;

        let mut builder = self
            .inner
            .completion_request(prompt)
            .messages(messages)
            .tools(request.tools)
            .tool_choice(request.tool_choice);

        if let Some(system_prompt) = request.system_prompt {
            builder = builder.preamble(system_prompt);
        }

        let mut stream = builder.stream().await.context(ModelSnafu {
            stage: "agent-model-stream".to_string(),
        })?;

        while let Some(item) = stream.next().await {
            item.context(ModelSnafu {
                stage: "agent-model-stream-next".to_string(),
            })?;
        }

        model_response_from_stream(stream)
    }
}

/// Normalizes a completed streaming response into the runtime's smaller output model.
pub fn model_response_from_stream<T>(
    response: llm::streaming::StreamingCompletionResponse<T>,
) -> Result<ModelResponse>
where
    T: Clone + Unpin + llm::usage::GetTokenUsage,
{
    let message_id = response.message_id;
    let usage = response
        .response
        .as_ref()
        .and_then(|raw| raw.token_usage())
        .unwrap_or_default();

    let completion = CompletionResponse {
        choice: response.choice,
        usage,
        raw_response: (),
        message_id,
    };

    model_response_from_completion(completion)
}

/// Normalizes a raw `llm` completion response into the runtime's smaller output model.
pub fn model_response_from_completion<T>(response: CompletionResponse<T>) -> Result<ModelResponse> {
    let CompletionResponse {
        choice,
        usage,
        raw_response: _,
        message_id,
    } = response;

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for content in choice.into_iter() {
        match content {
            AssistantContent::Text(text) => text_parts.push(text.text),
            AssistantContent::ToolCall(call) => tool_calls.push(ToolCallRequest {
                id: call.id,
                call_id: call.call_id,
                name: call.function.name,
                arguments: call.function.arguments,
            }),
            AssistantContent::Reasoning(_) | AssistantContent::Image(_) => {}
        }
    }

    let output = if tool_calls.is_empty() {
        ModelOutput::Text(text_parts.join("\n"))
    } else {
        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };
        ModelOutput::ToolCalls {
            text,
            calls: tool_calls,
        }
    };

    Ok(ModelResponse {
        output,
        usage,
        message_id,
    })
}

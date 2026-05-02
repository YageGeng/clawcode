use async_trait::async_trait;

use crate::{
    Result,
    collaboration::AgentRuntimeContext,
    context::{ToolCallRequest, ToolContext, ToolInvocation, ToolMetadata, ToolOutput},
    spec::ToolPromptMetadata,
};

/// Defines the minimal behavior required for a tool handler.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Returns the stable tool name exposed to the model.
    fn name(&self) -> &'static str;

    /// Returns the human-readable tool description.
    fn description(&self) -> &'static str;

    /// Returns the JSON schema for tool arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Returns runtime metadata such as approval and timeout requirements.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    /// Returns the one-line prompt snippet used in default system-prompt tool listings.
    fn prompt_snippet(&self) -> Option<&'static str> {
        None
    }

    /// Returns prompt guidelines contributed by this tool to the default system prompt.
    fn prompt_guidelines(&self) -> &'static [&'static str] {
        &[]
    }

    /// Builds the provider-facing tool definition.
    fn definition(&self) -> llm::completion::ToolDefinition {
        llm::completion::ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// Builds prompt metadata exposed through the visible tool spec.
    fn prompt_metadata(&self) -> ToolPromptMetadata {
        ToolPromptMetadata {
            prompt_snippet: self.prompt_snippet(),
            prompt_guidelines: self.prompt_guidelines(),
        }
    }

    /// Returns an optional visibility predicate evaluated per agent context.
    /// Tools that should be hidden at certain depths or for certain agents
    /// override this to return a predicate. `None` means always visible.
    fn visible_when(&self) -> Option<fn(&AgentRuntimeContext) -> bool> {
        None
    }

    /// Executes one normalized tool invocation.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput>;

    /// Preserves the legacy execute entrypoint while routing everything through invocations.
    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        self.handle(ToolInvocation::from_call_request(
            ToolCallRequest::new("direct-execute", self.name(), args),
            context,
        ))
        .await
    }
}

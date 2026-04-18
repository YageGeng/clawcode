use async_trait::async_trait;

use crate::{
    Result,
    context::{ToolCallRequest, ToolContext, ToolInvocation, ToolMetadata, ToolOutput},
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

    /// Builds the provider-facing tool definition.
    fn definition(&self) -> llm::completion::ToolDefinition {
        llm::completion::ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
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

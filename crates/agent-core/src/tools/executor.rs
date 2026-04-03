use snafu::{OptionExt, ResultExt};

use crate::{
    Result,
    error::{MissingToolSnafu, ToolTimeoutSnafu},
    tools::{ToolCallRequest, ToolContext, ToolOutput, registry::ToolRegistry},
};

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub call: ToolCallRequest,
    pub output: ToolOutput,
    pub message: llm::completion::Message,
}

pub struct ToolExecutor;

impl ToolExecutor {
    /// Executes all tool calls serially for the first milestone runtime.
    pub async fn execute_all(
        registry: &ToolRegistry,
        calls: Vec<ToolCallRequest>,
        context: ToolContext,
    ) -> Result<Vec<ToolExecutionResult>> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(Self::execute_one(registry, call, context.clone()).await?);
        }
        Ok(results)
    }

    /// Executes one tool call and converts its output into a tool-result message.
    async fn execute_one(
        registry: &ToolRegistry,
        call: ToolCallRequest,
        context: ToolContext,
    ) -> Result<ToolExecutionResult> {
        let tool = registry.get(&call.name).await.context(MissingToolSnafu {
            tool: call.name.clone(),
            stage: "tool-executor-lookup".to_string(),
        })?;

        let output = tokio::time::timeout(
            tool.metadata().timeout,
            tool.execute(call.arguments.clone(), context),
        )
        .await
        .context(ToolTimeoutSnafu {
            tool: call.name.clone(),
            stage: "tool-executor-timeout".to_string(),
        })??;

        let message = llm::completion::Message::tool_result_with_call_id(
            call.id.clone(),
            call.call_id.clone(),
            output.text.clone(),
        );

        Ok(ToolExecutionResult {
            call,
            output,
            message,
        })
    }
}

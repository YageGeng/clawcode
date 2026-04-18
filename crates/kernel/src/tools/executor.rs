use crate::{
    Result,
    tools::{ToolCallRequest, ToolContext, ToolOutput, router::ToolRouter},
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
        router: &ToolRouter,
        calls: Vec<ToolCallRequest>,
        context: ToolContext,
    ) -> Result<Vec<ToolExecutionResult>> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(Self::execute_one(router, call, context.clone()).await?);
        }
        Ok(results)
    }

    /// Executes one tool call and converts its output into a tool-result message.
    async fn execute_one(
        router: &ToolRouter,
        call: ToolCallRequest,
        context: ToolContext,
    ) -> Result<ToolExecutionResult> {
        let output = router
            .dispatch(call.clone(), context)
            .await
            .map_err(map_tool_error)?;

        // Keep plain text output first, then optionally append structured payloads.
        // The structured payload is preserved when it carries additional information
        // beyond the default `{ "text": ... }` wrapper.
        let mut content =
            llm::completion::message::ToolResultContent::from_tool_output(output.text.clone());
        if output.structured != serde_json::json!({ "text": output.text }) {
            let structured_content = llm::completion::message::ToolResultContent::from_tool_output(
                output.structured.to_string(),
            )
            .into_iter()
            .collect::<Vec<_>>();

            for item in structured_content {
                content.push(item);
            }
        }

        let call_id = call.call_id.clone().unwrap_or_else(|| call.id.clone());

        // Build a tool result message with mixed output content when present.
        let message = llm::completion::Message::User {
            content: llm::one_or_many::OneOrMany::one(
                llm::completion::message::UserContent::tool_result_with_call_id(
                    call.id.clone(),
                    call_id,
                    content,
                ),
            ),
        };

        Ok(ToolExecutionResult {
            call,
            output,
            message,
        })
    }
}

/// Maps extracted tools errors back into the kernel error surface.
fn map_tool_error(error: tools::Error) -> crate::Error {
    match error {
        tools::Error::Json { source, stage } => crate::Error::Json { source, stage },
        tools::Error::ToolTimeout {
            source,
            tool,
            stage,
        } => crate::Error::ToolTimeout {
            source,
            tool,
            stage,
        },
        tools::Error::MissingTool { tool, stage } => crate::Error::MissingTool { tool, stage },
        tools::Error::Runtime { message, stage } => crate::Error::Runtime { message, stage },
        tools::Error::ToolExecution {
            tool,
            message,
            stage,
        } => crate::Error::ToolExecution {
            tool,
            message,
            stage,
        },
        tools::Error::ToolApprovalRequired { tool, stage } => {
            crate::Error::ToolApprovalRequired { tool, stage }
        }
        tools::Error::ToolIo {
            tool,
            stage,
            source,
        } => crate::Error::ToolIo {
            tool,
            stage,
            source,
        },
    }
}

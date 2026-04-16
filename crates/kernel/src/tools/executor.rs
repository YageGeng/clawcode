use snafu::{OptionExt, ResultExt};

use crate::{
    Result,
    error::{MissingToolSnafu, ToolApprovalRequiredSnafu, ToolTimeoutSnafu},
    tools::{
        ApprovalRequirement, ToolApprovalRequest, ToolCallRequest, ToolContext, ToolOutput,
        registry::ToolRegistry,
    },
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

        if context.enforce_tool_approvals && tool.metadata().approval == ApprovalRequirement::Always
        {
            let approval_request = ToolApprovalRequest {
                tool: call.name.clone(),
                call_id: call.call_id.clone().or(Some(call.id.clone())),
                session_id: context.session_id.clone(),
                thread_id: context.thread_id.clone(),
                arguments: call.arguments.clone(),
            };

            let approved = context
                .tool_approval_handler
                .as_ref()
                .is_some_and(|handler| handler(&approval_request));

            if !approved {
                return ToolApprovalRequiredSnafu {
                    tool: call.name.clone(),
                    stage: "tool-executor-approval".to_string(),
                }
                .fail();
            }
        }

        let output = tokio::time::timeout(
            tool.metadata().timeout,
            tool.execute(call.arguments.clone(), context),
        )
        .await
        .context(ToolTimeoutSnafu {
            tool: call.name.clone(),
            stage: "tool-executor-timeout".to_string(),
        })??;

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

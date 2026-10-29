use super::*;

/// Delegates one self-contained task while holding its owner's tool invocation open.
pub(super) struct SubagentTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

#[async_trait]
impl AgentTool for SubagentTool {
    /// Uses the shared tool budget while the full answer remains in the child transcript.
    fn metadata(&self) -> &protocol::ToolMetadata {
        static METADATA: std::sync::LazyLock<protocol::ToolMetadata> =
            std::sync::LazyLock::new(|| {
                protocol::ToolMetadata::builder()
                    .output_policy(protocol::ToolOutputPolicy::Bounded)
                    .build()
            });
        &METADATA
    }

    /// Advertises isolated foreground delegation with the agreed model-facing contract.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "subagent".into(), description: "Delegate a self-contained task to a subagent with the same tools and get its answer back. Give it everything it needs to know; it does not see this conversation.".into(), parameters: serde_json::json!({"type":"object","properties":{"task":{"type":"string","description":"What the subagent should do"}},"required":["task"]}) }
    }

    /// Publishes the child immediately, then waits for its answering run and cancellation cleanup.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let task = call
            .arguments
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if task.trim().is_empty() {
            return Ok(ToolResult::builder()
                .tool_call_id(call.tool_call_id)
                .blocks(vec![ContentBlock::Text {
                    text: "subagent needs a task.".into(),
                }])
                .is_error(false)
                .build());
        }
        let _session = self
            .session
            .upgrade()
            .ok_or(SubagentError::SessionGone { tool: "subagent" })?;
        let title = SessionTitle::try_from(format!(
            "subagent: {}",
            task.chars().take(60).collect::<String>()
        ))
        .map_err(|e| ToolError::InvalidArguments {
            tool: call.name.clone(),
            message: e.to_string(),
        })?;
        let child = self
            .kernel
            .create_owned_session(
                SessionOwnership::Tool {
                    session_id: context.session_id.clone(),
                    tool_call_id: call.tool_call_id.clone(),
                },
                title,
            )
            .await
            .map_err(|source| SubagentError::Kernel {
                tool: "subagent",
                source,
            })?;
        let details = ToolResultDetails::Subagent {
            session_id: child.clone(),
        };
        context.publish(
            ToolResult::builder()
                .tool_call_id(call.tool_call_id.clone())
                .blocks(Vec::new())
                .is_error(false)
                .details(details.clone())
                .build(),
        );
        tracing::info!(
            "started subagent {} for session {} call {}",
            child,
            context.session_id,
            call.tool_call_id
        );
        // Deriving the token before the child run begins preserves cancellation that
        // races with creation; awaiting it holds the parent until cleanup completes.
        let answer = Box::pin(
            self.kernel.submit(
                SubmitRequest::builder()
                    .session_id(child.clone())
                    .content(vec![ContentBlock::Text { text: task.into() }])
                    .when_busy(WhenBusy::FollowUp)
                    .cancellation(Some(context.cancellation.clone()))
                    .trace_id(Some(context.trace_id.clone()))
                    .build(),
            ),
        )
        .await
        .map_err(|source| SubagentError::Kernel {
            tool: "subagent",
            source,
        })?;
        let (text, is_error) = match answer.outcome {
            AnswerOutcome::Completed => {
                tracing::info!("completed subagent {}", child);
                (
                    if answer.text.is_empty() {
                        "(no output)".into()
                    } else {
                        answer.text
                    },
                    false,
                )
            }
            AnswerOutcome::Cancelled if context.cancellation.is_cancelled() => {
                tracing::info!(
                    "cancelled subagent {} with its owner {}",
                    child,
                    context.session_id
                );
                return Err(ToolError::Cancelled);
            }
            AnswerOutcome::Cancelled => {
                tracing::info!(
                    "subagent {} was stopped before answering",
                    child
                );
                ("Subagent was stopped before answering.".into(), true)
            }
            AnswerOutcome::Failed(message) => {
                tracing::warn!("failed subagent {}: {}", child, message);
                (format!("Subagent failed: {message}"), true)
            }
        };
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text }])
            .is_error(is_error)
            .details(details)
            .build())
    }
}

//! `exec_command` and `write_stdin` as Kernel tools bound to one Session.

use std::sync::{Arc, Weak};

use ::tools::exec::{
    ExecCommandArgs, ExecEventSink, ExecProcessManager, ExecResponse,
    ExecStart, ExecWrite, WriteStdinArgs,
};
use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    ContentBlock, EXEC_COMMAND_TOOL, ToolCall, ToolCallId, ToolDefinition,
    ToolPromptContribution, ToolResult, WRITE_STDIN_TOOL,
};

use super::SessionExecSink;
use crate::runtime::{Kernel, Session};

/// Resolves the process manager of a still-live Session.
fn manager(
    session: &Weak<Session>,
    tool: &str,
) -> Result<Arc<ExecProcessManager>, ToolError> {
    let manager = session
        .upgrade()
        .and_then(|session| session.exec.as_ref().map(Arc::clone));
    manager.ok_or_else(|| {
        tracing::warn!("{} called without a live session process table", tool);
        ToolError::Execution {
            tool: tool.to_string(),
            message: "owning session is no longer available".to_string(),
        }
    })
}

/// Converts one exec response into the correlated tool result.
fn tool_result(
    tool_call_id: ToolCallId,
    response: &ExecResponse,
) -> ToolResult {
    ToolResult::builder()
        .tool_call_id(tool_call_id)
        .blocks(vec![ContentBlock::Text {
            text: response.model_text(),
        }])
        .is_error(response.is_error())
        .details(response.details())
        .build()
}

/// Starts shell commands that may keep running after the call returns.
pub(super) struct ExecCommandTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

#[async_trait]
impl AgentTool for ExecCommandTool {
    /// Uses codex's definition.
    fn definition(&self) -> ToolDefinition {
        ExecCommandArgs::definition()
    }

    /// Lists the tool in the System Prompt.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some(
                "Run shell commands; long-running ones keep running and return a session ID"
                    .to_string(),
            ),
            guidelines: Vec::new(),
        }
    }

    /// Rejects malformed arguments before anything runs.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        ExecCommandArgs::try_from(call).map(drop)
    }

    /// Runs the command until it exits or yields a session ID.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let args = ExecCommandArgs::try_from(&call)?;
        let manager = manager(&self.session, EXEC_COMMAND_TOOL)?;
        let sink: Arc<dyn ExecEventSink> = Arc::new(SessionExecSink {
            session: Weak::clone(&self.session),
            kernel: self.kernel.clone(),
            session_id: context.session_id.clone(),
        });
        let response = manager
            .exec_command(
                ExecStart::builder()
                    .args(args)
                    .cwd(context.cwd.clone())
                    .tool_call_id(call.tool_call_id.clone())
                    .turn_id(context.turn_id.clone())
                    .session_id(context.session_id.clone())
                    .cancellation(context.cancellation.clone())
                    .sink(sink)
                    .build(),
            )
            .await
            .map_err(|error| error.into_tool_error(EXEC_COMMAND_TOOL))?;
        Ok(tool_result(call.tool_call_id, &response))
    }
}

/// Polls, writes to, or interrupts a command session.
pub(super) struct WriteStdinTool {
    pub(super) session: Weak<Session>,
}

#[async_trait]
impl AgentTool for WriteStdinTool {
    /// Uses codex's definition.
    fn definition(&self) -> ToolDefinition {
        WriteStdinArgs::definition()
    }

    /// Lists the tool in the System Prompt.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some(
                "Poll or send input to a running exec_command session"
                    .to_string(),
            ),
            guidelines: Vec::new(),
        }
    }

    /// Rejects malformed arguments before anything is written.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        WriteStdinArgs::try_from(call).map(drop)
    }

    /// Writes the input, then returns output collected until the yield time.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let args = WriteStdinArgs::try_from(&call)?;
        let manager = manager(&self.session, WRITE_STDIN_TOOL)?;
        let response = manager
            .write_stdin(ExecWrite {
                args,
                cancellation: context.cancellation.clone(),
            })
            .await
            .map_err(|error| error.into_tool_error(WRITE_STDIN_TOOL))?;
        Ok(tool_result(call.tool_call_id, &response))
    }
}

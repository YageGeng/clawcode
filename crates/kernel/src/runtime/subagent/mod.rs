mod background;
mod foreground;
pub(super) use background::SessionBackgroundWork;

use super::{
    AnswerOutcome, Kernel, KernelError, Session, SubmitRequest, WhenBusy,
};
use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    ContentBlock, SessionOwnership, SessionTitle, ToolCall, ToolDefinition,
    ToolResult, ToolResultDetails,
};
use std::sync::{Arc, Weak};

/// Adds tool identity to infrastructure failures without changing their source.
#[derive(Debug, thiserror::Error)]
pub(in crate::runtime) enum SubagentError {
    #[error("{tool}: {source}")]
    Kernel {
        tool: &'static str,
        #[source]
        source: KernelError,
    },
    #[error("{tool}: owning session is no longer available")]
    SessionGone { tool: &'static str },
}

impl From<SubagentError> for ToolError {
    /// Preserves the useful source message in a recoverable tool result.
    fn from(error: SubagentError) -> Self {
        let tool = match &error {
            SubagentError::Kernel { tool, .. }
            | SubagentError::SessionGone { tool } => *tool,
        };
        tracing::warn!("failed {}: {}", tool, error);
        Self::Execution {
            tool: tool.into(),
            message: error.to_string(),
        }
    }
}

/// Registers built-in delegation unless an extension already supplies the same name.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
    kernel: &Kernel,
) -> Result<(), KernelError> {
    if runtime.tools.snapshot()?.has_extension("subagent") {
        tracing::info!(
            "skipped built-in subagent because an extension provides it"
        );
    } else {
        runtime.tools.register(Arc::new(foreground::SubagentTool {
            session: Arc::downgrade(runtime),
            kernel: kernel.clone(),
        }))?;
    }
    if runtime
        .tools
        .snapshot()?
        .has_extension("background_subagent")
    {
        tracing::info!(
            "skipped built-in background_subagent because an extension provides it"
        );
    } else {
        runtime.tools.register(Arc::new(
            background::BackgroundSubagentTool {
                session: Arc::downgrade(runtime),
                kernel: kernel.clone(),
            },
        ))?;
    }
    Ok(())
}

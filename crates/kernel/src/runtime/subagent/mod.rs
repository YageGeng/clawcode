mod background;
mod fork;
mod mail;
mod path;
mod roster;
mod tools;

pub(super) use background::SessionBackgroundWork;
pub use mail::{MailDelivery, MailPlacement};
pub(super) use roster::AgentRoster;
pub(in crate::runtime) use roster::SubagentRosterNote;

use super::{Kernel, KernelError, Session};
use ::tools::ToolError;
use std::sync::Arc;

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

/// Registers the built-in agent tools unless an extension supplies the same name.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
    kernel: &Kernel,
) -> Result<(), KernelError> {
    let session = Arc::downgrade(runtime);
    let tools: [Arc<dyn ::tools::AgentTool>; 6] = [
        Arc::new(tools::SpawnAgentTool {
            session: std::sync::Weak::clone(&session),
            kernel: kernel.clone(),
        }),
        Arc::new(tools::MailTool {
            session: std::sync::Weak::clone(&session),
            kernel: kernel.clone(),
            kind: protocol::MailKind::Message,
        }),
        Arc::new(tools::MailTool {
            session: std::sync::Weak::clone(&session),
            kernel: kernel.clone(),
            kind: protocol::MailKind::NewTask,
        }),
        Arc::new(tools::WaitAgentTool {
            session: std::sync::Weak::clone(&session),
        }),
        Arc::new(tools::ListAgentsTool {
            session: std::sync::Weak::clone(&session),
            kernel: kernel.clone(),
        }),
        Arc::new(tools::InterruptAgentTool {
            session,
            kernel: kernel.clone(),
        }),
    ];
    for tool in tools {
        let name = tool.definition().name;
        if runtime.tools.snapshot()?.has_extension(&name) {
            tracing::info!(
                "skipped built-in {} because an extension provides it",
                name
            );
        } else {
            runtime.tools.register_kernel(tool)?;
        }
    }
    Ok(())
}

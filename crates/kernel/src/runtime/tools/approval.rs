//! Session-owned, single-use permission for built-in file mutations.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ::tools::{FileMutationApprover, ToolError};
use protocol::{
    AgentEventPayload, FileApprovalDecision, FileApprovalRequest, IdGenerator,
    IdKind, SessionId, TimestampMs, ToolCall, TurnId,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::runtime::{Kernel, KernelError, Session};

/// One request visible to all clients and the only sender that may authorize it.
struct PendingFileApproval {
    request: FileApprovalRequest,
    answer: oneshot::Sender<FileApprovalDecision>,
}

/// Sibling tool calls can each wait for a different answer.
#[derive(Default)]
pub(in crate::runtime) struct SessionFileApprovals {
    pending: Mutex<BTreeMap<String, PendingFileApproval>>,
}

/// Removes an abandoned request and balances the shared Run wait count on every exit.
struct FileApprovalWait<'a> {
    session: &'a Session,
    request_id: &'a str,
    counted: bool,
}

impl Drop for FileApprovalWait<'_> {
    /// A failed event delivery or a dropped Run must not leave a pending permission.
    fn drop(&mut self) {
        if let Ok(mut pending) = self.session.file_approvals.pending.lock() {
            pending.remove(self.request_id);
        }
        if self.counted {
            self.session.execution.end_user_wait();
        }
    }
}

/// Correlation and cancellation shared by the mutation tools in a concurrent batch.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct FileApprovalGate {
    pub(super) session: Arc<Session>,
    pub(super) turn_id: TurnId,
    pub(super) id_generator: Arc<dyn IdGenerator>,
    pub(super) cancellation: CancellationToken,
}

impl FileApprovalGate {
    /// Sends the prompt and waits for either its first decision or cancellation.
    async fn request(
        &self,
        request: FileApprovalRequest,
    ) -> Result<FileApprovalDecision, KernelError> {
        let request_id = request.request_id.clone();
        let (answer, receiver) = oneshot::channel();
        self.session.file_approvals.pending.lock()?.insert(
            request_id.clone(),
            PendingFileApproval {
                request: request.clone(),
                answer,
            },
        );
        let mut wait = FileApprovalWait {
            session: &self.session,
            request_id: &request_id,
            counted: false,
        };
        let execution = &self.session.execution;
        execution
            .emit_to_run(
                self.turn_id.clone(),
                TimestampMs::now(),
                AgentEventPayload::FileApprovalRequested { request },
            )
            .await?;
        let first = execution.begin_user_wait();
        wait.counted = true;
        if first {
            execution
                .emit_to_run(
                    self.turn_id.clone(),
                    TimestampMs::now(),
                    AgentEventPayload::ActionRequired {
                        request_id: request_id.clone(),
                    },
                )
                .await?;
        }
        let decision = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => FileApprovalDecision::Cancelled,
            answer = receiver => answer.unwrap_or(FileApprovalDecision::Cancelled),
        };
        // Remove cancelled requests before publishing resolution to resuming clients.
        self.session
            .file_approvals
            .pending
            .lock()?
            .remove(&request_id);
        execution
            .emit_to_run(
                self.turn_id.clone(),
                TimestampMs::now(),
                AgentEventPayload::FileApprovalResolved {
                    request_id: request_id.clone(),
                    decision,
                },
            )
            .await?;
        wait.counted = false;
        if execution.end_user_wait() {
            execution
                .emit_to_run(
                    self.turn_id.clone(),
                    TimestampMs::now(),
                    AgentEventPayload::ActionResolved {
                        request_id: request_id.clone(),
                    },
                )
                .await?;
        }
        Ok(decision)
    }
}

#[async_trait::async_trait]
impl FileMutationApprover for FileApprovalGate {
    /// Allows only an explicit affirmative response for this exact invocation.
    async fn approve(
        &self,
        call: &ToolCall,
        target: &Path,
    ) -> Result<(), ToolError> {
        if self.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        // A symlink may resolve to a Unix byte path that a JSON permission prompt cannot identify.
        if target.to_str().is_none() {
            tracing::warn!(
                "refused file approval for call {} because its target cannot be represented as UTF-8",
                call.tool_call_id
            );
            return Err(ToolError::Execution {
                tool: call.name.clone(),
                message: "The external file path cannot be represented in a permission request.".into(),
            });
        }
        // Only the parent has a user-facing approval channel for an external mutation.
        if let Some(owner) = &self.session.ownership {
            tracing::warn!(
                "refused external {} call {} on {} from subagent {} of Session {}",
                call.name,
                call.tool_call_id,
                target.display(),
                owner.name,
                owner.session_id
            );
            return Err(ToolError::Execution {
                tool: call.name.clone(),
                message: format!(
                    "Only the parent session can request approval to {} {}. Send the parent this path and the intended change so it can request user approval and perform the operation.",
                    call.name,
                    target.display()
                ),
            });
        }
        let request = FileApprovalRequest::builder()
            .request_id(self.id_generator.next(IdKind::Request))
            .tool_call_id(call.tool_call_id.clone())
            .tool_name(call.name.clone())
            .path(target.to_path_buf())
            .build();
        tracing::info!(
            "requesting file approval {} for {} call {} on {}",
            request.request_id,
            call.name,
            call.tool_call_id,
            target.display()
        );
        let decision = self.request(request).await.map_err(|error| {
            tracing::warn!(
                "failed to request file approval for call {}: {}",
                call.tool_call_id,
                error
            );
            ToolError::Execution {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })?;
        tracing::info!(
            "resolved file approval for {} call {} as {:?}",
            call.name,
            call.tool_call_id,
            decision
        );
        match decision {
            FileApprovalDecision::Allow
                if !self.cancellation.is_cancelled() =>
            {
                Ok(())
            }
            FileApprovalDecision::Reject => Err(ToolError::Execution {
                tool: call.name.clone(),
                message: format!(
                    "Permission denied to {} {}.",
                    call.name,
                    target.display()
                ),
            }),
            FileApprovalDecision::Unavailable => Err(ToolError::Execution {
                tool: call.name.clone(),
                message: format!(
                    "No attached client could process the permission request to {} {}.",
                    call.name,
                    target.display()
                ),
            }),
            _ => Err(ToolError::Cancelled),
        }
    }
}

impl Kernel {
    /// Returns every file approval still waiting in this live Session.
    pub fn file_approvals(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<FileApprovalRequest>, KernelError> {
        Ok(self
            .session(session_id)?
            .file_approvals
            .pending
            .lock()?
            .values()
            .map(|pending| pending.request.clone())
            .collect())
    }

    /// Takes the unique answer route so stale or duplicate replies cannot authorize a write.
    pub fn respond_file_approval(
        &self,
        session_id: &SessionId,
        request_id: &str,
        decision: FileApprovalDecision,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let pending = session.file_approvals.pending.lock()?.remove(request_id);
        if let Some(pending) = pending
            && pending.answer.send(decision).is_ok()
        {
            return Ok(());
        }
        tracing::info!(
            "ignored response for file approval {} in Session {} that is no longer pending",
            request_id,
            session_id
        );
        Err(KernelError::FileApprovalResolved(request_id.into()))
    }
}

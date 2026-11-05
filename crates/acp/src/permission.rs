//! ACP `session/request_permission` for plans waiting in `exit_plan_mode`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::v2::schema as wire;
use agent_client_protocol::{
    Client, RequestCancellationHandle, v2::ConnectionTo as V2ConnectionTo,
};
use kernel::{Kernel, KernelError};
use protocol::{
    PlanApprovalDecision, PlanApprovalRequest, PlanApprovalResponse, SessionId,
};

/// Title every client shows for a plan approval.
const TITLE: &str = "Ready to execute the plan?";
/// Option that approves the plan and leaves plan mode.
const APPROVE: &str = "approve";
/// Option that keeps the Session in plan mode.
const KEEP_PLANNING: &str = "keep_planning";

/// A plan-approval transition carried by one projected event group.
#[derive(Debug, Clone)]
pub(crate) enum PlanApprovalChange {
    /// A plan started waiting for the user.
    Requested(Box<PlanApprovalRequest>),
    /// The plan was answered or withdrawn anywhere.
    Resolved { request_id: String },
}

/// Per-connection copies of pending plan approvals, so a settled one is withdrawn.
#[derive(Default)]
pub(crate) struct PlanApprovalBridge {
    outstanding:
        Arc<Mutex<HashMap<(SessionId, String), RequestCancellationHandle>>>,
}

impl PlanApprovalBridge {
    /// Asks the client to approve one plan without waiting for its answer.
    pub(crate) fn request(
        &self,
        connection: &V2ConnectionTo<Client>,
        kernel: &Arc<Kernel>,
        session_id: &SessionId,
        request: &PlanApprovalRequest,
    ) {
        let key = (session_id.clone(), request.request_id.clone());
        let Ok(mut outstanding) = self.outstanding.lock() else {
            tracing::warn!("ACP plan approval table lock poisoned");
            return;
        };
        if outstanding.contains_key(&key) {
            return;
        }
        tracing::info!(
            "sending session/request_permission for plan approval {} in Session {}",
            request.request_id,
            session_id
        );
        let sent = connection.send_request(
            wire::RequestPermissionRequest::new(
                session_id.to_string(),
                TITLE,
                vec![
                    wire::PermissionOption::new(
                        APPROVE,
                        "Approve and execute",
                        wire::PermissionOptionKind::AllowOnce,
                    ),
                    wire::PermissionOption::new(
                        KEEP_PLANNING,
                        "Keep planning",
                        wire::PermissionOptionKind::RejectOnce,
                    ),
                ],
            )
            .subject(wire::RequestPermissionSubject::from(
                wire::ToolCallPermissionSubject::new(
                    wire::ToolCallUpdate::new(request.tool_call_id.to_string())
                        .kind(wire::ToolKind::SwitchMode)
                        .content(vec![wire::ToolCallContent::from(
                            request.plan.clone(),
                        )]),
                ),
            )),
        );
        outstanding.insert(key.clone(), sent.cancellation_handle());
        drop(outstanding);
        let table = Arc::clone(&self.outstanding);
        let kernel = Arc::clone(kernel);
        if let Err(error) = connection.spawn(async move {
            let response = sent.block_task().await;
            if let Ok(mut outstanding) = table.lock() {
                outstanding.remove(&key);
            }
            let (session_id, request_id) = key;
            match response {
                Ok(response) => {
                    let decision = Self::decision(&response.outcome);
                    tracing::info!(
                        "received plan approval {} in Session {} as {:?}",
                        request_id,
                        session_id,
                        decision
                    );
                    Self::deliver(
                        &kernel,
                        PlanApprovalResponse {
                            session_id,
                            request_id,
                            decision,
                        },
                    )
                    .await;
                }
                // The plan stays pending for another client or a resume.
                Err(error) => tracing::warn!(
                    "session/request_permission for plan approval {} in Session {} failed: {}",
                    request_id,
                    session_id,
                    error
                ),
            }
            Ok(())
        }) {
            tracing::warn!("failed to start plan approval answer task: {}", error);
        }
    }

    /// Withdraws this connection's copy of an approval settled anywhere.
    pub(crate) fn resolved(&self, session_id: &SessionId, request_id: &str) {
        let handle =
            self.outstanding.lock().ok().and_then(|mut outstanding| {
                outstanding
                    .remove(&(session_id.clone(), request_id.to_string()))
            });
        if let Some(handle) = handle {
            tracing::info!(
                "cancelling session/request_permission for plan approval {} in Session {}",
                request_id,
                session_id
            );
            if let Err(error) = handle.cancel() {
                tracing::warn!(
                    "failed to cancel session/request_permission for plan approval {}: {}",
                    request_id,
                    error
                );
            }
        }
    }

    /// Asks again for the plan a resumed Session still holds.
    pub(crate) fn reestablish(
        &self,
        connection: &V2ConnectionTo<Client>,
        kernel: &Arc<Kernel>,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        if let Some(request) = kernel.plan_approval(session_id)? {
            self.request(connection, kernel, session_id, &request);
        }
        Ok(())
    }

    /// Maps the client's outcome; anything but a known option keeps planning.
    fn decision(
        outcome: &wire::RequestPermissionOutcome,
    ) -> PlanApprovalDecision {
        match outcome {
            wire::RequestPermissionOutcome::Selected(selected)
                if &*selected.option_id.0 == APPROVE =>
            {
                PlanApprovalDecision::Approve
            }
            wire::RequestPermissionOutcome::Selected(selected)
                if &*selected.option_id.0 == KEEP_PLANNING =>
            {
                PlanApprovalDecision::KeepPlanning
            }
            _ => PlanApprovalDecision::Cancelled,
        }
    }

    /// Hands one decision to the Kernel; a plan settled elsewhere is ignored.
    async fn deliver(kernel: &Kernel, response: PlanApprovalResponse) {
        let request_id = response.request_id.clone();
        match kernel.respond_plan_approval(response).await {
            Ok(()) | Err(KernelError::PlanApprovalResolved(_)) => {}
            Err(error) => tracing::warn!(
                "failed to deliver plan approval {}: {}",
                request_id,
                error
            ),
        }
    }
}

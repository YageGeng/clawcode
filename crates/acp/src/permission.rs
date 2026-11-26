//! ACP permission requests for plans and file mutations waiting on the user.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use agent_client_protocol::v2::schema as wire;
use agent_client_protocol::{
    Client, RequestCancellationHandle, v2::ConnectionTo as V2ConnectionTo,
};
use kernel::{Kernel, KernelError};
use protocol::{
    FileApprovalDecision, FileApprovalRequest, PlanApprovalDecision,
    PlanApprovalRequest, PlanApprovalResponse, SessionId,
};

use crate::tool_call::ToolKindOf;

/// Each permission option is valid only for the kind of request that offered it.
const APPROVE: &str = "approve";
const KEEP_PLANNING: &str = "keep_planning";
const ALLOW_ONCE: &str = "allow_once";
const REJECT_ONCE: &str = "reject_once";

/// Pending Kernel operations that use ACP's standard permission method.
#[derive(Debug)]
pub(crate) enum PendingApproval {
    Plan(PlanApprovalRequest),
    File(FileApprovalRequest),
}

impl PendingApproval {
    /// Returns the Kernel-generated identity, unique even when a tool call ID recurs.
    fn id(&self) -> &str {
        match self {
            Self::Plan(request) => &request.request_id,
            Self::File(request) => &request.request_id,
        }
    }

    /// Builds the prompt and the correlated tool-call subject for this operation.
    fn wire(&self, session_id: &SessionId) -> wire::RequestPermissionRequest {
        let (prompt, subject) = match self {
            Self::Plan(request) => (
                wire::RequestPermissionRequest::new(
                    session_id.to_string(),
                    "Ready to execute the plan?",
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
                ),
                wire::ToolCallUpdate::new(request.tool_call_id.to_string())
                    .kind(wire::ToolKind::SwitchMode)
                    .content(vec![wire::ToolCallContent::from(
                        request.plan.clone(),
                    )]),
            ),
            Self::File(request) => {
                let path = request.path.display();
                let action = if request.tool_name == "edit" {
                    "editing"
                } else {
                    "writing to"
                };
                (
                    wire::RequestPermissionRequest::new(
                        session_id.to_string(),
                        format!("Allow {action} {path}?"),
                        vec![
                            wire::PermissionOption::new(
                                ALLOW_ONCE,
                                "Allow once",
                                wire::PermissionOptionKind::AllowOnce,
                            ),
                            wire::PermissionOption::new(
                                REJECT_ONCE,
                                "Reject once",
                                wire::PermissionOptionKind::RejectOnce,
                            ),
                        ],
                    )
                    .description("This file is outside the session workspace."),
                    wire::ToolCallUpdate::new(request.tool_call_id.to_string())
                        .name(request.tool_name.clone())
                        .kind(wire::ToolKind::from(ToolKindOf(
                            &request.tool_name,
                        )))
                        .locations(vec![wire::ToolCallLocation::new(
                            request.path.clone(),
                        )])
                        .content(vec![wire::ToolCallContent::from(
                            path.to_string(),
                        )]),
                )
            }
        };
        prompt.subject(wire::RequestPermissionSubject::from(
            wire::ToolCallPermissionSubject::new(subject),
        ))
    }

    /// Delivers the first known decision, treating dismissal and unknown options as cancellation.
    async fn deliver(
        &self,
        kernel: &Kernel,
        session_id: &SessionId,
        outcome: wire::RequestPermissionOutcome,
    ) -> Result<(), KernelError> {
        let selection = match &outcome {
            wire::RequestPermissionOutcome::Selected(selected) => {
                Some(selected.option_id.0.as_ref())
            }
            _ => None,
        };
        match self {
            Self::Plan(request) => {
                let decision = match selection {
                    Some(APPROVE) => PlanApprovalDecision::Approve,
                    Some(KEEP_PLANNING) => PlanApprovalDecision::KeepPlanning,
                    _ => PlanApprovalDecision::Cancelled,
                };
                kernel
                    .respond_plan_approval(PlanApprovalResponse {
                        session_id: session_id.clone(),
                        request_id: request.request_id.clone(),
                        decision,
                    })
                    .await
            }
            Self::File(request) => {
                let decision = match selection {
                    Some(ALLOW_ONCE) => FileApprovalDecision::Allow,
                    Some(REJECT_ONCE) => FileApprovalDecision::Reject,
                    _ => FileApprovalDecision::Cancelled,
                };
                kernel.respond_file_approval(
                    session_id,
                    &request.request_id,
                    decision,
                )
            }
        }
    }
}

/// A permission transition carried only by a live projected event group.
#[derive(Debug, Clone)]
pub(crate) enum ApprovalChange {
    Requested(Arc<PendingApproval>),
    Resolved { request_id: String },
}

/// A failed copy remains recorded until the same Kernel request settles.
enum ClientPrompt {
    Sent(RequestCancellationHandle),
    Failed,
}

/// All clients watching one Session, including watchers that have not dispatched a prompt yet.
#[derive(Default)]
pub(crate) struct ApprovalPeers(Mutex<Vec<Weak<ApprovalBridge>>>);

impl ApprovalPeers {
    /// Registers a watcher before its first live event or pending-request snapshot.
    pub(crate) fn attach(
        self: &Arc<Self>,
        session_id: &SessionId,
        connection: &V2ConnectionTo<Client>,
        kernel: &Arc<Kernel>,
    ) -> Result<Arc<ApprovalBridge>, KernelError> {
        let bridge = Arc::new(
            ApprovalBridge::builder()
                .session_id(session_id.clone())
                .connection(connection.clone())
                .kernel(Arc::clone(kernel))
                .peers(Arc::clone(self))
                .build(),
        );
        let mut peers = self.0.lock()?;
        peers.retain(|peer| peer.strong_count() > 0);
        peers.push(Arc::downgrade(&bridge));
        Ok(bridge)
    }
}

/// This connection's copies of a single Session's pending permissions.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct ApprovalBridge {
    session_id: SessionId,
    connection: V2ConnectionTo<Client>,
    kernel: Arc<Kernel>,
    peers: Arc<ApprovalPeers>,
    #[builder(default, setter(skip))]
    outstanding: Mutex<HashMap<String, ClientPrompt>>,
}

impl ApprovalBridge {
    /// Sends or withdraws this connection's prompt for a live Kernel transition.
    pub(crate) fn apply(
        self: &Arc<Self>,
        change: &ApprovalChange,
    ) -> Result<(), KernelError> {
        let mut outstanding = self.outstanding.lock()?;
        match change {
            ApprovalChange::Requested(request) => {
                self.send(&mut outstanding, Arc::clone(request));
            }
            ApprovalChange::Resolved { request_id } => {
                if let Some(ClientPrompt::Sent(handle)) =
                    outstanding.remove(request_id)
                {
                    self.withdraw(request_id, &handle);
                }
            }
        }
        Ok(())
    }

    /// Asks again for every permission still pending in a resumed Session.
    pub(crate) fn reestablish(self: &Arc<Self>) -> Result<(), KernelError> {
        // Serialize the snapshot and its sends with live withdrawal on this connection.
        let mut outstanding = self.outstanding.lock()?;
        let pending: Vec<_> = self
            .kernel
            .plan_approval(&self.session_id)?
            .into_iter()
            .map(PendingApproval::Plan)
            .chain(
                self.kernel
                    .file_approvals(&self.session_id)?
                    .into_iter()
                    .map(PendingApproval::File),
            )
            .collect();
        let pending_ids: HashSet<_> =
            pending.iter().map(PendingApproval::id).collect();
        // A repeated resume may replace this connection's watcher before it sends a resolution.
        outstanding.retain(|id, prompt| {
            if pending_ids.contains(id.as_str()) {
                return true;
            }
            if let ClientPrompt::Sent(handle) = prompt {
                self.withdraw(id, handle);
            }
            false
        });
        for request in pending {
            self.send(&mut outstanding, Arc::new(request));
        }
        Ok(())
    }

    /// Closes a client-side copy whose request has already settled in the Kernel.
    fn withdraw(&self, request_id: &str, handle: &RequestCancellationHandle) {
        tracing::info!(
            "withdrawing ACP approval {} in Session {}",
            request_id,
            self.session_id
        );
        if let Err(error) = handle.cancel() {
            tracing::warn!(
                "failed to withdraw ACP approval {}: {}",
                request_id,
                error
            );
        }
    }

    /// Fails the file tool when every attached client explicitly failed its copy.
    fn file_request_failed(&self, request_id: &str) -> Result<(), KernelError> {
        // Serialize attachment and explicit failures; a peer without a copy yet still gets to answer.
        let peers = self.peers.0.lock()?;
        let mut outstanding = self.outstanding.lock()?;
        let Some(prompt) = outstanding.get_mut(request_id) else {
            return Ok(());
        };
        *prompt = ClientPrompt::Failed;
        drop(outstanding);
        for peer in peers.iter().filter_map(Weak::upgrade) {
            if !peer.connection.is_incoming_closed()
                && peer.connection.termination().is_none()
                && !matches!(
                    peer.outstanding.lock()?.get(request_id),
                    Some(ClientPrompt::Failed)
                )
            {
                return Ok(());
            }
        }
        tracing::warn!(
            "all attached clients failed file approval {} in Session {}",
            request_id,
            self.session_id
        );
        self.kernel.respond_file_approval(
            &self.session_id,
            request_id,
            FileApprovalDecision::Unavailable,
        )
    }

    /// Distinguishes a client reply from local transport loss using SDK-owned error origins.
    async fn answered(
        &self,
        request: &PendingApproval,
        response: agent_client_protocol::Result<
            wire::RequestPermissionResponse,
        >,
    ) -> Result<(), KernelError> {
        let request_id = request.id();
        let result = match response {
            Ok(response) => {
                let result = request
                    .deliver(&self.kernel, &self.session_id, response.outcome)
                    .await;
                if result.is_ok() {
                    tracing::info!(
                        "delivered ACP approval {} in Session {}",
                        request_id,
                        self.session_id
                    );
                }
                result
            }
            Err(error)
                if agent_client_protocol::is_incoming_transport_closed(
                    &error,
                ) || agent_client_protocol::is_connection_terminated(
                    &error,
                ) =>
            {
                tracing::info!(
                    "preserved approval {} in Session {} after ACP transport loss",
                    request_id,
                    self.session_id
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    "session/request_permission for approval {} in Session {} failed: {}",
                    request_id,
                    self.session_id,
                    error.code
                );
                if matches!(request, PendingApproval::File(_)) {
                    // Retain the failure for peer accounting until the Kernel settles the request.
                    return self.file_request_failed(request_id);
                }
                Ok(())
            }
        };
        self.outstanding.lock()?.remove(request_id);
        result
    }

    /// Starts one ACP request without blocking the Session watcher or its sibling prompts.
    fn send(
        self: &Arc<Self>,
        outstanding: &mut HashMap<String, ClientPrompt>,
        request: Arc<PendingApproval>,
    ) {
        let request_id = request.id();
        if outstanding.contains_key(request_id) {
            return;
        }
        tracing::info!(
            "sending session/request_permission for approval {} in Session {}",
            request_id,
            self.session_id
        );
        let sent = self.connection.send_request(request.wire(&self.session_id));
        outstanding.insert(
            request_id.to_owned(),
            ClientPrompt::Sent(sent.cancellation_handle()),
        );
        let bridge = Arc::clone(self);
        let task_request = Arc::clone(&request);
        if let Err(error) = self.connection.spawn(async move {
            match bridge
                .answered(&task_request, sent.block_task().await)
                .await
            {
                Ok(())
                | Err(
                    KernelError::PlanApprovalResolved(_)
                    | KernelError::FileApprovalResolved(_),
                ) => {}
                Err(error) => tracing::warn!(
                    "failed to deliver ACP approval {} in Session {}: {}",
                    task_request.id(),
                    bridge.session_id,
                    error
                ),
            }
            Ok(())
        }) {
            outstanding.remove(request_id);
            tracing::warn!(
                "failed to start ACP approval answer task in Session {}: {}",
                self.session_id,
                error
            );
        }
    }
}

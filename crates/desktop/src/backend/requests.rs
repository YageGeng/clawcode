//! Agent→client requests: the parked responders waiting for the user, and
//! the requests the UI shows for them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use agent_client_protocol::v2::{Responder, schema as wire};
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;

use super::{BackendError, BackendEvent, SessionId};

/// Client-local identity of one agent→client request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

/// An agent→client request waiting for the user.
#[derive(Clone, Debug)]
pub enum AgentRequest {
    /// `session/request_permission`, such as a plan approval.
    Permission {
        /// Local identity used to answer it.
        id: RequestId,
        /// The request as the agent sent it.
        request: wire::RequestPermissionRequest,
    },
    /// `elicitation/create` scoped to a session.
    Elicitation {
        /// Local identity used to answer it.
        id: RequestId,
        /// The request as the agent sent it.
        request: wire::CreateElicitationRequest,
    },
}

impl AgentRequest {
    /// Local identity of the request.
    pub fn id(&self) -> RequestId {
        match self {
            AgentRequest::Permission { id, .. }
            | AgentRequest::Elicitation { id, .. } => *id,
        }
    }

    /// Session the request belongs to; request-scoped elicitations have none.
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            AgentRequest::Permission { request, .. } => {
                Some(&request.session_id)
            }
            AgentRequest::Elicitation { request, .. } => {
                match request.scope() {
                    wire::ElicitationScope::Session(scope) => {
                        Some(&scope.session_id)
                    }
                    _ => None,
                }
            }
        }
    }
}

/// The responder of one open request, kept until the user answers or the agent withdraws it.
pub(super) enum Waiting {
    /// Answers a permission request.
    Permission(Responder<wire::RequestPermissionResponse>),
    /// Answers an elicitation.
    Elicitation(Responder<wire::CreateElicitationResponse>),
}

/// One open request: its responder and the signal that ends its cancellation watch.
pub(super) struct Pending {
    waiting: Waiting,
    answered: oneshot::Sender<()>,
}

/// Open agent→client requests, shared by the connection handlers and the UI's answers.
#[derive(Clone, Default)]
pub(super) struct PendingRequests {
    next: Arc<AtomicU64>,
    open: Arc<Mutex<HashMap<RequestId, Pending>>>,
}

impl PendingRequests {
    /// Answers a permission request with one of its options, or `None` to cancel it.
    ///
    /// A request the agent already withdrew is answered by nobody: this is then a no-op.
    pub(super) fn answer_permission(
        &self,
        id: RequestId,
        option_id: Option<&str>,
    ) -> Result<(), BackendError> {
        let Some(open) = self.take(id) else {
            return Ok(());
        };
        let _ = open.answered.send(());
        let outcome = match option_id {
            Some(option) => wire::RequestPermissionOutcome::Selected(
                wire::SelectedPermissionOutcome::new(option.to_owned()),
            ),
            None => wire::RequestPermissionOutcome::Cancelled(
                wire::CancelledPermissionOutcome::new(),
            ),
        };
        match open.waiting {
            Waiting::Permission(responder) => responder
                .respond(wire::RequestPermissionResponse::new(outcome))
                .map_err(BackendError::from),
            Waiting::Elicitation(responder) => {
                // Mismatched answer kinds cancel rather than leave the agent waiting.
                Waiting::Elicitation(responder).withdraw();
                Err(BackendError::Request(format!(
                    "request {} is not a permission request",
                    id.0
                )))
            }
        }
    }

    /// Answers an elicitation; a request the agent already withdrew makes this a no-op.
    pub(super) fn answer_elicitation(
        &self,
        id: RequestId,
        response: wire::CreateElicitationResponse,
    ) -> Result<(), BackendError> {
        let Some(open) = self.take(id) else {
            return Ok(());
        };
        let _ = open.answered.send(());
        match open.waiting {
            Waiting::Elicitation(responder) => {
                responder.respond(response).map_err(BackendError::from)
            }
            Waiting::Permission(responder) => {
                Waiting::Permission(responder).withdraw();
                Err(BackendError::Request(format!(
                    "request {} is not an elicitation",
                    id.0
                )))
            }
        }
    }

    /// Parks a responder, reports the request built by `request`, and starts
    /// watching for the agent withdrawing it.
    ///
    /// Returns the request's local id. When the agent sends `$/cancel_request`
    /// first, the request is answered as cancelled and `RequestCancelled` is reported.
    pub(super) fn park(
        &self,
        waiting: Waiting,
        cancellation: agent_client_protocol::RequestCancellation,
        events: UnboundedSender<BackendEvent>,
        request: impl FnOnce(RequestId) -> AgentRequest,
    ) -> RequestId {
        let id = RequestId(self.next.fetch_add(1, Ordering::Relaxed));
        let (answered, answered_rx) = oneshot::channel();
        self.lock().insert(id, Pending { waiting, answered });
        // Reported before the watcher starts, so a withdrawal can never be
        // seen ahead of the request it withdraws.
        let _ =
            events.unbounded_send(BackendEvent::Request(Box::new(request(id))));
        let pending = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = cancellation.cancelled() => {
                    if let Some(open) = pending.take(id) {
                        open.waiting.withdraw();
                        let _ = events.unbounded_send(BackendEvent::RequestCancelled(id));
                    }
                }
                // Answered by the user; nothing left to watch.
                _ = answered_rx => {}
            }
        });
        id
    }

    /// Removes an open request; `None` when it was answered or withdrawn already.
    pub(super) fn take(&self, id: RequestId) -> Option<Pending> {
        self.lock().remove(&id)
    }

    /// The open-request table; a panic elsewhere never makes it unusable.
    pub(super) fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<RequestId, Pending>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Waiting {
    /// Answers the request the way a dismissed prompt would.
    pub(super) fn withdraw(self) {
        let result = match self {
            Waiting::Permission(responder) => {
                responder.respond(wire::RequestPermissionResponse::new(
                    wire::RequestPermissionOutcome::Cancelled(
                        wire::CancelledPermissionOutcome::new(),
                    ),
                ))
            }
            Waiting::Elicitation(responder) => {
                responder.respond(wire::CreateElicitationResponse::new(
                    wire::ElicitationAction::Cancel,
                ))
            }
        };
        if let Err(error) = result {
            tracing::debug!("withdrawn request was already closed: {}", error);
        }
    }
}

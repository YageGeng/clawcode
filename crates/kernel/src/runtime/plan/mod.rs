//! Plan mode: the Session's mode and how it is persisted.

use std::sync::{Mutex, RwLock};

use protocol::{
    PlanApprovalDecision, PlanApprovalRequest, SessionMode, TurnId,
};
use serde::{Deserialize, Serialize};
use store::SessionEntry;
use tokio_util::sync::CancellationToken;

use crate::runtime::KernelError;
use crate::runtime::session::ClawcodeEntry;

mod approval;
mod gate;
mod note;
mod todo;
pub(in crate::runtime) use approval::register as register_exit_plan_mode;
pub(in crate::runtime) use gate::PlanModeGate;
pub(in crate::runtime) use note::{ModeNote, ModeNotes};
pub(in crate::runtime) use todo::register as register_todo_write;

/// Custom-entry type that records one mode switch.
const MODE_CUSTOM_TYPE: &str = "session_mode";

/// One persisted mode switch, stored as a clawcode custom entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::runtime) struct ModeChange {
    pub(in crate::runtime) mode: SessionMode,
    /// Turn the switch belongs to, so rebuilt notes keep the identity they had live.
    pub(in crate::runtime) turn_id: TurnId,
}

impl ClawcodeEntry for ModeChange {
    const CUSTOM_TYPE: &'static str = MODE_CUSTOM_TYPE;
}

/// A plan waiting for the user, with the route its answer takes.
struct PendingPlanApproval {
    request: PlanApprovalRequest,
    answer: tokio::sync::oneshot::Sender<PlanApprovalDecision>,
}

/// One plan's wait for its answer.
///
/// Dropping it forgets the plan however the wait ends, including with a Run
/// future dropped mid-wait, so a stale plan never blocks the next one.
pub(in crate::runtime) struct PlanApprovalWait<'a> {
    state: &'a SessionPlanState,
    request_id: String,
    answer: tokio::sync::oneshot::Receiver<PlanApprovalDecision>,
}

impl PlanApprovalWait<'_> {
    /// Waits for the client's decision; stopping the Run cancels the plan.
    ///
    /// The plan is forgotten by the time this returns.
    pub(in crate::runtime) async fn decision(
        mut self,
        cancellation: &CancellationToken,
    ) -> PlanApprovalDecision {
        tokio::select! {
            () = cancellation.cancelled() => PlanApprovalDecision::Cancelled,
            answer = &mut self.answer => {
                answer.unwrap_or(PlanApprovalDecision::Cancelled)
            }
        }
    }
}

impl Drop for PlanApprovalWait<'_> {
    /// Forgets the plan unless an answer already took it.
    fn drop(&mut self) {
        if let Ok(mut approval) = self.state.approval.lock() {
            approval.take_if(|pending| {
                pending.request.request_id == self.request_id
            });
        }
    }
}

/// The Session's current mode and the plan, if any, waiting for approval.
pub(in crate::runtime) struct SessionPlanState {
    mode: RwLock<SessionMode>,
    /// At most one plan waits at a time.
    approval: Mutex<Option<PendingPlanApproval>>,
}

impl SessionPlanState {
    /// Starts from the mode restored for the active branch.
    pub(in crate::runtime) fn new(mode: SessionMode) -> Self {
        Self {
            mode: RwLock::new(mode),
            approval: Mutex::new(None),
        }
    }

    /// Records a plan as waiting and returns its wait, or `None` when another
    /// plan is already waiting.
    pub(in crate::runtime) fn begin(
        &self,
        request: PlanApprovalRequest,
    ) -> Result<Option<PlanApprovalWait<'_>>, KernelError> {
        let mut approval = self.approval.lock()?;
        if approval.is_some() {
            return Ok(None);
        }
        let (answer, receiver) = tokio::sync::oneshot::channel();
        let request_id = request.request_id.clone();
        *approval = Some(PendingPlanApproval { request, answer });
        Ok(Some(PlanApprovalWait {
            state: self,
            request_id,
            answer: receiver,
        }))
    }

    /// Delivers a decision to the waiting plan it names.
    pub(in crate::runtime) fn resolve(
        &self,
        request_id: &str,
        decision: PlanApprovalDecision,
    ) -> Result<(), KernelError> {
        let mut approval = self.approval.lock()?;
        let Some(pending) = approval
            .take_if(|pending| pending.request.request_id == request_id)
        else {
            // Another client answered first, or the Run was stopped.
            tracing::info!(
                "ignored answer for plan approval {} that is no longer pending",
                request_id
            );
            return Err(KernelError::PlanApprovalResolved(request_id.into()));
        };
        pending.answer.send(decision).map_err(|_decision| {
            KernelError::PlanApprovalResolved(request_id.into())
        })
    }

    /// Returns the plan waiting for approval, if any.
    pub(in crate::runtime) fn pending(
        &self,
    ) -> Result<Option<PlanApprovalRequest>, KernelError> {
        Ok(self
            .approval
            .lock()?
            .as_ref()
            .map(|pending| pending.request.clone()))
    }

    /// Returns the mode future Turns will use.
    pub(in crate::runtime) fn mode(&self) -> Result<SessionMode, KernelError> {
        self.mode
            .read()
            .map(|mode| *mode)
            .map_err(KernelError::from)
    }

    /// Replaces the mode after its switch has been persisted.
    pub(in crate::runtime) fn replace_mode(
        &self,
        mode: SessionMode,
    ) -> Result<(), KernelError> {
        *self.mode.write()? = mode;
        Ok(())
    }

    /// Derives the mode a branch ends in from its last readable switch.
    pub(in crate::runtime) fn branch_mode(
        entries: &[SessionEntry],
    ) -> SessionMode {
        entries
            .iter()
            .rev()
            .find_map(ModeChange::from_entry)
            .map_or(SessionMode::Default, |change| change.mode)
    }
}

#[cfg(test)]
mod tests {
    use protocol::{PlanApprovalRequest, SessionMode, ToolCallId};

    use super::SessionPlanState;

    /// Builds one plan approval request.
    fn request(request_id: &str) -> PlanApprovalRequest {
        PlanApprovalRequest {
            request_id: request_id.to_string(),
            tool_call_id: ToolCallId::try_from("call-exit").expect("call id"),
            plan: "1. Write the file.".to_string(),
        }
    }

    /// A wait dropped mid-flight, such as with its Run future, forgets its plan.
    #[test]
    fn dropped_wait_forgets_the_plan() {
        let state = SessionPlanState::new(SessionMode::Plan);
        let wait = state
            .begin(request("request-1"))
            .expect("begin")
            .expect("no other plan waits");
        assert!(state.pending().expect("pending").is_some());
        drop(wait);
        assert!(state.pending().expect("pending").is_none());
        assert!(state.begin(request("request-2")).expect("begin").is_some());
    }
}

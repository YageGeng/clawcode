pub(crate) mod mailbox;
pub(crate) mod supervisor;
pub(crate) mod work_queue;
pub(crate) mod worker;

use std::sync::Arc;

pub(crate) use supervisor::AgentSupervisor;
pub(crate) use worker::KernelCollaborationRuntime;

use crate::{Result, context::SessionTaskContext};

/// Shareable collaboration session state that keeps one mailbox supervisor alive across turns.
#[derive(Clone)]
pub struct CollaborationSession {
    supervisor: Arc<AgentSupervisor>,
}

impl CollaborationSession {
    /// Builds a collaboration session backed by a fresh mailbox supervisor for one store.
    pub fn new(store: Arc<SessionTaskContext>) -> Self {
        Self {
            supervisor: Arc::new(AgentSupervisor::new(store)),
        }
    }

    /// Replays persisted collaboration events into the shared supervisor state.
    pub async fn replay_events(&self, events: &[store::SessionEvent]) -> Result<()> {
        self.supervisor.replay_events(events).await
    }

    /// Reuses an existing supervisor inside a shareable public handle.
    pub(crate) fn from_supervisor(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }

    /// Returns the underlying supervisor so new runtimes can share the same session graph.
    pub(crate) fn supervisor(&self) -> Arc<AgentSupervisor> {
        Arc::clone(&self.supervisor)
    }
}

impl From<crate::Error> for tools::Error {
    /// Narrows kernel runtime failures onto the tools-layer runtime error surface.
    fn from(error: crate::Error) -> Self {
        Self::Runtime {
            message: error.display_message().to_string(),
            stage: "collaboration-runtime".to_string(),
        }
    }
}

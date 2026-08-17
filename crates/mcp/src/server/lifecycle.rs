use protocol::{
    McpCatalog, McpCatalogRevisions, McpFailureStage, McpOAuthStatus,
    McpServerFailure, McpServerImplementation, McpServerState, McpServerStatus,
    TimestampMs,
};

use crate::McpError;

/// Owns one Server status and permits only valid lifecycle transitions.
#[derive(Debug)]
pub struct McpServerStateMachine {
    status: McpServerStatus,
}

impl McpServerStateMachine {
    /// Creates a state machine from one fully initialized status snapshot.
    pub fn new(status: McpServerStatus) -> Self {
        Self { status }
    }

    /// Returns the current immutable status value.
    #[must_use]
    pub fn status(&self) -> &McpServerStatus {
        &self.status
    }

    /// Replaces only the safe OAuth projection while retaining Server lifecycle state.
    pub fn set_oauth(&mut self, oauth: McpOAuthStatus) {
        self.status.oauth = oauth;
        self.status.updated_at_ms = TimestampMs::now();
    }

    /// Advances to one legal state while retaining all existing status details.
    pub fn transition(
        &mut self,
        next: McpServerState,
        updated_at_ms: TimestampMs,
    ) -> Result<&McpServerStatus, McpError> {
        let current = self.status.state;
        let allowed = matches!(
            (current, next),
            (McpServerState::Starting, McpServerState::Negotiating)
                | (McpServerState::Starting, McpServerState::Failed)
                | (McpServerState::Starting, McpServerState::Stopping)
                | (McpServerState::Negotiating, McpServerState::Discovering)
                | (McpServerState::Negotiating, McpServerState::Failed)
                | (McpServerState::Negotiating, McpServerState::Stopping)
                | (McpServerState::Discovering, McpServerState::Ready)
                | (McpServerState::Discovering, McpServerState::Failed)
                | (McpServerState::Discovering, McpServerState::Stopping)
                | (McpServerState::Ready, McpServerState::Degraded)
                | (McpServerState::Ready, McpServerState::Failed)
                | (McpServerState::Ready, McpServerState::Stopping)
                | (McpServerState::Degraded, McpServerState::Ready)
                | (McpServerState::Degraded, McpServerState::Failed)
                | (McpServerState::Degraded, McpServerState::Stopping)
                | (McpServerState::Failed, McpServerState::Starting)
                | (McpServerState::Failed, McpServerState::Stopping)
                | (McpServerState::Failed, McpServerState::Stopped)
                | (McpServerState::Stopping, McpServerState::Stopped)
                | (McpServerState::Stopping, McpServerState::Failed)
                | (McpServerState::Stopped, McpServerState::Starting)
        );
        if !allowed {
            return Err(McpError::InvalidStateTransition {
                from: current,
                to: next,
            });
        }
        self.status.state = next;
        self.status.updated_at_ms = updated_at_ms;
        Ok(&self.status)
    }

    /// Records one terminal failure through a validated transition.
    pub fn fail(
        &mut self,
        stage: McpFailureStage,
        message: String,
        occurred_at_ms: TimestampMs,
    ) -> Result<&McpServerStatus, McpError> {
        self.transition(McpServerState::Failed, occurred_at_ms)?;
        self.status.failure = Some(McpServerFailure {
            stage,
            message,
            occurred_at_ms,
        });
        Ok(&self.status)
    }

    /// Publishes one complete initial catalog and transitions discovery to Ready.
    pub fn ready(
        &mut self,
        implementation: Option<McpServerImplementation>,
        catalog: &McpCatalog,
        updated_at_ms: TimestampMs,
    ) -> Result<&McpServerStatus, McpError> {
        self.transition(McpServerState::Ready, updated_at_ms)?;
        self.status.implementation = implementation;
        self.status.counts = catalog.counts();
        self.status.revisions = McpCatalogRevisions {
            tools: u64::from(!catalog.tools.is_empty()),
            prompts: u64::from(!catalog.prompts.is_empty()),
            resources: u64::from(!catalog.resources.is_empty()),
            resource_templates: u64::from(
                !catalog.resource_templates.is_empty(),
            ),
        };
        self.status.failure = None;
        Ok(&self.status)
    }

    /// Publishes one successful dynamic refresh and recovers a degraded Server.
    pub fn catalog_refreshed(
        &mut self,
        catalog: &McpCatalog,
        updated_at_ms: TimestampMs,
    ) -> Result<&McpServerStatus, McpError> {
        if self.status.state == McpServerState::Degraded {
            self.transition(McpServerState::Ready, updated_at_ms)?;
        } else if self.status.state != McpServerState::Ready {
            return Err(McpError::InvalidStateTransition {
                from: self.status.state,
                to: McpServerState::Ready,
            });
        }
        self.status.counts = catalog.counts();
        self.status.revisions.tools =
            self.status.revisions.tools.saturating_add(1);
        self.status.revisions.prompts =
            self.status.revisions.prompts.saturating_add(1);
        self.status.revisions.resources =
            self.status.revisions.resources.saturating_add(1);
        self.status.revisions.resource_templates =
            self.status.revisions.resource_templates.saturating_add(1);
        self.status.failure = None;
        self.status.updated_at_ms = updated_at_ms;
        Ok(&self.status)
    }

    /// Marks dynamic synchronization degraded while retaining the last-good catalog.
    pub fn degrade(
        &mut self,
        message: String,
        occurred_at_ms: TimestampMs,
    ) -> Result<&McpServerStatus, McpError> {
        if self.status.state == McpServerState::Ready {
            self.transition(McpServerState::Degraded, occurred_at_ms)?;
        } else if self.status.state != McpServerState::Degraded {
            return Err(McpError::InvalidStateTransition {
                from: self.status.state,
                to: McpServerState::Degraded,
            });
        }
        self.status.failure = Some(McpServerFailure {
            stage: McpFailureStage::Synchronization,
            message,
            occurred_at_ms,
        });
        self.status.updated_at_ms = occurred_at_ms;
        Ok(&self.status)
    }
}

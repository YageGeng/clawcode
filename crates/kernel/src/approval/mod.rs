//! Approval policy and session-scoped approval cache.

mod store;

use std::sync::Mutex;

pub use protocol::{ApprovalMode, AskForApproval};
pub use store::{ApprovalStore, with_cached_approval};

/// Thread-safe approval policy for a session.
///
/// The mode can be changed at runtime via [`ApprovalPolicy::set_mode`].
pub struct ApprovalPolicy {
    mode: Mutex<ApprovalMode>,
    policy: Mutex<AskForApproval>,
}

impl ApprovalPolicy {
    /// Create a new policy with the given initial mode.
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            policy: Mutex::new(AskForApproval::from(mode)),
            mode: Mutex::new(mode),
        }
    }

    /// Create a new policy with both legacy mode and enhanced policy.
    pub fn new_with_policy(mode: ApprovalMode, policy: AskForApproval) -> Self {
        Self {
            mode: Mutex::new(mode),
            policy: Mutex::new(policy),
        }
    }

    /// Return the current approval mode.
    pub fn mode(&self) -> ApprovalMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the current enhanced approval policy.
    pub fn policy(&self) -> AskForApproval {
        *self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Change the approval mode at runtime.
    pub fn set_mode(&self, mode: ApprovalMode) {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
        *self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            AskForApproval::from(mode);
    }
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self::new(ApprovalMode::default())
    }
}

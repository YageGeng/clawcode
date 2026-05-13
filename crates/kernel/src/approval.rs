//! Approval module — controls whether tool invocations require user confirmation.
//!
//! [`ApprovalMode`] is defined in the config crate and read from the
//! configuration file. [`ApprovalPolicy`] wraps it with thread-safe
//! runtime mutability.

use std::sync::Mutex;

pub use protocol::ApprovalMode;

/// Thread-safe approval policy for a session.
///
/// The mode can be changed at runtime via [`ApprovalPolicy::set_mode`].
pub struct ApprovalPolicy {
    mode: Mutex<ApprovalMode>,
}

impl ApprovalPolicy {
    /// Create a new policy with the given initial mode.
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode: Mutex::new(mode),
        }
    }

    /// Return the current approval mode.
    pub fn mode(&self) -> ApprovalMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Change the approval mode at runtime.
    pub fn set_mode(&self, mode: ApprovalMode) {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
    }
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self::new(ApprovalMode::default())
    }
}

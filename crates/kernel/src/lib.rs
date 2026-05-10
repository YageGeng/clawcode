//! Core kernel abstractions for the workspace.

/// Represents the central kernel boundary for the workspace.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Kernel;

impl Kernel {
    /// Creates a new kernel instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

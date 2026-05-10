//! Provider abstractions that integrate with the workspace kernel.

use kernel::Kernel;

/// Describes a provider that can attach to the kernel boundary.
pub trait Provider {
    /// Returns the stable provider name.
    fn name(&self) -> &str;

    /// Attaches the provider to the supplied kernel boundary.
    fn attach(&self, kernel: &Kernel);
}

/// Provides a minimal provider implementation for workspace bootstrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticProvider {
    name: String,
}

impl StaticProvider {
    /// Creates a static provider instance with a stable name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Provider for StaticProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn attach(&self, _kernel: &Kernel) {
        // The bootstrap provider does not need initialization logic yet.
    }
}

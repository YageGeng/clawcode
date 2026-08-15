use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Project trust is being resolved before project resources are read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrustEvent {
    /// Server-side project directory being evaluated.
    pub cwd: PathBuf,
}

/// Reason extension-owned resources are being discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcesDiscoverReason {
    /// Initial discovery for a newly constructed session runtime.
    Startup,
}

/// Extension-owned skill and prompt roots are being discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcesDiscoverEvent {
    /// Server-side project directory used to resolve relative paths.
    pub cwd: PathBuf,
    /// Lifecycle reason for this discovery pass.
    pub reason: ResourcesDiscoverReason,
}

//! Application-level runtime configuration.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

const DEFAULT_RECOVERY_MAX_BATCH_SIZE: NonZeroUsize = NonZeroUsize::new(128)
    .expect("default recovery batch size must be positive");

/// Recovery transport settings grouped under the `recovery` section.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RecoveryConfig {
    /// Maximum number of queued ACP Session updates emitted in one JSON-RPC batch.
    pub max_batch_size: NonZeroUsize,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            max_batch_size: DEFAULT_RECOVERY_MAX_BATCH_SIZE,
        }
    }
}

/// Runtime settings grouped under the top-level `app` section.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppSectionConfig {
    /// ACP recovery replay batching policy.
    #[serde(default)]
    pub recovery: RecoveryConfig,
}

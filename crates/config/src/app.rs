//! Application-level runtime configuration.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

const DEFAULT_RECORY_MAX_BATCH_SIZE: NonZeroUsize =
    NonZeroUsize::new(128).expect("default recory batch size must be positive");

/// Recovery transport settings grouped under the intentionally named `recory` section.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RecoryConfig {
    /// Maximum number of queued ACP Session updates emitted in one JSON-RPC batch.
    pub max_batch_size: NonZeroUsize,
}

impl Default for RecoryConfig {
    fn default() -> Self {
        Self {
            max_batch_size: DEFAULT_RECORY_MAX_BATCH_SIZE,
        }
    }
}

/// Runtime settings grouped under the top-level `app` section.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppSectionConfig {
    /// ACP recovery replay batching policy.
    #[serde(default)]
    pub recory: RecoryConfig,
}

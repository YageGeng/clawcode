//! Kernel runtime configuration.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

/// Maximum sequential Turn policy for one Kernel Run.
#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq,
)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum TurnLimit {
    /// Allows a Run to continue until its model or caller terminates it.
    #[default]
    Unlimited,
    /// Stops a Run after the configured positive number of Turns.
    Limited {
        /// Maximum number of Turns accepted by one Run.
        turns: NonZeroUsize,
    },
}

/// Runtime policies applied by the Kernel.
#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq,
)]
pub struct KernelConfig {
    /// Maximum sequential Turn policy for one Run.
    #[serde(default)]
    pub max_turns: TurnLimit,
}

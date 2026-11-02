//! Subagent delegation configuration.

use serde::{Deserialize, Serialize};

use crate::kernel::QueuePlacement;

/// Delegation settings grouped under the top-level `subagent` section.
#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq,
)]
pub struct SubagentConfig {
    /// Lane a background subagent's reply joins while its owner is busy.
    ///
    /// Foreground `subagent` answers return as tool results and never queue.
    #[serde(default)]
    pub reply_as: QueuePlacement,
}

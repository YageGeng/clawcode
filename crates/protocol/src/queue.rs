use serde::{Deserialize, Serialize};

use crate::{AgentMessage, QueueId};

/// Position at which a queued message participates in an active run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueKind {
    /// Continues immediately after the active Turn settles.
    Steering,
    /// Starts another Turn only after the run would otherwise settle.
    FollowUp,
}

/// One fully formed message waiting to enter a Turn.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    /// Stable identifier used for removal and persistence correlation.
    pub queue_id: QueueId,
    /// Scheduling position relative to the active run.
    pub kind: QueueKind,
    /// Complete message identity, timing, and content accepted by the kernel.
    pub message: AgentMessage,
}

/// Queued messages grouped by their run scheduling behavior.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct PendingMessages {
    /// Messages consumed immediately after the active Turn.
    pub steering: Vec<QueuedMessage>,
    /// Messages consumed only when the run would otherwise settle.
    pub follow_up: Vec<QueuedMessage>,
}

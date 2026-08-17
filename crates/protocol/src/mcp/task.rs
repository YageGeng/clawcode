use serde::{Deserialize, Serialize};

use crate::TimestampMs;

/// Public state of one Modern MCP Task extension operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpTaskState {
    /// The Server is still processing the Task.
    Working,
    /// The Task requires additional Host or user input.
    InputRequired,
    /// The Task completed successfully.
    Completed,
    /// The Task completed with an error.
    Failed,
    /// The Host or Server cancelled the Task.
    Cancelled,
}

/// Safe Task status projected into Tool progress and ACP updates.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpTaskStatus {
    /// Server-assigned opaque Task identifier.
    pub task_id: String,
    /// Current Task lifecycle state.
    pub state: McpTaskState,
    /// Optional normalized progress from zero through one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub progress: Option<f64>,
    /// Optional safe human-readable status detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub message: Option<String>,
    /// Unix millisecond time of the latest Task update.
    pub updated_at_ms: TimestampMs,
}

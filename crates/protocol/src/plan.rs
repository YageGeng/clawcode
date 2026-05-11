//! Plan and task-progress types for structured agent output.

use serde::{Deserialize, Serialize};

/// A single entry in the agent's execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    /// Human-readable task name.
    pub name: String,
    /// Priority level for this task.
    pub priority: PlanPriority,
    /// Current execution status.
    pub status: PlanStatus,
}

/// Priority level for a plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    Low,
    Medium,
    High,
}

/// Execution status of a plan entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

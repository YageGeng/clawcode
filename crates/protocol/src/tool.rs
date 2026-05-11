//! Tool definition and execution status types.

use serde::{Deserialize, Serialize};

/// Tool definition registered with the agent kernel.
///
/// Describes a callable tool the LLM can invoke via function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name exposed to the model.
    pub name: String,
    /// Human-readable description sent to the model.
    pub description: String,
    /// JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// Execution status of a tool call within the agent kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Tool call requested but execution has not started.
    Pending,
    /// Tool is currently executing.
    InProgress,
    /// Tool execution completed successfully.
    Completed,
    /// Tool execution failed with an error.
    Failed,
}

//! Tool execution output types shared by built-in and external tools.

use protocol::FileChange;

/// Structured display output produced by a tool.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDisplayOutput {
    /// The tool has no structured display payload.
    None,
    /// The tool produced final before/after file states for frontend diff rendering.
    FileChanges(Vec<FileChange>),
}

/// Tool execution result split into model-facing text and display-only payloads.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    /// Short text result that is sent back to the model.
    pub model_output: String,
    /// Optional structured payload intended only for frontend display.
    pub display: ToolDisplayOutput,
}

impl ToolExecutionResult {
    /// Build a text-only result for tools without rich display payloads.
    #[must_use]
    pub fn text(model_output: impl Into<String>) -> Self {
        Self {
            model_output: model_output.into(),
            display: ToolDisplayOutput::None,
        }
    }
}

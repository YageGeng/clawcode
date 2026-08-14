use serde::{Deserialize, Serialize};

use crate::{ContentBlock, ToolCallId};

/// Limit that caused a pi-compatible tool output to be truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationLimit {
    /// The configured complete-line limit was reached first.
    Lines,
    /// The configured UTF-8 byte limit was reached first.
    Bytes,
}

/// Complete diagnostics for one pi-compatible output truncation operation.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct TruncationDetails {
    /// Truncated content returned to the model.
    pub content: String,
    /// Whether truncation occurred.
    pub truncated: bool,
    /// First limit that caused truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub truncated_by: Option<TruncationLimit>,
    /// Complete-line count in the original content.
    pub total_lines: usize,
    /// UTF-8 byte count in the original content.
    pub total_bytes: usize,
    /// Complete-line count returned to the model.
    pub output_lines: usize,
    /// UTF-8 byte count returned to the model.
    pub output_bytes: usize,
    /// Whether tail truncation retained only part of the final source line.
    pub last_line_partial: bool,
    /// Whether head truncation could not retain the first source line.
    pub first_line_exceeds_limit: bool,
    /// Applied complete-line limit.
    pub max_lines: usize,
    /// Applied UTF-8 byte limit.
    pub max_bytes: usize,
}

/// Tool-specific diagnostics retained without an untyped JSON details bag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultDetails {
    /// Diagnostics produced by a truncated file read.
    Read {
        /// Applied truncation and continuation measurements.
        truncation: TruncationDetails,
    },
    /// Diagnostics produced by bash output accumulation.
    Bash {
        /// Applied truncation when the visible tail was bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncation: Option<TruncationDetails>,
        /// Temporary file containing complete output when truncation occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_output_path: Option<String>,
    },
    /// Diff data produced after a successful exact-text edit.
    Edit {
        /// Absolute edited file path used by ACP's native diff update.
        path: String,
        /// Display-oriented diff with source line numbers.
        diff: String,
        /// Standard unified patch for the completed mutation.
        patch: String,
        /// First changed line in the resulting file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_changed_line: Option<usize>,
    },
}

/// Model-facing definition of a callable tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name exposed to the model.
    pub name: String,

    /// Human-readable description sent to the model.
    pub description: String,

    /// JSON Schema describing accepted arguments.
    pub parameters: serde_json::Value,
}

/// One tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Stable call identifier shared with its result.
    pub tool_call_id: ToolCallId,

    /// Registered tool name.
    pub name: String,

    /// JSON arguments supplied by the model.
    pub arguments: serde_json::Value,
}

/// Complete result returned for one tool invocation.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ToolResult {
    /// Stable call identifier being answered.
    pub tool_call_id: ToolCallId,

    /// Ordered model-visible result blocks.
    pub blocks: Vec<ContentBlock>,

    /// Whether tool execution failed.
    pub is_error: bool,

    /// Optional typed diagnostics used by rendering and protocol adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub details: Option<ToolResultDetails>,
}

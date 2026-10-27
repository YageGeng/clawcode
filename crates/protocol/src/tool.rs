use serde::{Deserialize, Serialize};

use crate::{ContentBlock, McpToolRef, ToolCallId};

/// Persisted preferences, independent from the current connection catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSelection {
    pub version: u8,
    pub loaded: std::collections::BTreeSet<String>,
    pub disabled: std::collections::BTreeSet<String>,
}

impl Default for ToolSelection {
    /// Starts with current exposure defaults and no explicit overrides.
    fn default() -> Self {
        Self {
            version: 1,
            loaded: Default::default(),
            disabled: Default::default(),
        }
    }
}

impl From<Vec<String>> for ToolSelection {
    /// Converts explicit loaded names without implicitly disabling future catalog entries.
    fn from(names: Vec<String>) -> Self {
        Self {
            loaded: names.into_iter().collect(),
            ..Self::default()
        }
    }
}

/// Host policy controlling model discovery independently from remote hints.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposure {
    #[default]
    Direct,
    Deferred,
    Hidden,
}

/// Host output policy applied after result hooks.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputPolicy {
    #[default]
    Preserve,
    Bounded,
    /// Complete local snapshot pages must remain valid after extension hooks.
    ResourcePage(McpResourcePageKind),
}

/// Resource envelope family used for snapshot pagination and post-hook validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpResourcePageKind {
    Resources,
    Templates,
}

impl McpResourcePageKind {
    /// Returns the canonical array key for this resource page family.
    pub const fn array_key(self) -> &'static str {
        match self {
            Self::Resources => "resources",
            Self::Templates => "resourceTemplates",
        }
    }
}

/// Untrusted server hints; absence is deliberately distinct from false.
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub open_world_hint: Option<bool>,
}

/// Host metadata kept outside the Provider's three-field tool definition.
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct ToolMetadata {
    #[serde(default)]
    #[builder(default)]
    pub exposure: ToolExposure,
    #[serde(default)]
    #[builder(default)]
    pub annotations: ToolAnnotations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    #[builder(default)]
    pub output_policy: ToolOutputPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub mcp_ref: Option<McpToolRef>,
}

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
    /// Policy disposition produced before a tool implementation is invoked.
    Blocked {
        /// Human-readable reason returned to the model and protocol clients.
        reason: String,
        /// Whether the policy requested termination after the current batch.
        terminate: bool,
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

    /// Independent output-budget diagnostics, compatible with existing tool details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub output: Option<ToolOutputInfo>,
}

/// Bounded reasons why a complete temporary output could not be retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputSaveFailure {
    Io,
    TooLarge,
    Cancelled,
}

/// Measurements and temporary-file provenance without retaining the full result twice.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutputInfo {
    pub original_text_bytes: usize,
    pub visible_text_bytes: usize,
    pub omitted_media: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub full_output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub save_failure: Option<ToolOutputSaveFailure>,
}

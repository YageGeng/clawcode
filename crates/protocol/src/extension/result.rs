use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, CompactionData, ContentBlock, ExtensionMessageDraft,
    ModelUsage, ToolCallId, ToolResult, ToolResultDetails,
};

/// Trust answer returned by a project-trust handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTrustDecision {
    /// The handler does not decide and later handlers may run.
    Undecided,
    /// Project-local resources may be loaded.
    Yes,
    /// Project-local resources must not be loaded.
    No,
}

/// One project-trust handler decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrustResult {
    /// Trust decision made by this handler.
    pub trusted: ProjectTrustDecision,
    /// Whether the host may persist the decision.
    pub remember: bool,
}

/// Resource paths contributed by one extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcesDiscoverResult {
    /// Additional server-side skill roots.
    pub skill_paths: Vec<PathBuf>,
    /// Additional server-side prompt-template roots.
    pub prompt_paths: Vec<PathBuf>,
}

/// Result of preprocessing one user input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputResult {
    /// Continue with the current input unchanged.
    Continue,
    /// Continue with replacement text.
    Transform {
        /// Text observed by later handlers and the input expansion chain.
        text: String,
    },
    /// Stop normal input and agent processing.
    Handled,
}

/// Optional replacement for the model-facing context of one request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextResult {
    /// Replacement messages, or no change when absent.
    pub messages: Option<Vec<AgentMessage>>,
}

/// Chained changes produced before an agent run starts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BeforeAgentStartResult {
    /// Ordered custom messages appended by this handler or composed runtime.
    pub messages: Vec<ExtensionMessageDraft>,
    /// Replacement system prompt observed by later handlers.
    pub system_prompt: Option<String>,
}

/// One explicit provider-header mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum HeaderMutation {
    /// Add or replace one header value.
    Set {
        /// Case-insensitive header name.
        name: String,
        /// Complete replacement value.
        value: String,
    },
    /// Remove one header.
    Remove {
        /// Case-insensitive header name.
        name: String,
    },
}

/// Ordered provider-header changes returned by one handler.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderPatch {
    /// Mutations applied in order.
    pub mutations: Vec<HeaderMutation>,
}

/// Cancellation decision shared by simple session-before points.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SessionCancelResult {
    /// Whether the pending operation must stop without changing state.
    pub cancel: bool,
}

/// Result returned before forking a session branch.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SessionBeforeForkResult {
    /// Whether the fork must be cancelled.
    pub cancel: bool,
    /// Whether conversation restoration should be skipped after forking.
    pub skip_conversation_restore: bool,
}

/// Result returned before compacting model context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBeforeCompactResult {
    /// Whether compaction must be cancelled.
    pub cancel: bool,
    /// Optional complete compaction supplied by the extension.
    pub compaction: Option<CompactionData>,
}

/// Complete extension-provided branch summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreeSummary {
    /// Model-facing summary text.
    pub summary: String,
    /// Extension-owned structured diagnostics.
    pub details: Option<serde_json::Value>,
    /// Optional usage from summary generation.
    pub usage: Option<ModelUsage>,
}

/// Result returned before navigating the session tree.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct SessionBeforeTreeResult {
    /// Whether navigation must be cancelled.
    #[builder(default)]
    pub cancel: bool,
    /// Optional complete branch summary.
    #[builder(default)]
    pub summary: Option<TreeSummary>,
    /// Optional replacement or appended summary instructions.
    #[builder(default)]
    pub custom_instructions: Option<String>,
    /// Whether custom instructions replace host defaults.
    #[builder(default)]
    pub replace_instructions: Option<bool>,
    /// Optional label attached to the summary entry.
    #[builder(default)]
    pub label: Option<String>,
}

/// Reason and termination behavior for a blocked tool call.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ToolBlock {
    /// Human-readable reason returned to the model.
    pub reason: String,
    /// Whether this result requests termination after the current tool batch.
    pub terminate: bool,
}

impl From<(ToolCallId, ToolBlock)> for ToolResult {
    /// Converts a policy block into a model-visible error while retaining its typed disposition.
    fn from((tool_call_id, block): (ToolCallId, ToolBlock)) -> Self {
        let reason = block.reason;
        Self::builder()
            .tool_call_id(tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: reason.clone(),
            }])
            .is_error(true)
            .details(ToolResultDetails::Blocked {
                reason,
                terminate: block.terminate,
            })
            .build()
    }
}

/// Result of preprocessing one parsed tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallResult {
    /// Continue with the current tool arguments.
    Continue,
    /// Replace arguments before later handlers and schema validation.
    Replace {
        /// Complete replacement argument object.
        arguments: serde_json::Value,
    },
    /// Block this tool without calling its implementation.
    Block(ToolBlock),
}

/// Optional replacements applied to a completed tool result.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ToolResultPatch {
    /// Replacement model-visible blocks.
    #[builder(default)]
    pub blocks: Option<Vec<ContentBlock>>,
    /// Replacement typed tool details.
    #[builder(default)]
    pub details: Option<ToolResultDetails>,
    /// Replacement error state.
    #[builder(default)]
    pub is_error: Option<bool>,
    /// Replacement tool-local usage.
    #[builder(default)]
    pub usage: Option<ModelUsage>,
}

/// Complete Pi-compatible result returned by user-bash execution or a replacement handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserBashDisposition {
    /// The command ran and the exit fields describe its process outcome.
    Completed,
    /// An extension policy prevented process creation.
    Blocked {
        /// Human-readable policy explanation displayed to the user.
        reason: String,
    },
}

/// Complete Pi-compatible result returned by user-bash execution or a replacement handler.
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
pub struct UserBashResult {
    /// Typed execution outcome independent from shell exit-code conventions.
    pub disposition: UserBashDisposition,
    /// Combined stdout and stderr in process-arrival order.
    pub output: String,
    /// Process exit code, absent when the process was terminated by a signal.
    #[builder(default)]
    pub exit_code: Option<i32>,
    /// Whether cancellation terminated the command.
    pub cancelled: bool,
    /// Whether visible output was reduced to Pi's bounded tail.
    pub truncated: bool,
    /// Complete output retained when truncation occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub full_output_path: Option<String>,
}

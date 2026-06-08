//! Hook protocol event types.

use serde::{Deserialize, Serialize};

use crate::TurnId;

/// Hook lifecycle event name.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookEventName {
    /// Runs before a tool executes.
    PreToolUse,
    /// Runs before user or policy approval is requested.
    PermissionRequest,
    /// Runs after a tool produces a result.
    PostToolUse,
    /// Runs before context compaction.
    PreCompact,
    /// Runs after context compaction.
    PostCompact,
    /// Runs when a session starts.
    SessionStart,
    /// Runs before a user prompt is submitted.
    UserPromptSubmit,
    /// Runs when a user-visible subagent starts.
    SubagentStart,
    /// Runs when a user-visible subagent stops.
    SubagentStop,
    /// Runs when a root turn stops.
    Stop,
}

/// Type of hook handler.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookHandlerType {
    /// Shell command handler.
    Command,
    /// Prompt handler parsed for compatibility.
    Prompt,
    /// Agent handler parsed for compatibility.
    Agent,
}

/// Runtime execution mode for a hook handler.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookExecutionMode {
    /// Synchronous command execution.
    Sync,
    /// Async mode parsed for compatibility.
    Async,
}

/// Scope that owns a hook run.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookScope {
    /// Thread-scoped hook run.
    Thread,
    /// Turn-scoped hook run.
    Turn,
}

/// Source layer that declared a hook.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    /// User-level hook config.
    User,
    /// Project-level hook config.
    Project,
}

/// Status of one hook command run.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookRunStatus {
    /// Hook run was announced and is still executing.
    Running,
    /// Hook run completed without blocking or stopping.
    Completed,
    /// Hook run failed.
    Failed,
    /// Hook run blocked the lifecycle operation.
    Blocked,
    /// Hook run stopped the lifecycle operation.
    Stopped,
}

/// Kind of hook output entry.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookOutputEntryKind {
    /// Warning text intended for display.
    Warning,
    /// Stop reason entry.
    Stop,
    /// Feedback shown to the model or user.
    Feedback,
    /// Additional model context.
    Context,
    /// Error text.
    Error,
}

/// One visible entry produced by a hook run.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HookOutputEntry {
    /// Entry classification.
    pub kind: HookOutputEntryKind,
    /// Entry text.
    pub text: String,
}

impl HookOutputEntry {
    /// Build a warning output entry.
    #[must_use]
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            kind: HookOutputEntryKind::Warning,
            text: text.into(),
        }
    }

    /// Build a stop output entry.
    #[must_use]
    pub fn stop(text: impl Into<String>) -> Self {
        Self {
            kind: HookOutputEntryKind::Stop,
            text: text.into(),
        }
    }

    /// Build a feedback output entry.
    #[must_use]
    pub fn feedback(text: impl Into<String>) -> Self {
        Self {
            kind: HookOutputEntryKind::Feedback,
            text: text.into(),
        }
    }

    /// Build a context output entry.
    #[must_use]
    pub fn context(text: impl Into<String>) -> Self {
        Self {
            kind: HookOutputEntryKind::Context,
            text: text.into(),
        }
    }

    /// Build an error output entry.
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: HookOutputEntryKind::Error,
            text: text.into(),
        }
    }
}

/// Stable summary for a hook run.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct HookRunSummary {
    /// Stable hook run id.
    pub id: String,
    /// Lifecycle event this run belongs to.
    pub event_name: HookEventName,
    /// Handler type.
    pub handler_type: HookHandlerType,
    /// Execution mode.
    pub execution_mode: HookExecutionMode,
    /// Run scope.
    pub scope: HookScope,
    /// Config file path that declared the hook.
    pub source_path: String,
    /// Config source layer.
    pub source: HookSource,
    /// Discovery order used for stable display.
    pub display_order: i64,
    /// Current run status.
    pub status: HookRunStatus,
    /// Optional status message configured by the handler.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Unix timestamp in seconds when the hook started.
    pub started_at: i64,
    /// Unix timestamp in seconds when the hook completed.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    /// Runtime duration in milliseconds.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Structured output entries.
    #[builder(default)]
    #[serde(default)]
    pub entries: Vec<HookOutputEntry>,
}

/// Completed hook run emitted by the kernel.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct HookCompletedEvent {
    /// Turn that owns this hook run.
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// Completed run summary.
    pub run: HookRunSummary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, SessionId};

    /// Serializes completed hook events using the public event envelope.
    #[test]
    fn serializes_hook_completed_event() {
        let event = Event::HookCompleted {
            session_id: SessionId::from("session-1"),
            completed: HookCompletedEvent::builder()
                .turn_id(crate::TurnId::from("turn-1"))
                .run(
                    HookRunSummary::builder()
                        .id("run-1".to_string())
                        .event_name(HookEventName::PreToolUse)
                        .handler_type(HookHandlerType::Command)
                        .execution_mode(HookExecutionMode::Sync)
                        .scope(HookScope::Turn)
                        .source_path("/repo/.claw/hooks.json".to_string())
                        .source(HookSource::Project)
                        .display_order(0)
                        .status(HookRunStatus::Completed)
                        .started_at(10)
                        .entries(vec![HookOutputEntry::context("context")])
                        .build(),
                )
                .build(),
        };

        let value =
            serde_json::to_value(event).expect("event should serialize");

        assert_eq!(value["event"], "hook_completed");
        assert_eq!(value["completed"]["run"]["event_name"], "pre_tool_use");
        assert_eq!(value["completed"]["run"]["entries"][0]["kind"], "context");
    }
}

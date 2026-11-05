//! Plan mode values shared by the kernel, the ACP adapter, and persisted settings.

use serde::{Deserialize, Serialize};

/// Model-facing name of the tool that submits a finished plan for approval.
pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";
/// Model-facing name of the tool that replaces the session's todo list.
pub const TODO_WRITE_TOOL: &str = "todo_write";

/// How freely the agent may act in a Session.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Every active tool may run.
    #[default]
    Default,
    /// Read-only research that ends with a plan submitted for approval.
    Plan,
}

/// A finished plan waiting for the user's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanApprovalRequest {
    /// Session-unique identity used to route the user's answer back.
    pub request_id: String,
    /// The `exit_plan_mode` call that submitted the plan.
    pub tool_call_id: crate::ToolCallId,
    /// The plan as Markdown.
    pub plan: String,
}

/// What the user decided about a submitted plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalDecision {
    /// Leave plan mode and carry out the plan.
    Approve,
    /// Stay in plan mode; the user will give feedback.
    KeepPlanning,
    /// The prompt was dismissed or the work was stopped.
    Cancelled,
}

/// One client's answer to a pending plan approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanApprovalResponse {
    /// Session that holds the pending approval.
    pub session_id: crate::SessionId,
    /// Request being answered.
    pub request_id: String,
    /// The user's decision.
    pub decision: PlanApprovalDecision,
}

/// Progress of one todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started yet.
    Pending,
    /// Being worked on now.
    InProgress,
    /// Finished.
    Completed,
}

/// One entry of the agent's todo list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// What needs doing, in imperative form.
    pub content: String,
    /// Where the item stands.
    pub status: TodoStatus,
    /// The same task in present-continuous form, shown while it is in progress.
    pub active_form: String,
}

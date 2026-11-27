//! Plan updates derived from plan-mode tool results.
//!
//! Only persisted result details feed these updates, so a live Session and a
//! replay publish the same plans even when an extension rewrote the arguments.

use std::collections::HashSet;

use agent_client_protocol::v2::schema as wire;
use protocol::{
    AgentEventPayload, MessageContent, TODO_WRITE_TOOL, TodoStatus,
    ToolResultDetails,
};

/// Stable id of the plan the user last approved; each approval replaces it.
const PLAN_MODE_PLAN_ID: &str = "plan-mode";
/// Stable id of the agent's todo list; each `todo_write` replaces it.
const TODOS_PLAN_ID: &str = "todos";

/// Borrows one tool result's details to publish the plan they carry, if any.
pub(crate) struct PlanUpdateOf<'a>(pub(crate) Option<&'a ToolResultDetails>);

impl PlanUpdateOf<'_> {
    /// Builds the plan update: an approved plan as Markdown, a todo list as items.
    pub(crate) fn update(
        &self,
        metadata: &wire::Meta,
    ) -> Option<wire::SessionUpdate> {
        let content = match self.0? {
            ToolResultDetails::PlanApproval {
                approved: true,
                plan: Some(plan),
            } => wire::PlanUpdateContent::markdown(PLAN_MODE_PLAN_ID, plan),
            ToolResultDetails::TodoList { todos } => {
                wire::PlanUpdateContent::items(
                    TODOS_PLAN_ID,
                    todos
                        .iter()
                        .map(|todo| {
                            let (content, status) = match todo.status {
                                TodoStatus::Pending => (
                                    &todo.content,
                                    wire::PlanEntryStatus::Pending,
                                ),
                                // The current item reads as what is happening now.
                                TodoStatus::InProgress => (
                                    &todo.active_form,
                                    wire::PlanEntryStatus::InProgress,
                                ),
                                TodoStatus::Completed => (
                                    &todo.content,
                                    wire::PlanEntryStatus::Completed,
                                ),
                            };
                            wire::PlanEntry::new(
                                content.clone(),
                                wire::PlanEntryPriority::Medium,
                                status,
                            )
                        })
                        .collect(),
                )
            }
            _ => return None,
        };
        Some(wire::SessionUpdate::PlanUpdate(
            wire::PlanUpdate::new(content).meta(metadata.clone()),
        ))
    }
}

/// The set of tool updates requested by one ACP client at initialization.
#[derive(Clone, Copy, Default)]
pub(crate) struct ToolCallHistory(pub(crate) bool);

impl ToolCallHistory {
    /// Includes todo tool calls only for clients displaying the full tool history.
    pub(crate) fn includes(self, update: &wire::SessionUpdate) -> bool {
        self.0
            || !matches!(
                update,
                wire::SessionUpdate::ToolCallUpdate(call)
                    if call.name.value().is_some_and(|name| name == TODO_WRITE_TOOL)
            )
    }
}

/// Tracks the identity of todo calls across patches that omit the tool name.
#[derive(Debug, Default)]
pub(crate) struct TodoToolCalls(HashSet<String>);

impl TodoToolCalls {
    /// Names every todo patch before journaling so even a mid-call resume can filter it.
    pub(crate) fn identify(
        &mut self,
        payload: &AgentEventPayload,
        updates: &mut [wire::SessionUpdate],
    ) {
        // Result details also identify successful calls in partial durable histories.
        let todo_result = match payload {
            AgentEventPayload::ToolExecutionEnd { result, .. } => {
                matches!(
                    result.details,
                    Some(ToolResultDetails::TodoList { .. })
                )
            }
            AgentEventPayload::MessageEnd { message } => matches!(
                message.content,
                MessageContent::ToolResult {
                    details: Some(ToolResultDetails::TodoList { .. }),
                    ..
                }
            ),
            _ => false,
        };
        for update in updates {
            let wire::SessionUpdate::ToolCallUpdate(call) = update else {
                continue;
            };
            if call
                .name
                .value()
                .is_some_and(|name| name == TODO_WRITE_TOOL)
            {
                self.0.insert(call.tool_call_id.to_string());
            } else if todo_result || self.0.contains(&*call.tool_call_id.0) {
                call.name =
                    agent_client_protocol::schema::MaybeUndefined::Value(
                        TODO_WRITE_TOOL.to_string(),
                    );
            }
        }
        // The persisted result message is the final patch, including for blocked calls.
        if let AgentEventPayload::MessageEnd { message } = payload
            && let MessageContent::ToolResult { tool_call_id, .. } =
                &message.content
        {
            self.0.remove(tool_call_id.as_str());
        }
    }
}

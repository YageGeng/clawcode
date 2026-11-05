//! Plan updates derived from plan-mode tool results.
//!
//! Only persisted result details feed these updates, so a live Session and a
//! replay publish the same plans even when an extension rewrote the arguments.

use std::collections::HashSet;

use agent_client_protocol::v2::schema as wire;
use protocol::{
    AgentEventPayload, ContentBlock, MessageContent, TODO_WRITE_TOOL,
    TodoStatus, ToolResultDetails,
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

    /// Returns whether the result belongs to `todo_write`, which shows no tool card.
    pub(crate) fn is_todo_list(&self) -> bool {
        matches!(self.0, Some(ToolResultDetails::TodoList { .. }))
    }
}

/// Tool calls kept off clients' tool cards because they belong to `todo_write`.
///
/// A failed or blocked call carries neither the tool name nor a todo list in its
/// result, so the call id seen when the call started is what identifies it.
#[derive(Debug, Default)]
pub(crate) struct HiddenToolCalls(HashSet<String>);

impl HiddenToolCalls {
    /// Remembers the `todo_write` calls an event announces and drops every tool
    /// card update for them, including the one announcing the call; plan
    /// updates and the rest of the event pass through.
    pub(crate) fn apply(
        &mut self,
        payload: &AgentEventPayload,
        updates: &mut Vec<wire::SessionUpdate>,
    ) {
        match payload {
            AgentEventPayload::ToolExecutionStart { call, .. }
                if call.name == TODO_WRITE_TOOL =>
            {
                self.0.insert(call.tool_call_id.to_string());
            }
            AgentEventPayload::MessageEnd { message } => {
                if let MessageContent::Assistant { blocks, .. } =
                    &message.content
                {
                    self.0.extend(blocks.iter().filter_map(
                        |block| match block {
                            ContentBlock::ToolCall {
                                tool_call_id,
                                name,
                                ..
                            } if name == TODO_WRITE_TOOL => {
                                Some(tool_call_id.to_string())
                            }
                            _ => None,
                        },
                    ));
                }
            }
            _ => {}
        }
        if !self.0.is_empty() {
            updates.retain(|update| {
                !matches!(
                    update,
                    wire::SessionUpdate::ToolCallUpdate(update)
                        if self.0.contains(&*update.tool_call_id.0)
                )
            });
        }
        // The result message is the call's last update, so forget it after it.
        if let AgentEventPayload::MessageEnd { message } = payload
            && let MessageContent::ToolResult { tool_call_id, .. } =
                &message.content
        {
            self.0.remove(tool_call_id.as_str());
        }
    }
}

//! `todo_write`: the agent's todo list, replaced whole on every call.

use std::sync::Arc;

use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    ContentBlock, TODO_WRITE_TOOL, TodoItem, ToolCall, ToolDefinition,
    ToolResult, ToolResultDetails,
};

use crate::runtime::{KernelError, Session};

/// Tells the model the list was taken and to keep working from it.
const UPDATED_TEXT: &str = "Todo list updated. Keep it current as you work: mark an item in_progress before starting it and completed as soon as it is done.";

/// Replaces the Session's todo list; clients show it as the agent's plan.
struct TodoWriteTool;

impl TodoWriteTool {
    /// Reads and checks the complete list a call carries.
    fn todos(call: &ToolCall) -> Result<Vec<TodoItem>, ToolError> {
        let invalid = |message: String| ToolError::InvalidArguments {
            tool: TODO_WRITE_TOOL.into(),
            message,
        };
        let todos: Vec<TodoItem> = call
            .arguments
            .get("todos")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| invalid(error.to_string()))?
            .ok_or_else(|| invalid("todos is required".into()))?;
        if let Some(position) = todos.iter().position(|todo| {
            todo.content.trim().is_empty() || todo.active_form.trim().is_empty()
        }) {
            return Err(invalid(format!(
                "item {} needs non-empty content and activeForm",
                position + 1
            )));
        }
        Ok(todos)
    }
}

#[async_trait]
impl AgentTool for TodoWriteTool {
    /// Declares the list format and when keeping one helps.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TODO_WRITE_TOOL.into(),
            description: "Keep a todo list for multi-step work: tasks of three or more steps, several requests at once, or new instructions mid-task. Skip it for simple tasks. Send the complete list each time. Mark an item in_progress before you start it, keep only one in_progress at a time, and mark it completed as soon as it is done.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The complete, updated todo list",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "The task, in imperative form" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                                "activeForm": { "type": "string", "description": "The task in present-continuous form, shown while in progress" }
                            },
                            "required": ["content", "status", "activeForm"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    /// Refuses lists with unknown statuses or blank text.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        Self::todos(call).map(drop)
    }

    /// Accepts the list; the details carry it to clients and replays.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let todos = Self::todos(&call)?;
        tracing::debug!(
            "updated todo list with {} items for session {}",
            todos.len(),
            context.session_id
        );
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: UPDATED_TEXT.into(),
            }])
            .is_error(false)
            .details(ToolResultDetails::TodoList { todos })
            .build())
    }
}

/// Registers `todo_write` as the kernel's own tool for one Session.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
) -> Result<(), KernelError> {
    runtime.tools.register_kernel(Arc::new(TodoWriteTool))?;
    Ok(())
}

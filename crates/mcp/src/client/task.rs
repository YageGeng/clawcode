//! Drives the MCP 2026-07-28 Tasks extension under the owning Tool deadline.

use std::time::Duration;

use protocol::{McpTaskState, McpTaskStatus, TimestampMs};
use rmcp::model::{
    CallToolResult, CancelTaskParams, CreateTaskResult, GetTaskParams, Task,
    TaskPayload, TaskStatus, UpdateTaskParams,
};

use super::mrtr::ToolCallDriver;
use crate::McpError;

/// Borrowed wire Task converted into the public transport-independent status.
struct TaskSnapshot<'a>(&'a Task);

impl TryFrom<TaskSnapshot<'_>> for McpTaskStatus {
    type Error = &'static str;

    /// Maps every known Tasks extension state without exposing wire-only fields.
    fn try_from(snapshot: TaskSnapshot<'_>) -> Result<Self, Self::Error> {
        let state = match snapshot.0.status {
            TaskStatus::Working => McpTaskState::Working,
            TaskStatus::InputRequired => McpTaskState::InputRequired,
            TaskStatus::Completed => McpTaskState::Completed,
            TaskStatus::Failed => McpTaskState::Failed,
            TaskStatus::Cancelled => McpTaskState::Cancelled,
            _ => return Err("unsupported MCP Task state"),
        };
        Ok(McpTaskStatus::builder()
            .task_id(snapshot.0.task_id.clone())
            .state(state)
            .message(snapshot.0.status_message.clone())
            .updated_at_ms(TimestampMs::now())
            .build())
    }
}

/// One asynchronous Task continuation sharing its parent Tool invocation policy.
pub(super) struct TaskDriver<'driver, 'client> {
    call: &'driver ToolCallDriver<'client>,
    deadline: tokio::time::Instant,
}

impl<'driver, 'client> TaskDriver<'driver, 'client> {
    /// Creates a Task driver that cannot outlive its originating Tool call.
    pub(super) fn new(
        call: &'driver ToolCallDriver<'client>,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self { call, deadline }
    }

    /// Polls, fulfills input, and returns the original Tool result at terminal completion.
    pub(super) async fn drive(
        &self,
        created: CreateTaskResult,
    ) -> Result<CallToolResult, McpError> {
        let task_id = created.task.task_id.clone();
        self.publish(&created.task)?;
        let mut poll_interval = created.task.poll_interval_ms.unwrap_or(250);
        loop {
            tokio::select! {
                _ = self.call.control.lifecycle.cancelled() => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestCancelled(
                        self.call.server_id.clone(),
                    ));
                }
                _ = self.call.control.cancellation.cancelled() => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestCancelled(
                        self.call.server_id.clone(),
                    ));
                }
                _ = tokio::time::sleep_until(self.deadline) => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestTimeout(
                        self.call.server_id.clone(),
                    ));
                }
                _ = tokio::time::sleep(Duration::from_millis(poll_interval)) => {}
            }

            let response = tokio::select! {
                _ = self.call.control.lifecycle.cancelled() => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestCancelled(
                        self.call.server_id.clone(),
                    ));
                }
                _ = self.call.control.cancellation.cancelled() => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestCancelled(
                        self.call.server_id.clone(),
                    ));
                }
                _ = tokio::time::sleep_until(self.deadline) => {
                    self.cancel(&task_id).await;
                    return Err(McpError::RequestTimeout(
                        self.call.server_id.clone(),
                    ));
                }
                response = self.call.peer.get_task(GetTaskParams::new(&task_id)) => {
                    response.map_err(|error| self.call.service_error(error))?
                }
            };
            let detailed = response.task;
            self.publish(&detailed.task)?;
            poll_interval = detailed.task.poll_interval_ms.unwrap_or(250);
            match detailed.payload {
                TaskPayload::Working => {}
                TaskPayload::InputRequired { input_requests } => {
                    let responses = self
                        .call
                        .fulfill_input_requests(input_requests, self.deadline)
                        .await?;
                    tokio::select! {
                        _ = self.call.control.lifecycle.cancelled() => {
                            self.cancel(&task_id).await;
                            return Err(McpError::RequestCancelled(
                                self.call.server_id.clone(),
                            ));
                        }
                        _ = self.call.control.cancellation.cancelled() => {
                            self.cancel(&task_id).await;
                            return Err(McpError::RequestCancelled(
                                self.call.server_id.clone(),
                            ));
                        }
                        _ = tokio::time::sleep_until(self.deadline) => {
                            self.cancel(&task_id).await;
                            return Err(McpError::RequestTimeout(
                                self.call.server_id.clone(),
                            ));
                        }
                        result = self.call.peer.update_task(
                            UpdateTaskParams::new(&task_id, responses),
                        ) => {
                            result.map_err(|error| self.call.service_error(error))?;
                        }
                    }
                }
                TaskPayload::Completed { result } => {
                    return serde_json::from_value(serde_json::Value::Object(
                        result,
                    ))
                    .map_err(|error| McpError::TaskFailed {
                        server_id: self.call.server_id.clone(),
                        task_id,
                        message: format!(
                            "completed Task returned an invalid Tool result: {error}"
                        ),
                    });
                }
                TaskPayload::Failed { error } => {
                    return Err(McpError::TaskFailed {
                        server_id: self.call.server_id.clone(),
                        task_id,
                        message: serde_json::Value::Object(error).to_string(),
                    });
                }
                TaskPayload::Cancelled => {
                    return Err(McpError::TaskCancelled {
                        server_id: self.call.server_id.clone(),
                        task_id,
                    });
                }
                _ => {
                    return Err(McpError::TaskFailed {
                        server_id: self.call.server_id.clone(),
                        task_id,
                        message: "unsupported MCP Task payload".to_string(),
                    });
                }
            }
        }
    }

    /// Publishes one safe normalized Task snapshot to the owning Tool observer.
    fn publish(&self, task: &Task) -> Result<(), McpError> {
        let status =
            McpTaskStatus::try_from(TaskSnapshot(task)).map_err(|message| {
                McpError::TaskFailed {
                    server_id: self.call.server_id.clone(),
                    task_id: task.task_id.clone(),
                    message: message.to_string(),
                }
            })?;
        self.call.control.progress.publish_task(status);
        Ok(())
    }

    /// Signals cooperative Server-side cancellation with a bounded best-effort request.
    async fn cancel(&self, task_id: &str) {
        let cancellation =
            self.call.peer.cancel_task(CancelTaskParams::new(task_id));
        match tokio::time::timeout(Duration::from_secs(1), cancellation).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                "MCP Task '{}' cancellation failed for server '{}': {}",
                task_id,
                self.call.server_id,
                error
            ),
            Err(_elapsed) => tracing::warn!(
                "MCP Task '{}' cancellation timed out for server '{}'",
                task_id,
                self.call.server_id
            ),
        }
    }
}

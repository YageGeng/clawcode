use crate::{
    Result,
    tools::{ToolCallRequest, ToolContext, ToolOutput, router::ToolRouter},
};
use futures_util::future::join_all;
use snafu::ResultExt;
use std::{collections::VecDeque, mem};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub handle_id: String,
    pub call: ToolCallRequest,
    pub output: ToolOutput,
    pub message: llm::completion::Message,
}

/// Structured batch report that preserves completed results even when a later call fails.
#[derive(Debug)]
pub enum ToolExecutionBatchReport {
    Completed(Vec<ToolExecutionResult>),
    Failed(Box<ToolExecutionFailure>),
}

/// Failure report for one execution batch, including any results completed before the error.
#[derive(Debug)]
pub struct ToolExecutionFailure {
    pub completed_results: Vec<ToolExecutionResult>,
    pub failed_result: ToolExecutionResult,
    pub failed_request: ToolExecutionRequest,
    pub error: crate::Error,
}

/// One queued execution unit with a stable runtime handle and its tool call payload.
#[derive(Debug, Clone)]
pub struct ToolExecutionRequest {
    pub handle_id: String,
    pub call: ToolCallRequest,
}

/// Queue of tool calls collected during one loop iteration and ready for execution.
#[derive(Debug, Clone, Default)]
pub struct ToolExecutionQueue {
    calls: VecDeque<ToolExecutionRequest>,
}

impl ToolExecutionQueue {
    /// Builds a queue from the provided tool calls while preserving their existing order.
    pub fn from_calls(calls: Vec<ToolCallRequest>) -> Self {
        Self {
            calls: calls
                .into_iter()
                .map(|call| ToolExecutionRequest {
                    handle_id: call.call_id.clone().unwrap_or_else(|| call.id.clone()),
                    call,
                })
                .collect(),
        }
    }

    /// Builds a queue from explicit execution requests while preserving their existing order.
    pub fn from_requests(calls: Vec<ToolExecutionRequest>) -> Self {
        Self {
            calls: calls.into(),
        }
    }

    /// Pops the next tool call that should execute.
    pub fn pop_front(&mut self) -> Option<ToolExecutionRequest> {
        self.calls.pop_front()
    }

    /// Returns the number of pending tool calls still waiting to execute.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    /// Returns true when the queue contains no pending tool calls.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Returns all queued calls in their preserved order.
    pub fn into_requests(self) -> Vec<ToolExecutionRequest> {
        self.calls.into_iter().collect()
    }
}

/// Selects how a queued batch of tool calls should be drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionMode {
    /// Run each tool call one after another in queue order.
    #[default]
    Serial,
    /// Dispatch all queued tool calls together and await their completion as one batch.
    Parallel,
}

pub struct ToolExecutor;

impl ToolExecutor {
    /// Executes all tool calls serially for the first milestone runtime.
    pub async fn execute_all(
        router: &ToolRouter,
        calls: Vec<ToolCallRequest>,
        context: ToolContext,
    ) -> Result<Vec<ToolExecutionResult>> {
        Self::execute_queue(router, ToolExecutionQueue::from_calls(calls), context).await
    }

    /// Executes a queued series of tool calls serially while preserving queue order.
    pub async fn execute_queue(
        router: &ToolRouter,
        queue: ToolExecutionQueue,
        context: ToolContext,
    ) -> Result<Vec<ToolExecutionResult>> {
        Self::execute_queue_with_mode(router, queue, context, ToolExecutionMode::Serial).await
    }

    /// Executes a queued series of tool calls using the selected drain strategy.
    pub async fn execute_queue_with_mode(
        router: &ToolRouter,
        queue: ToolExecutionQueue,
        context: ToolContext,
        mode: ToolExecutionMode,
    ) -> Result<Vec<ToolExecutionResult>> {
        Self::execute_queue_with_mode_and_cancellation(
            router,
            queue,
            context,
            mode,
            CancellationToken::new(),
        )
        .await
    }

    /// Executes a queued series of tool calls using the selected drain strategy and
    /// aborts the in-flight batch when the provided cancellation token is triggered.
    pub async fn execute_queue_with_mode_and_cancellation(
        router: &ToolRouter,
        mut queue: ToolExecutionQueue,
        context: ToolContext,
        mode: ToolExecutionMode,
        cancellation: CancellationToken,
    ) -> Result<Vec<ToolExecutionResult>> {
        match mode {
            ToolExecutionMode::Serial => {
                match Self::execute_serial(router, &mut queue, context, cancellation).await? {
                    ToolExecutionBatchReport::Completed(results) => Ok(results),
                    ToolExecutionBatchReport::Failed(failure) => Err(failure.error),
                }
            }
            ToolExecutionMode::Parallel => {
                match Self::execute_parallel(router, queue, context, cancellation).await? {
                    ToolExecutionBatchReport::Completed(results) => Ok(results),
                    ToolExecutionBatchReport::Failed(failure) => Err(failure.error),
                }
            }
        }
    }

    /// Executes a queued series of tool calls and returns a report that can retain partial success.
    pub async fn execute_queue_report_with_mode_and_cancellation(
        router: &ToolRouter,
        mut queue: ToolExecutionQueue,
        context: ToolContext,
        mode: ToolExecutionMode,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionBatchReport> {
        match mode {
            ToolExecutionMode::Serial => {
                Self::execute_serial(router, &mut queue, context, cancellation).await
            }
            ToolExecutionMode::Parallel => {
                Self::execute_parallel(router, queue, context, cancellation).await
            }
        }
    }

    /// Drains the queue one tool call at a time while preserving queue order.
    async fn execute_serial(
        router: &ToolRouter,
        queue: &mut ToolExecutionQueue,
        context: ToolContext,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionBatchReport> {
        let mut results = Vec::with_capacity(queue.len());
        while let Some(call) = queue.pop_front() {
            let result = tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(Self::cancelled_error());
                }
                result = Self::execute_one(router, call.clone(), context.clone()) => result,
            };

            match result {
                Ok(result) => results.push(result),
                Err(error) => {
                    let failure_result = Self::failure_response(&call, &error);
                    return Ok(ToolExecutionBatchReport::Failed(Box::new(
                        ToolExecutionFailure {
                            completed_results: results,
                            failed_result: failure_result,
                            failed_request: call,
                            error,
                        },
                    )));
                }
            }
        }

        Ok(ToolExecutionBatchReport::Completed(results))
    }

    /// Drains the queue as one concurrent batch while preserving submission order in the result.
    async fn execute_parallel(
        router: &ToolRouter,
        queue: ToolExecutionQueue,
        context: ToolContext,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionBatchReport> {
        let futures = queue
            .calls
            .into_iter()
            .map(|call| {
                let context = context.clone();
                async move {
                    let failed_request = call.clone();
                    match Self::execute_one(router, call, context).await {
                        Ok(result) => Ok(result),
                        Err(error) => Err((failed_request, error)),
                    }
                }
            })
            .collect::<Vec<_>>();

        tokio::select! {
            _ = cancellation.cancelled() => Err(Self::cancelled_error()),
            reports = join_all(futures) => {
                let mut completed_results = Vec::new();
                let mut failure: Option<ToolExecutionFailure> = None;
                for report in reports {
                    match report {
                        Ok(result) => completed_results.push(result),
                        Err((failed_request, error)) if failure.is_none() => {
                            let failed_output = Self::failure_response(&failed_request, &error);
                            failure = Some(ToolExecutionFailure {
                                completed_results: Vec::new(),
                                failed_result: failed_output,
                                failed_request,
                                error,
                            });
                        }
                        Err((_failed_request, _error)) => {}
                    }
                }

                if let Some(mut failure) = failure {
                    failure
                        .completed_results
                        .extend(mem::take(&mut completed_results));
                    Ok(ToolExecutionBatchReport::Failed(Box::new(failure)))
                } else {
                    Ok(ToolExecutionBatchReport::Completed(completed_results))
                }
            },
        }
    }

    /// Builds a model-visible failure output for one failed request.
    ///
    /// This keeps tool-call observability consistent by preserving a structured
    /// failure output payload for snapshots and error reporting.
    fn failure_response(
        request: &ToolExecutionRequest,
        error: &crate::Error,
    ) -> ToolExecutionResult {
        let output = ToolOutput::failure(error.display_message());
        ToolExecutionResult {
            handle_id: request.handle_id.clone(),
            call: request.call.clone(),
            output: output.clone(),
            message: Self::build_result_message(&request.call, &output),
        }
    }

    /// Builds a tool-result message from one execution output.
    fn build_result_message(
        request: &ToolCallRequest,
        output: &ToolOutput,
    ) -> llm::completion::Message {
        let mut content =
            llm::completion::message::ToolResultContent::from_tool_output(output.text.clone());
        if !output.structured.is_plain_text_equivalent(&output.text) {
            let structured_content = llm::completion::message::ToolResultContent::from_tool_output(
                output.structured.to_serde_value().to_string(),
            )
            .into_iter()
            .collect::<Vec<_>>();

            for item in structured_content {
                content.push(item);
            }
        }

        let call_id = request
            .call_id
            .clone()
            .unwrap_or_else(|| request.id.clone());
        llm::completion::Message::User {
            content: llm::one_or_many::OneOrMany::one(
                llm::completion::message::UserContent::tool_result_with_call_id(
                    request.id.clone(),
                    call_id,
                    content,
                ),
            ),
        }
    }

    /// Executes one tool call and converts its output into a tool-result message.
    async fn execute_one(
        router: &ToolRouter,
        request: ToolExecutionRequest,
        context: ToolContext,
    ) -> Result<ToolExecutionResult> {
        let ToolExecutionRequest { handle_id, call } = request;
        let output =
            router
                .dispatch(call.clone(), context)
                .await
                .context(crate::error::ToolSnafu {
                    stage: "dispatch-tool".to_string(),
                    inflight_snapshot: None,
                })?;

        let message = Self::build_result_message(&call, &output);

        Ok(ToolExecutionResult {
            handle_id,
            call,
            output,
            message,
        })
    }

    /// Builds a uniform runtime error for executor-level cancellation paths.
    fn cancelled_error() -> crate::Error {
        crate::Error::Runtime {
            message: "tool execution cancelled".to_string(),
            stage: "tool-executor-cancelled".to_string(),
            inflight_snapshot: None,
        }
    }
}

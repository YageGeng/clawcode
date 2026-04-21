use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kernel::tools::executor::{ToolExecutionMode, ToolExecutionQueue, ToolExecutor};
use kernel::tools::{
    Tool, ToolCallRequest, ToolContext, ToolInvocation, ToolMetadata, ToolOutput, ToolRouter,
    registry::ToolRegistryBuilder,
};
use serde_json::json;
use tokio::sync::{Barrier, Notify};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tools::{Error as ToolError, Result as ToolResult};

/// Echo tool used to verify queue-driven execution order.
struct TestEchoTool;

#[async_trait]
impl Tool for TestEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Echoes text so tool-executor queue tests can assert order."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .ok_or(ToolError::Runtime {
                message: "missing text argument".to_string(),
                stage: "tool-executor-test-parse-args".to_string(),
            })?;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: json!({ "text": text }),
        })
    }
}

/// Tool that waits on a shared barrier so tests can prove calls ran concurrently.
struct BarrierEchoTool {
    barrier: Arc<Barrier>,
}

#[async_trait]
impl Tool for BarrierEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Waits on a barrier before echoing text so tests can observe parallel execution."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .ok_or(ToolError::Runtime {
                message: "missing text argument".to_string(),
                stage: "tool-executor-barrier-parse-args".to_string(),
            })?;

        // Wait for both queued tool calls to arrive before allowing either to finish.
        self.barrier.wait().await;

        Ok(ToolOutput {
            text: text.to_string(),
            structured: json!({ "text": text }),
        })
    }
}

/// Tool that blocks forever after signalling it started so cancellation tests can
/// assert whether queued calls were launched before the executor was cancelled.
struct BlockingEchoTool {
    started: Arc<AtomicUsize>,
    started_notify: Arc<Notify>,
}

#[async_trait]
impl Tool for BlockingEchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Signals start, then waits forever so queue cancellation can drop the in-flight future."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo."
                }
            },
            "required": ["text"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let text = invocation
            .function_arguments()
            .and_then(|arguments| arguments.get("text"))
            .and_then(|value| value.as_str())
            .ok_or(ToolError::Runtime {
                message: "missing text argument".to_string(),
                stage: "tool-executor-blocking-parse-args".to_string(),
            })?;

        self.started.fetch_add(1, Ordering::SeqCst);
        self.started_notify.notify_waiters();

        // Keep the tool in flight until the executor cancels and drops this future.
        std::future::pending::<()>().await;

        #[allow(unreachable_code)]
        Ok(ToolOutput {
            text: text.to_string(),
            structured: json!({ "text": text }),
        })
    }
}

/// Waits until the blocking test tool has been started the expected number of times.
async fn wait_for_started_calls(started: &AtomicUsize, started_notify: &Notify, expected: usize) {
    loop {
        if started.load(Ordering::SeqCst) >= expected {
            return;
        }
        started_notify.notified().await;
    }
}

#[tokio::test]
async fn execute_queue_preserves_queue_order() {
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(TestEchoTool));
    let router = builder.build_router();
    let queue = ToolExecutionQueue::from_calls(vec![
        ToolCallRequest::new("call_2", "echo", json!({ "text": "second" })),
        ToolCallRequest::new("call_1", "echo", json!({ "text": "first" })),
    ]);

    let results = ToolExecutor::execute_queue(
        &router,
        queue,
        ToolContext::new(
            kernel::session::SessionId::new(),
            kernel::session::ThreadId::new(),
        ),
    )
    .await
    .unwrap();

    let outputs = results
        .into_iter()
        .map(|result| result.output.text)
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["second".to_string(), "first".to_string()]);
}

#[tokio::test]
async fn execute_queue_allows_empty_queues() {
    let router = ToolRouter::new(Arc::new(kernel::tools::ToolRegistry::default()), Vec::new());
    let results = ToolExecutor::execute_queue(
        &router,
        ToolExecutionQueue::default(),
        ToolContext::new(
            kernel::session::SessionId::new(),
            kernel::session::ThreadId::new(),
        ),
    )
    .await
    .unwrap();

    assert!(results.is_empty());
}

#[tokio::test]
async fn execute_queue_parallel_mode_runs_multiple_calls_together() {
    let barrier = Arc::new(Barrier::new(2));
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BarrierEchoTool {
        barrier: Arc::clone(&barrier),
    }));
    let router = builder.build_router();
    let queue = ToolExecutionQueue::from_calls(vec![
        ToolCallRequest::new("call_1", "echo", json!({ "text": "first" })),
        ToolCallRequest::new("call_2", "echo", json!({ "text": "second" })),
    ]);

    let results = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        ToolExecutor::execute_queue_with_mode(
            &router,
            queue,
            ToolContext::new(
                kernel::session::SessionId::new(),
                kernel::session::ThreadId::new(),
            ),
            ToolExecutionMode::Parallel,
        ),
    )
    .await
    .expect("parallel queue execution should not deadlock")
    .unwrap();

    let outputs = results
        .into_iter()
        .map(|result| result.output.text)
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["first".to_string(), "second".to_string()]);
}

#[tokio::test]
async fn execute_queue_cancellation_stops_serial_queue_before_later_calls_start() {
    let started = Arc::new(AtomicUsize::new(0));
    let started_notify = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        started_notify: Arc::clone(&started_notify),
    }));
    let router = builder.build_router();
    let queue = ToolExecutionQueue::from_calls(vec![
        ToolCallRequest::new("call_1", "echo", json!({ "text": "first" })),
        ToolCallRequest::new("call_2", "echo", json!({ "text": "second" })),
    ]);
    let cancellation = CancellationToken::new();
    let cancellation_for_task = cancellation.clone();

    let execution = tokio::spawn(async move {
        ToolExecutor::execute_queue_with_mode_and_cancellation(
            &router,
            queue,
            ToolContext::new(
                kernel::session::SessionId::new(),
                kernel::session::ThreadId::new(),
            ),
            ToolExecutionMode::Serial,
            cancellation_for_task,
        )
        .await
    });

    wait_for_started_calls(&started, &started_notify, 1).await;
    cancellation.cancel();

    let error = timeout(Duration::from_secs(1), execution)
        .await
        .expect("serial cancellation should not hang")
        .unwrap()
        .expect_err("serial queue should fail when cancelled");

    assert!(matches!(
        error,
        kernel::Error::Runtime { ref stage, .. } if stage == "tool-executor-cancelled"
    ));
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn execute_queue_cancellation_aborts_parallel_queue_without_hanging() {
    let started = Arc::new(AtomicUsize::new(0));
    let started_notify = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.push_handler_spec(Arc::new(BlockingEchoTool {
        started: Arc::clone(&started),
        started_notify: Arc::clone(&started_notify),
    }));
    let router = builder.build_router();
    let queue = ToolExecutionQueue::from_calls(vec![
        ToolCallRequest::new("call_1", "echo", json!({ "text": "first" })),
        ToolCallRequest::new("call_2", "echo", json!({ "text": "second" })),
    ]);
    let cancellation = CancellationToken::new();
    let cancellation_for_task = cancellation.clone();

    let execution = tokio::spawn(async move {
        ToolExecutor::execute_queue_with_mode_and_cancellation(
            &router,
            queue,
            ToolContext::new(
                kernel::session::SessionId::new(),
                kernel::session::ThreadId::new(),
            ),
            ToolExecutionMode::Parallel,
            cancellation_for_task,
        )
        .await
    });

    wait_for_started_calls(&started, &started_notify, 2).await;
    cancellation.cancel();

    let error = timeout(Duration::from_secs(1), execution)
        .await
        .expect("parallel cancellation should not hang")
        .unwrap()
        .expect_err("parallel queue should fail when cancelled");

    assert!(matches!(
        error,
        kernel::Error::Runtime { ref stage, .. } if stage == "tool-executor-cancelled"
    ));
    assert_eq!(started.load(Ordering::SeqCst), 2);
}

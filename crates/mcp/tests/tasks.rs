#![expect(
    deprecated,
    reason = "Task input requests may contain Roots in MCP 2026-07-28"
)]
#![expect(
    clippy::panic,
    reason = "protocol fixtures require exhaustive assertions"
)]

mod support;

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpError, McpHost, McpProgress, McpProgressSink,
    McpRequestControl, ModernClient,
};
use protocol::{
    McpHostRequest, McpHostResponse, McpRequestContext, McpRoot,
    McpRootsResult, McpTaskState, McpTaskStatus, McpToolRef, McpToolRequest,
    SessionId, TimestampMs, TraceId, TurnId,
};
use rmcp::model::{
    CallToolResult, ClientJsonRpcMessage, ClientRequest, CreateTaskResult,
    DetailedTask, DiscoverResult, GetTaskResult, Implementation, InputRequest,
    InputRequests, ListRootsRequest, ProtocolVersion, ServerCapabilities,
    ServerJsonRpcMessage, ServerResult, Task, TaskAckResult, TaskPayload,
    TaskStatus,
};
use rmcp::transport::{IntoTransport, Transport};
use tokio_util::sync::CancellationToken;

/// Host fixture that authorizes one Task input request.
struct RootsHost;

#[async_trait]
impl McpHost for RootsHost {
    /// Handles only the Roots callback exercised by this Task lifecycle.
    async fn handle(
        &self,
        request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        let McpHostRequest::Roots(_request) = request else {
            panic!("expected Roots request");
        };
        Ok(McpHostResponse::Roots(McpRootsResult {
            roots: vec![McpRoot {
                uri: "file:///workspace".to_string(),
                name: None,
            }],
        }))
    }
}

/// Progress fixture retaining the ordered public Task states.
struct CapturingProgress {
    states: Mutex<Vec<McpTaskState>>,
    task_started: tokio::sync::Notify,
}

impl McpProgressSink for CapturingProgress {
    /// Ignores numeric progress outside the Task assertions.
    fn publish(&self, _progress: McpProgress) {}

    /// Retains each normalized Task state in publication order.
    fn publish_task(&self, status: McpTaskStatus) {
        let state = status.state;
        self.states.lock().expect("progress lock").push(state);
        if state == McpTaskState::Working {
            self.task_started.notify_one();
        }
    }
}

/// Builds one correlated control for the complete Task lifecycle.
fn request_control(
    progress: Arc<dyn McpProgressSink>,
    cancellation: CancellationToken,
) -> McpRequestControl {
    McpRequestControl::builder()
        .context(
            McpRequestContext::builder()
                .server_id(support::server_id())
                .session_id(
                    SessionId::try_from("session-task").expect("Session id"),
                )
                .turn_id(TurnId::try_from("turn-task").expect("Turn id"))
                .trace_id(TraceId::try_from("trace-task").expect("Trace id"))
                .requested_at_ms(TimestampMs::now())
                .build(),
        )
        .cancellation(cancellation)
        .lifecycle(CancellationToken::new())
        .idle_timeout(Duration::from_secs(1))
        .total_timeout(Duration::from_secs(2))
        .max_rounds(NonZeroU32::new(2).expect("non-zero rounds"))
        .progress(progress)
        .build()
}

/// Creates stable wire metadata for one reference Task state.
fn task(status: TaskStatus) -> Task {
    Task::new(
        "task-1",
        status,
        "2026-08-17T00:00:00Z",
        "2026-08-17T00:00:01Z",
    )
    .with_poll_interval_ms(1)
}

/// Modern Tasks poll, fulfill input, update the Server, and return the final Tool result.
#[tokio::test]
async fn modern_tool_drives_task_to_completion() {
    let (server_transport, client_transport) = tokio::io::duplex(16_384);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(
                    DiscoverResult::new(
                        vec![ProtocolVersion::V_2026_07_28],
                        ServerCapabilities::builder()
                            .enable_tools()
                            .enable_tasks()
                            .build(),
                    )
                    .with_server_info(Implementation::new(
                        "task-server",
                        "1.0.0",
                    )),
                ),
                discover.id,
            ))
            .await
            .expect("discover response");

        let ClientJsonRpcMessage::Request(call) =
            server.receive().await.expect("Tool call")
        else {
            panic!("expected Tool call");
        };
        assert!(matches!(call.request, ClientRequest::CallToolRequest(_)));
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::CreateTaskResult(CreateTaskResult::new(task(
                    TaskStatus::Working,
                ))),
                call.id,
            ))
            .await
            .expect("Task creation");

        let ClientJsonRpcMessage::Request(get) =
            server.receive().await.expect("first tasks/get")
        else {
            panic!("expected tasks/get");
        };
        let ClientRequest::GetTaskRequest(get_request) = get.request else {
            panic!("expected tasks/get request");
        };
        assert_eq!(get_request.params.task_id, "task-1");
        let mut input_requests = InputRequests::new();
        input_requests.insert(
            "roots".to_string(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::GetTaskResult(GetTaskResult::new(
                    DetailedTask::new(
                        task(TaskStatus::InputRequired),
                        TaskPayload::InputRequired { input_requests },
                    ),
                )),
                get.id,
            ))
            .await
            .expect("input-required Task");

        let ClientJsonRpcMessage::Request(update) =
            server.receive().await.expect("tasks/update")
        else {
            panic!("expected tasks/update");
        };
        let ClientRequest::UpdateTaskRequest(update_request) = update.request
        else {
            panic!("expected tasks/update request");
        };
        assert_eq!(update_request.params.task_id, "task-1");
        assert_eq!(
            update_request.params.input_responses["roots"]["roots"][0]["uri"],
            "file:///workspace"
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::TaskAckResult(TaskAckResult::new()),
                update.id,
            ))
            .await
            .expect("Task update acknowledgement");

        let ClientJsonRpcMessage::Request(get) =
            server.receive().await.expect("second tasks/get")
        else {
            panic!("expected tasks/get");
        };
        let result = serde_json::to_value(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text("task done"),
        ]))
        .expect("Tool result JSON")
        .as_object()
        .expect("Tool result object")
        .clone();
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::GetTaskResult(GetTaskResult::new(
                    DetailedTask::new(
                        task(TaskStatus::Completed),
                        TaskPayload::Completed { result },
                    ),
                )),
                get.id,
            ))
            .await
            .expect("completed Task");
    });

    let client = ModernClient::connect_with_host(
        support::server_id(),
        client_transport,
        Arc::new(RootsHost),
    )
    .await
    .expect("Modern client");
    let progress = Arc::new(CapturingProgress {
        states: Mutex::new(Vec::new()),
        task_started: tokio::sync::Notify::new(),
    });
    let result = client
        .call_tool(
            McpToolRequest {
                reference: McpToolRef {
                    server_id: support::server_id(),
                    remote_name: "task_tool".to_string(),
                },
                arguments: serde_json::Map::new(),
            },
            request_control(
                Arc::clone(&progress) as Arc<dyn McpProgressSink>,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("Task Tool result");
    assert_eq!(result.content.len(), 1);
    assert_eq!(
        *progress.states.lock().expect("progress lock"),
        vec![
            McpTaskState::Working,
            McpTaskState::InputRequired,
            McpTaskState::Completed,
        ]
    );
    client.shutdown().await.expect("Modern shutdown");
    server_task.await.expect("reference Server");
}

/// Cancelling the owning Turn sends tasks/cancel before surfacing cancellation.
#[tokio::test]
async fn modern_task_cancellation_reaches_server() {
    let (server_transport, client_transport) = tokio::io::duplex(16_384);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(DiscoverResult::new(
                    vec![ProtocolVersion::V_2026_07_28],
                    ServerCapabilities::builder()
                        .enable_tools()
                        .enable_tasks()
                        .build(),
                )),
                discover.id,
            ))
            .await
            .expect("discover response");
        let ClientJsonRpcMessage::Request(call) =
            server.receive().await.expect("Tool call")
        else {
            panic!("expected Tool call");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::CreateTaskResult(CreateTaskResult::new(
                    task(TaskStatus::Working).with_poll_interval_ms(10_000),
                )),
                call.id,
            ))
            .await
            .expect("Task creation");
        let ClientJsonRpcMessage::Request(cancel) =
            server.receive().await.expect("tasks/cancel")
        else {
            panic!("expected tasks/cancel");
        };
        let ClientRequest::CancelTaskRequest(cancel_request) = cancel.request
        else {
            panic!("expected tasks/cancel request");
        };
        assert_eq!(cancel_request.params.task_id, "task-1");
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::TaskAckResult(TaskAckResult::new()),
                cancel.id,
            ))
            .await
            .expect("Task cancellation acknowledgement");
    });

    let client = ModernClient::connect_with_host(
        support::server_id(),
        client_transport,
        Arc::new(RootsHost),
    )
    .await
    .expect("Modern client");
    let cancellation = CancellationToken::new();
    let progress = Arc::new(CapturingProgress {
        states: Mutex::new(Vec::new()),
        task_started: tokio::sync::Notify::new(),
    });
    let call = client.call_tool(
        McpToolRequest {
            reference: McpToolRef {
                server_id: support::server_id(),
                remote_name: "task_tool".to_string(),
            },
            arguments: serde_json::Map::new(),
        },
        request_control(
            Arc::clone(&progress) as Arc<dyn McpProgressSink>,
            cancellation.clone(),
        ),
    );
    let cancel = async {
        progress.task_started.notified().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(call, cancel);
    assert!(matches!(result, Err(McpError::RequestCancelled(_))));
    client.shutdown().await.expect("Modern shutdown");
    server_task.await.expect("reference Server");
}

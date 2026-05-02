use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tools::builtin::collaboration::WaitAgentTool;
use tools::{
    AgentCommandAck, AgentRuntimeContext, AgentStatus, AgentSummary, CloseAgentRequest,
    CollaborationRuntime, ListAgentsRequest, ListAgentsResponse, MailboxEvent, MailboxEventKind,
    SendAgentInputRequest, SpawnAgentRequest, SpawnAgentResponse, ToolCallRequest, ToolContext,
    ToolHandler, ToolRouter, WaitAgentRequest, WaitAgentResponse, build_default_tool_registry_plan,
};

/// Fake collaboration runtime used to verify builtin tool dispatch wiring.
#[derive(Debug, Default)]
struct StubCollaborationRuntime;

#[async_trait]
impl CollaborationRuntime for StubCollaborationRuntime {
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> tools::Result<SpawnAgentResponse> {
        let name = request.name.clone();
        Ok(SpawnAgentResponse {
            agent: AgentSummary {
                agent_id: format!(
                    "agent-{}",
                    name.clone().unwrap_or_else(|| "child".to_string())
                ),
                parent_agent_id: request.origin.agent_id,
                thread_id: "thread-child".to_string(),
                path: "1".to_string(),
                name,
                status: AgentStatus::Running,
                pending_tasks: 1,
                unread_mailbox_events: 0,
            },
            started: request.task.is_some(),
        })
    }

    async fn send_agent_input(
        &self,
        request: SendAgentInputRequest,
    ) -> tools::Result<AgentCommandAck> {
        Ok(AgentCommandAck {
            agent: AgentSummary {
                agent_id: request.target,
                parent_agent_id: Some("root-agent".to_string()),
                thread_id: "thread-child".to_string(),
                path: "1".to_string(),
                name: Some("writer".to_string()),
                status: AgentStatus::Running,
                pending_tasks: 1,
                unread_mailbox_events: 0,
            },
            queued: true,
        })
    }

    async fn wait_agent(&self, _request: WaitAgentRequest) -> tools::Result<WaitAgentResponse> {
        Ok(WaitAgentResponse {
            timed_out: false,
            event: Some(MailboxEvent {
                event_id: 7,
                agent_id: "agent-writer".to_string(),
                path: "1".to_string(),
                event_kind: MailboxEventKind::Completed,
                message: "done".to_string(),
                status: AgentStatus::Completed,
            }),
        })
    }

    async fn close_agent(&self, request: CloseAgentRequest) -> tools::Result<AgentCommandAck> {
        Ok(AgentCommandAck {
            agent: AgentSummary {
                agent_id: request.target,
                parent_agent_id: Some("root-agent".to_string()),
                thread_id: "thread-child".to_string(),
                path: "1".to_string(),
                name: Some("writer".to_string()),
                status: AgentStatus::Closed,
                pending_tasks: 0,
                unread_mailbox_events: 0,
            },
            queued: false,
        })
    }

    async fn list_agents(&self, _request: ListAgentsRequest) -> tools::Result<ListAgentsResponse> {
        Ok(ListAgentsResponse {
            agents: vec![AgentSummary {
                agent_id: "agent-writer".to_string(),
                parent_agent_id: Some("root-agent".to_string()),
                thread_id: "thread-child".to_string(),
                path: "1".to_string(),
                name: Some("writer".to_string()),
                status: AgentStatus::Completed,
                pending_tasks: 0,
                unread_mailbox_events: 1,
            }],
        })
    }
}

/// Verifies the default tool registry includes the collaboration builtin handlers.
#[test]
fn default_tool_registry_plan_contains_collaboration_tools() {
    let plan = build_default_tool_registry_plan(temp_root("collaboration-plan"));
    let spec_names = plan
        .specs
        .iter()
        .map(|configured| configured.name().to_string())
        .collect::<Vec<_>>();

    assert!(spec_names.contains(&"spawn_agent".to_string()));
    assert!(spec_names.contains(&"send_agent_input".to_string()));
    assert!(spec_names.contains(&"wait_agent".to_string()));
    assert!(spec_names.contains(&"close_agent".to_string()));
    assert!(spec_names.contains(&"list_agents".to_string()));
}

/// Verifies the default `from_path` router preserves depth-aware visibility for `spawn_agent`.
#[tokio::test]
async fn default_router_hides_spawn_agent_when_depth_limit_is_reached() {
    let router = ToolRouter::from_path(temp_root("collaboration-depth-filter")).await;
    let tool_names = router
        .definitions_for_agent(&AgentRuntimeContext {
            subagent_depth: 1,
            max_subagent_depth: Some(1),
            ..AgentRuntimeContext::default()
        })
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();

    assert!(!tool_names.iter().any(|name| name == "spawn_agent"));
    assert!(tool_names.iter().any(|name| name == "wait_agent"));
}

/// Verifies collaboration tools fail clearly when no runtime is available in the tool context.
#[tokio::test]
async fn spawn_agent_requires_collaboration_runtime() {
    let router = ToolRouter::from_path(temp_root("spawn-agent-no-runtime")).await;
    let error = router
        .dispatch(
            ToolCallRequest::new(
                "call-spawn",
                "spawn_agent",
                serde_json::json!({
                    "name": "writer",
                    "task": "draft the summary"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .expect_err("spawn_agent should require a collaboration runtime");

    assert!(
        error
            .to_string()
            .contains("collaboration runtime is unavailable")
    );
}

/// Verifies spawn, send, wait, close, and list builtins serialize runtime results into JSON.
#[tokio::test]
async fn collaboration_tools_dispatch_through_runtime() {
    let router = ToolRouter::from_path(temp_root("collaboration-dispatch")).await;
    let context = ToolContext::new("session-1", "thread-1")
        .with_agent_runtime_context(AgentRuntimeContext {
            agent_id: Some("root-agent".to_string()),
            name: Some("planner".to_string()),
            system_prompt: Some("system".to_string()),
            cwd: Some("/workspace".to_string()),
            current_date: Some("2026-05-01".to_string()),
            timezone: Some("Asia/Shanghai".to_string()),
            ..AgentRuntimeContext::default()
        })
        .with_collaboration_runtime(Arc::new(StubCollaborationRuntime));

    let spawn_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-spawn",
                "spawn_agent",
                serde_json::json!({
                    "name": "writer",
                    "task": "draft the summary"
                }),
            ),
            context.clone(),
        )
        .await
        .expect("spawn_agent should succeed");
    let spawn_value = spawn_output.structured.to_serde_value();
    assert_eq!(spawn_value["agent"]["agent_id"], "agent-writer");
    assert_eq!(spawn_value["agent"]["path"], "1");
    assert_eq!(spawn_value["started"], true);

    let send_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-send",
                "send_agent_input",
                serde_json::json!({
                    "target": "agent-writer",
                    "input": "continue",
                    "interrupt": false
                }),
            ),
            context.clone(),
        )
        .await
        .expect("send_agent_input should succeed");
    let send_value = send_output.structured.to_serde_value();
    assert_eq!(send_value["agent"]["status"], "running");
    assert_eq!(send_value["queued"], true);

    let wait_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-wait",
                "wait_agent",
                serde_json::json!({
                    "targets": ["agent-writer"],
                    "timeout_ms": 10
                }),
            ),
            context.clone(),
        )
        .await
        .expect("wait_agent should succeed");
    let wait_value = wait_output.structured.to_serde_value();
    assert_eq!(wait_value["timed_out"], false);
    assert_eq!(wait_value["event"]["event_kind"], "completed");
    assert_eq!(wait_value["event"]["message"], "done");

    let list_output = router
        .dispatch(
            ToolCallRequest::new("call-list", "list_agents", serde_json::json!({})),
            context.clone(),
        )
        .await
        .expect("list_agents should succeed");
    let list_value = list_output.structured.to_serde_value();
    assert_eq!(list_value["agents"][0]["agent_id"], "agent-writer");
    assert_eq!(list_value["agents"][0]["unread_mailbox_events"], 1);

    let close_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-close",
                "close_agent",
                serde_json::json!({
                    "target": "agent-writer"
                }),
            ),
            context,
        )
        .await
        .expect("close_agent should succeed");
    let close_value = close_output.structured.to_serde_value();
    assert_eq!(close_value["agent"]["status"], "closed");
}

/// Verifies `wait_agent` overrides the default short tool timeout because the
/// mailbox wait is intentionally allowed to block much longer than normal tools.
#[test]
fn wait_agent_tool_uses_extended_timeout_metadata() {
    let metadata = WaitAgentTool.metadata();
    assert!(metadata.timeout > Duration::from_secs(10));
}

/// Builds an isolated temporary root path for the tools-router tests.
fn temp_root(label: &str) -> std::path::PathBuf {
    tools::test_temp_root("tools-collaboration", label)
}

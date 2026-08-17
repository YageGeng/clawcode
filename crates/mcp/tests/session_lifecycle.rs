mod session_support;

use std::sync::Arc;
use std::time::Duration;

use mcp::{McpFactory, McpSessionChange, SessionMcpFactory};
use protocol::{McpFailureStage, McpServerState};

/// Enabled Servers bootstrap concurrently while failures remain isolated and ordered.
#[tokio::test]
async fn bootstrap_is_concurrent_ordered_and_failure_isolated() {
    let connector = Arc::new(session_support::ScenarioConnector::new());
    let factory = SessionMcpFactory::new(
        vec![
            session_support::runtime_server("disabled", false),
            session_support::runtime_server("healthy-a", true),
            session_support::runtime_server("transport-failed", true),
            session_support::runtime_server("protocol-mismatch", true),
            session_support::runtime_server("discovery-failed", true),
            session_support::runtime_server("healthy-b", true),
        ],
        Arc::clone(&connector) as Arc<dyn mcp::McpConnector>,
    );

    let session = factory
        .create(session_support::session_request())
        .await
        .expect("single-Server failures must not fail Session creation");
    let snapshot = session.snapshot();

    assert!(connector.max_active() >= 2);
    assert_eq!(
        snapshot
            .servers
            .iter()
            .map(|status| status.server_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "disabled",
            "healthy-a",
            "transport-failed",
            "protocol-mismatch",
            "discovery-failed",
            "healthy-b",
        ]
    );
    assert_eq!(snapshot.servers[0].state, McpServerState::Disabled);
    assert_eq!(snapshot.servers[1].state, McpServerState::Ready);
    assert_eq!(snapshot.servers[2].state, McpServerState::Failed);
    assert_eq!(
        snapshot.servers[2]
            .failure
            .as_ref()
            .expect("transport failure")
            .stage,
        McpFailureStage::Transport
    );
    assert_eq!(
        snapshot.servers[3]
            .failure
            .as_ref()
            .expect("negotiation failure")
            .stage,
        McpFailureStage::Negotiation
    );
    assert_eq!(
        snapshot.servers[4]
            .failure
            .as_ref()
            .expect("discovery failure")
            .stage,
        McpFailureStage::Discovery
    );
    assert_eq!(snapshot.servers[5].state, McpServerState::Ready);
    assert_eq!(snapshot.catalog.tools.len(), 2);
    assert!(snapshot.revision > 0);

    let mut events = session.subscribe();
    session.shutdown().await.expect("Session shutdown");
    let event = loop {
        let event = events.recv().await.expect("shutdown Snapshot event");
        if event.change == McpSessionChange::Shutdown {
            break event;
        }
    };
    assert_eq!(event.change, McpSessionChange::Shutdown);
    assert!(event.revision > snapshot.revision);
}

/// Initial discovery obeys the configured request deadline and isolates the failed Server.
#[tokio::test]
async fn bootstrap_bounds_initial_discovery() {
    let connector = Arc::new(session_support::ScenarioConnector::new());
    let mut server = session_support::runtime_server("discovery-hangs", true);
    server.request_timeout = Duration::from_millis(25);
    let factory = SessionMcpFactory::new(
        vec![server],
        connector as Arc<dyn mcp::McpConnector>,
    );

    let session = tokio::time::timeout(
        Duration::from_millis(250),
        factory.create(session_support::session_request()),
    )
    .await
    .expect("bounded discovery must settle Session bootstrap")
    .expect("discovery failure remains Server-local");
    let status = &session.snapshot().servers[0];
    assert_eq!(status.state, McpServerState::Failed);
    assert_eq!(
        status.failure.as_ref().expect("discovery timeout").stage,
        McpFailureStage::Discovery
    );
}

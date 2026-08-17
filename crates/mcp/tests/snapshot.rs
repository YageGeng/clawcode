use protocol::{
    McpCapabilityCounts, McpCatalogRevisions, McpOAuthStatus,
    McpProtocolVersion, McpServerId, McpServerState, McpServerStatus,
    McpTransportKind, TimestampMs,
};

use mcp::McpServerStateMachine;

/// State transitions reject lifecycle jumps that would publish impossible status.
#[test]
fn server_state_machine_rejects_illegal_transition() {
    let status = McpServerStatus::builder()
        .server_id(McpServerId::try_from("state-machine").expect("Server id"))
        .protocol(McpProtocolVersion::V2025_11_25)
        .transport(McpTransportKind::Stdio)
        .state(McpServerState::Starting)
        .revisions(McpCatalogRevisions::default())
        .counts(McpCapabilityCounts::default())
        .oauth(McpOAuthStatus::default())
        .updated_at_ms(TimestampMs::from(1))
        .build();
    let mut machine = McpServerStateMachine::new(status);

    machine
        .transition(McpServerState::Ready, TimestampMs::from(2))
        .expect_err("Starting cannot skip negotiation and discovery");
    assert_eq!(machine.status().state, McpServerState::Starting);
}

use protocol::McpServerId;
use rmcp::model::ServerCapabilities;

/// Returns the stable Server identifier shared by lifecycle tests.
pub fn server_id() -> McpServerId {
    McpServerId::try_from("lifecycle-server").expect("valid test Server id")
}

/// Returns a Server capability declaration that permits only Tool discovery.
#[allow(
    dead_code,
    reason = "each integration-test crate compiles this shared fixture independently"
)]
pub fn tools_capabilities() -> ServerCapabilities {
    ServerCapabilities::builder().enable_tools().build()
}

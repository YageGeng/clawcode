//! Host callback boundary implemented by the Kernel in a later composition layer.

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{McpHostRequest, McpHostResponse};

use crate::McpError;

/// Handles Server-to-client Sampling, Elicitation, Roots, and authorization requests.
#[async_trait]
pub trait McpHost: Send + Sync {
    /// Routes one transport-independent Host request to the owning Agent Session.
    async fn handle(
        &self,
        request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError>;
}

/// Rejects callbacks for standalone clients that did not install a Kernel Host.
struct UnavailableHost;

#[async_trait]
impl McpHost for UnavailableHost {
    /// Rejects every callback because no application Host was supplied.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host(
            "no MCP Host was configured for this client".to_string(),
        ))
    }
}

/// Returns the non-advertising Host used by standalone lifecycle clients.
pub(crate) fn unavailable_host() -> Arc<dyn McpHost> {
    Arc::new(UnavailableHost)
}

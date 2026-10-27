//! Cancellable single-round Tool requests with bounded progress and total deadlines.
use super::invocation::InvocationRegistry;
use crate::{McpError, McpProgress, McpRequestControl};
use futures::StreamExt;
use protocol::McpServerId;
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientRequest, ServerResult,
};
use rmcp::service::{Peer, PeerRequestOptions, RoleClient};

/// Borrowed dependencies for one Tool request and its Turn correlation.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct ToolRequest<'a> {
    server_id: &'a McpServerId,
    peer: &'a Peer<RoleClient>,
    progress: &'a ProgressDispatcher,
    invocations: &'a InvocationRegistry,
    control: McpRequestControl,
}

impl ToolRequest<'_> {
    /// Executes one request without replaying a potentially side-effectful tool call.
    pub(super) async fn call(
        self,
        params: CallToolRequestParams,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        let deadline = tokio::time::Instant::now() + self.control.total_timeout;
        let invocation = self.invocations.begin(self.control.context.clone());
        let request =
            ClientRequest::CallToolRequest(CallToolRequest::new(params));
        // Queue admission belongs to the same deadline and cancellation scope as the response.
        let mut handle = tokio::select! {
            biased;
            () = self.control.lifecycle.cancelled() => return Err(McpError::RequestCancelled(self.server_id.clone())),
            () = self.control.cancellation.cancelled() => return Err(McpError::RequestCancelled(self.server_id.clone())),
            () = tokio::time::sleep_until(deadline) => return Err(McpError::RequestTimeout(self.server_id.clone())),
            () = tokio::time::sleep(self.control.idle_timeout) => return Err(McpError::RequestTimeout(self.server_id.clone())),
            result = self.peer.send_cancellable_request(request, PeerRequestOptions::no_options()) => result.map_err(|error| self.service_error(error))?,
        };
        invocation.associate(handle.id.clone());
        let mut progress =
            self.progress.subscribe(handle.progress_token.clone()).await;
        let mut progress_open = true;
        let idle = tokio::time::sleep(self.control.idle_timeout);
        let total = tokio::time::sleep_until(deadline);
        tokio::pin!(idle);
        tokio::pin!(total);
        let response = loop {
            tokio::select! {
                biased;
                _ = self.control.lifecycle.cancelled() => break Err(("MCP lifecycle stopped", McpError::RequestCancelled(self.server_id.clone()))),
                _ = self.control.cancellation.cancelled() => break Err(("owning Turn cancelled", McpError::RequestCancelled(self.server_id.clone()))),
                // Expired deadlines outrank queued replies and progress, including a continuously ready stream.
                _ = &mut total => break Err(("request total timeout", McpError::RequestTimeout(self.server_id.clone()))),
                _ = &mut idle => break Err(("request idle timeout", McpError::RequestTimeout(self.server_id.clone()))),
                response = &mut handle.rx => {
                    break Ok(response
                        .map_err(|error| McpError::Service {
                            server_id: self.server_id.clone(),
                            message: error.to_string(),
                        })?
                        .map_err(|error| self.service_error(error))?);
                }
                notification = progress.next(), if progress_open => {
                    if let Some(notification) = notification {
                        self.control.progress.publish(McpProgress {
                            progress: notification.progress,
                            total: notification.total,
                            message: notification.message,
                        });
                        idle.as_mut().reset(tokio::time::Instant::now() + self.control.idle_timeout);
                    } else {
                        // A closed progress stream must not remain a permanently ready branch.
                        progress_open = false;
                    }
                }
            }
        };
        let response = match response {
            Ok(response) => response,
            Err((reason, error)) => {
                // Release the bounded progress queue and Host association before attempting the cancellation notice.
                drop(progress);
                drop(invocation);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    handle.cancel(Some(reason.to_string())),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(notice_error)) => tracing::debug!(
                        "MCP cancellation notice failed for server {}: {}",
                        self.server_id,
                        notice_error
                    ),
                    Err(_elapsed) => tracing::debug!(
                        "MCP cancellation notice timed out for server {}",
                        self.server_id
                    ),
                }
                return Err(error);
            }
        };
        match response {
            ServerResult::CallToolResult(result) => Ok(result),
            _ => Err(McpError::Service {
                server_id: self.server_id.clone(),
                message: "unexpected response to tools/call".to_string(),
            }),
        }
    }

    /// Attaches the connection identity to request errors.
    fn service_error(&self, error: rmcp::service::ServiceError) -> McpError {
        McpError::Service {
            server_id: self.server_id.clone(),
            message: error.to_string(),
        }
    }
}

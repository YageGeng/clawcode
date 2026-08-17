//! Drives Modern multi-round Tool requests without weakening cancellation or progress semantics.
#![allow(
    deprecated,
    reason = "MRTR input requests retain Roots and Sampling callbacks required by MCP 2026-07-28"
)]

use std::sync::Arc;

use futures::StreamExt;
use protocol::McpServerId;
use rmcp::ClientHandler;
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResponse, ClientRequest,
    InputRequest, InputRequiredResult, InputResponses, RequestId, ServerResult,
};
use rmcp::service::{Peer, PeerRequestOptions, RequestContext, RoleClient};

use super::handler::McpLifecycleHandler;
use super::invocation::{InvocationLease, InvocationRegistry};
use super::task::TaskDriver;
use crate::{McpError, McpProgress, McpRequestControl};

/// One bounded Tool invocation spanning all MRTR attempts and nested Host callbacks.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct ToolCallDriver<'a> {
    pub(super) server_id: &'a McpServerId,
    pub(super) peer: &'a Peer<RoleClient>,
    handler: &'a McpLifecycleHandler,
    progress: &'a ProgressDispatcher,
    invocations: &'a InvocationRegistry,
    supports_tasks: bool,
    pub(super) control: McpRequestControl,
}

impl ToolCallDriver<'_> {
    /// Drives input-required retries under one total deadline and one correlation lease.
    pub(super) async fn call(
        self,
        mut params: CallToolRequestParams,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        let deadline = tokio::time::Instant::now() + self.control.total_timeout;
        // Register before transport dispatch because a Server may issue a nested Host request
        // before the first request handle exposes its generated JSON-RPC identifier.
        let invocation = self.invocations.begin(self.control.context.clone());
        let mut state_only_rounds = 0_u32;
        for _round in 0..self.control.max_rounds.get() {
            match self
                .request_round(params.clone(), &invocation, deadline)
                .await?
            {
                CallToolResponse::Complete(result) => return Ok(result),
                CallToolResponse::InputRequired(result) => {
                    let had_requests = result
                        .input_requests
                        .as_ref()
                        .is_some_and(|requests| !requests.is_empty());
                    if !had_requests && result.request_state.is_none() {
                        return Err(self.protocol_error(
                            "input_required omitted both inputRequests and requestState",
                        ));
                    }
                    let responses =
                        self.fulfill_input_required(result, deadline).await?;
                    if had_requests {
                        state_only_rounds = 0;
                    } else {
                        self.wait_state_only(state_only_rounds, deadline)
                            .await?;
                        state_only_rounds = state_only_rounds.saturating_add(1);
                    }
                    params.input_responses =
                        (!responses.0.is_empty()).then_some(responses.0);
                    params.request_state = responses.1;
                }
                CallToolResponse::Task(task) => {
                    if !self.supports_tasks {
                        return Err(McpError::CapabilityUnavailable {
                            server_id: self.server_id.clone(),
                            capability: "tasks",
                        });
                    }
                    return TaskDriver::new(&self, deadline).drive(task).await;
                }
                _ => {
                    return Err(self.protocol_error(
                        "unsupported response to Modern tools/call",
                    ));
                }
            }
        }
        Err(McpError::MrtrRoundsExceeded {
            server_id: self.server_id.clone(),
            max_rounds: self.control.max_rounds,
        })
    }

    /// Sends one cancellable request attempt while forwarding progress and enforcing deadlines.
    async fn request_round(
        &self,
        params: CallToolRequestParams,
        invocation: &InvocationLease,
        deadline: tokio::time::Instant,
    ) -> Result<CallToolResponse, McpError> {
        let request =
            ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let mut handle = self
            .peer
            .send_cancellable_request(request, PeerRequestOptions::no_options())
            .await
            .map_err(|error| self.service_error(error))?;
        invocation.associate(handle.id.clone());
        let mut progress =
            self.progress.subscribe(handle.progress_token.clone()).await;
        let idle = tokio::time::sleep(self.control.idle_timeout);
        let total = tokio::time::sleep_until(deadline);
        tokio::pin!(idle);
        tokio::pin!(total);
        let response = loop {
            tokio::select! {
                biased;
                _ = self.control.lifecycle.cancelled() => {
                    handle.cancel(Some("MCP lifecycle stopped".to_string()))
                        .await
                        .map_err(|error| self.service_error(error))?;
                    return Err(McpError::RequestCancelled(self.server_id.clone()));
                }
                _ = self.control.cancellation.cancelled() => {
                    handle.cancel(Some("owning Turn cancelled".to_string()))
                        .await
                        .map_err(|error| self.service_error(error))?;
                    return Err(McpError::RequestCancelled(self.server_id.clone()));
                }
                response = &mut handle.rx => {
                    break response
                        .map_err(|error| McpError::Service {
                            server_id: self.server_id.clone(),
                            message: error.to_string(),
                        })?
                        .map_err(|error| self.service_error(error))?;
                }
                notification = progress.next() => {
                    if let Some(notification) = notification {
                        self.control.progress.publish(McpProgress {
                            progress: notification.progress,
                            total: notification.total,
                            message: notification.message,
                        });
                        idle.as_mut().reset(
                            tokio::time::Instant::now() + self.control.idle_timeout,
                        );
                    }
                }
                _ = &mut idle => {
                    handle.cancel(Some("request idle timeout".to_string()))
                        .await
                        .map_err(|error| self.service_error(error))?;
                    return Err(McpError::RequestTimeout(self.server_id.clone()));
                }
                _ = &mut total => {
                    handle.cancel(Some("request total timeout".to_string()))
                        .await
                        .map_err(|error| self.service_error(error))?;
                    return Err(McpError::RequestTimeout(self.server_id.clone()));
                }
            }
        };
        match response {
            ServerResult::CallToolResult(result) => {
                Ok(CallToolResponse::Complete(result))
            }
            ServerResult::InputRequiredResult(result) => {
                Ok(CallToolResponse::InputRequired(result))
            }
            ServerResult::CreateTaskResult(result) => {
                Ok(CallToolResponse::Task(result))
            }
            _ => Err(self.protocol_error("unexpected response to tools/call")),
        }
    }

    /// Resolves every nested Host request and retains the opaque state for the retry.
    async fn fulfill_input_required(
        &self,
        result: InputRequiredResult,
        deadline: tokio::time::Instant,
    ) -> Result<(InputResponses, Option<String>), McpError> {
        let requests = result.input_requests.unwrap_or_default();
        let responses = self.fulfill_input_requests(requests, deadline).await?;
        Ok((responses, result.request_state))
    }

    /// Resolves one MRTR or Task input batch through the correlated Host.
    pub(super) async fn fulfill_input_requests(
        &self,
        requests: rmcp::model::InputRequests,
        deadline: tokio::time::Instant,
    ) -> Result<InputResponses, McpError> {
        let fulfillment = async {
            let mut responses = InputResponses::new();
            for (key, request) in requests {
                let response =
                    self.fulfill_input_request(&key, request).await?;
                responses.insert(key, response);
            }
            Ok::<InputResponses, McpError>(responses)
        };
        let responses = tokio::select! {
            _ = self.control.lifecycle.cancelled() => {
                return Err(McpError::RequestCancelled(self.server_id.clone()));
            }
            _ = self.control.cancellation.cancelled() => {
                return Err(McpError::RequestCancelled(self.server_id.clone()));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(McpError::RequestTimeout(self.server_id.clone()));
            }
            responses = fulfillment => responses?,
        };
        Ok(responses)
    }

    /// Dispatches one type-safe MRTR input request through the normal lifecycle handler.
    async fn fulfill_input_request(
        &self,
        key: &str,
        request: InputRequest,
    ) -> Result<serde_json::Value, McpError> {
        let request_id = RequestId::String(Arc::from(key));
        match request {
            InputRequest::CreateMessage(mut request) => {
                let mut context =
                    RequestContext::new(request_id, self.peer.clone());
                context.extensions = std::mem::take(&mut request.extensions);
                let response = self
                    .handler
                    .create_message(request.params, context)
                    .await
                    .map_err(|error| McpError::Host(error.to_string()))?;
                serde_json::to_value(response)
                    .map_err(|error| McpError::Host(error.to_string()))
            }
            InputRequest::Elicitation(mut request) => {
                let mut context =
                    RequestContext::new(request_id, self.peer.clone());
                context.extensions = std::mem::take(&mut request.extensions);
                let response = self
                    .handler
                    .create_elicitation(request.params, context)
                    .await
                    .map_err(|error| McpError::Host(error.to_string()))?;
                serde_json::to_value(response)
                    .map_err(|error| McpError::Host(error.to_string()))
            }
            InputRequest::ListRoots(mut request) => {
                let mut context =
                    RequestContext::new(request_id, self.peer.clone());
                context.extensions = std::mem::take(&mut request.extensions);
                let response = self
                    .handler
                    .list_roots(context)
                    .await
                    .map_err(|error| McpError::Host(error.to_string()))?;
                serde_json::to_value(response)
                    .map_err(|error| McpError::Host(error.to_string()))
            }
            _ => Err(self.protocol_error("unsupported MRTR input request")),
        }
    }

    /// Applies bounded backoff for state-only retries without hiding cancellation.
    async fn wait_state_only(
        &self,
        rounds: u32,
        deadline: tokio::time::Instant,
    ) -> Result<(), McpError> {
        let multiplier = 1_u64 << rounds.min(3);
        let delay = std::time::Duration::from_millis(
            50_u64.saturating_mul(multiplier).min(250),
        );
        tokio::select! {
            _ = self.control.lifecycle.cancelled() => {
                Err(McpError::RequestCancelled(self.server_id.clone()))
            }
            _ = self.control.cancellation.cancelled() => {
                Err(McpError::RequestCancelled(self.server_id.clone()))
            }
            _ = tokio::time::sleep_until(deadline) => {
                Err(McpError::RequestTimeout(self.server_id.clone()))
            }
            _ = tokio::time::sleep(delay) => Ok(()),
        }
    }

    /// Attaches this driver's Server identity to one rmcp service failure.
    pub(super) fn service_error(
        &self,
        error: rmcp::service::ServiceError,
    ) -> McpError {
        McpError::Service {
            server_id: self.server_id.clone(),
            message: error.to_string(),
        }
    }

    /// Creates one Server-scoped semantic protocol failure.
    fn protocol_error(&self, message: impl Into<String>) -> McpError {
        McpError::Service {
            server_id: self.server_id.clone(),
            message: message.into(),
        }
    }
}

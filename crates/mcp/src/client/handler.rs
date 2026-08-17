//! rmcp client handler that normalizes Legacy notifications for the runtime.
#![expect(
    deprecated,
    reason = "Legacy 2025-11-25 Server logging remains required by the supported protocol"
)]

use std::sync::Arc;

use protocol::{
    McpElicitationAction, McpElicitationMode, McpElicitationRequest,
    McpHostRequest, McpHostResponse, McpRootsRequest, McpSamplingRequest,
    MessageId, Role, StopReason, TimestampMs,
};
use rmcp::ClientHandler;
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    ClientInfo, CreateMessageRequestParams, CreateMessageResult,
    ElicitRequestParams, ElicitResult, ElicitationAction,
    ErrorData as RmcpError, ListRootsResult, LoggingMessageNotificationParam,
    ProgressNotificationParam, ResourceUpdatedNotificationParam, Root,
    SamplingMessage,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use tokio::sync::broadcast;

use super::content::{IntoProtocolSamplingMessage, IntoWireSamplingContent};
use super::invocation::InvocationRegistry;
use crate::{McpClientEvent, McpHost};

/// Lifecycle handler retaining client identity and normalized notification channels.
#[derive(Clone)]
pub(super) struct McpLifecycleHandler {
    info: ClientInfo,
    changes: broadcast::Sender<McpClientEvent>,
    progress: ProgressDispatcher,
    host: Arc<dyn McpHost>,
    invocations: InvocationRegistry,
}

/// Shared channels created with one lifecycle handler and retained by the client core.
pub(super) struct McpClientChannels {
    pub(super) changes: broadcast::Sender<McpClientEvent>,
    pub(super) progress: ProgressDispatcher,
    pub(super) invocations: InvocationRegistry,
    pub(super) handler: McpLifecycleHandler,
}

impl McpLifecycleHandler {
    /// Creates one handler and its shared notification sender.
    pub(super) fn new(
        info: ClientInfo,
        host: Arc<dyn McpHost>,
    ) -> (Self, McpClientChannels) {
        let (changes, _receiver) = broadcast::channel(128);
        let progress = ProgressDispatcher::new();
        let invocations = InvocationRegistry::new();
        let handler = Self {
            info,
            changes: changes.clone(),
            progress: progress.clone(),
            host,
            invocations: invocations.clone(),
        };
        (
            handler.clone(),
            McpClientChannels {
                changes,
                progress,
                invocations,
                handler,
            },
        )
    }
}

impl ClientHandler for McpLifecycleHandler {
    /// Returns the exact lifecycle identity configured by the protocol wrapper.
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    /// Returns only roots authorized by the owning Kernel Host for this Turn.
    async fn list_roots(
        &self,
        context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, RmcpError> {
        let mut request_context =
            self.invocations.resolve(&context).ok_or_else(|| {
                RmcpError::invalid_params(
                    "MCP Roots request is not associated with one active Turn",
                    None,
                )
            })?;
        request_context.requested_at_ms = TimestampMs::now();
        let response = self
            .host
            .handle(McpHostRequest::Roots(McpRootsRequest {
                context: request_context,
            }))
            .await
            .map_err(|error| {
                RmcpError::internal_error(error.to_string(), None)
            })?;
        let McpHostResponse::Roots(result) = response else {
            return Err(RmcpError::internal_error(
                "MCP Host returned a mismatched Roots response",
                None,
            ));
        };
        Ok(ListRootsResult::new(
            result
                .roots
                .into_iter()
                .map(|root| {
                    let mut value = Root::new(root.uri);
                    if let Some(name) = root.name {
                        value = value.with_name(name);
                    }
                    value
                })
                .collect(),
        ))
    }

    /// Routes a Sampling callback through the owning Kernel Host.
    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, RmcpError> {
        let mut request_context =
            self.invocations.resolve(&context).ok_or_else(|| {
                RmcpError::invalid_params(
                    "MCP Sampling request is not associated with one active Turn",
                    None,
                )
            })?;
        request_context.requested_at_ms = TimestampMs::now();
        let server_id = request_context.server_id.clone();
        let messages = params
            .messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                let message_id = MessageId::try_from(format!(
                    "mcp-sampling-{}-{index}",
                    context.id
                ))
                .map_err(|error| {
                    RmcpError::internal_error(error.to_string(), None)
                })?;
                message
                    .into_protocol_sampling(&request_context, message_id)
                    .map_err(|error| {
                        RmcpError::internal_error(error.to_string(), None)
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let response = self
            .host
            .handle(McpHostRequest::Sampling(
                McpSamplingRequest::builder()
                    .context(request_context)
                    .messages(messages)
                    .system_prompt(params.system_prompt)
                    .max_tokens(u64::from(params.max_tokens))
                    .temperature(params.temperature.map(f64::from))
                    .build(),
            ))
            .await
            .map_err(|error| {
                RmcpError::internal_error(error.to_string(), None)
            })?;
        let McpHostResponse::Sampling(result) = response else {
            return Err(RmcpError::internal_error(
                "MCP Host returned a mismatched Sampling response",
                None,
            ));
        };
        let role = match result.role {
            Role::User => rmcp::model::Role::User,
            Role::Assistant => rmcp::model::Role::Assistant,
            Role::System => {
                return Err(RmcpError::internal_error(
                    "MCP Sampling response cannot use the System role",
                    None,
                ));
            }
        };
        let mut blocks = result
            .blocks
            .into_iter()
            .map(|block| {
                block.into_wire_sampling(&server_id).map_err(|error| {
                    RmcpError::internal_error(error.to_string(), None)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let message = if blocks.len() == 1 {
            SamplingMessage::new(role, blocks.remove(0))
        } else {
            SamplingMessage::new_multiple(role, blocks)
        };
        Ok(
            CreateMessageResult::new(message, result.model).with_stop_reason(
                match result.stop_reason {
                    StopReason::EndTurn => {
                        CreateMessageResult::STOP_REASON_END_TURN
                    }
                    StopReason::ToolUse => {
                        CreateMessageResult::STOP_REASON_TOOL_USE
                    }
                    StopReason::MaxTokens => {
                        CreateMessageResult::STOP_REASON_END_MAX_TOKEN
                    }
                    StopReason::Refusal => "refusal",
                    StopReason::Error => "error",
                    StopReason::Cancelled => "cancelled",
                },
            ),
        )
    }

    /// Routes form and URL elicitation through the owning interactive Host.
    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, RmcpError> {
        let mut request_context =
            self.invocations.resolve(&context).ok_or_else(|| {
                RmcpError::invalid_params(
                    "MCP Elicitation request is not associated with one active Turn",
                    None,
                )
            })?;
        request_context.requested_at_ms = TimestampMs::now();
        let mode = match request {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => McpElicitationMode::Form {
                message,
                requested_schema: serde_json::to_value(requested_schema)
                    .map_err(|error| {
                        RmcpError::invalid_params(error.to_string(), None)
                    })?,
            },
            ElicitRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => McpElicitationMode::Url {
                message,
                url,
                elicitation_id,
            },
            _ => {
                return Err(RmcpError::invalid_params(
                    "unsupported MCP Elicitation mode",
                    None,
                ));
            }
        };
        let request_id = format!(
            "{}:{}:{}",
            request_context.server_id, request_context.trace_id, context.id
        );
        let response = self
            .host
            .handle(McpHostRequest::Elicitation(McpElicitationRequest {
                request_id,
                context: request_context,
                mode,
            }))
            .await
            .map_err(|error| {
                RmcpError::internal_error(error.to_string(), None)
            })?;
        let McpHostResponse::Elicitation(result) = response else {
            return Err(RmcpError::internal_error(
                "MCP Host returned a mismatched Elicitation response",
                None,
            ));
        };
        let action = match result.action {
            McpElicitationAction::Accept => ElicitationAction::Accept,
            McpElicitationAction::Decline => ElicitationAction::Decline,
            McpElicitationAction::Cancel => ElicitationAction::Cancel,
        };
        let mut response = ElicitResult::new(action);
        if let Some(content) = result.content {
            response = response.with_content(content);
        }
        Ok(response)
    }

    /// Normalizes Legacy Tool-list invalidation.
    async fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.changes.send(McpClientEvent::ToolsChanged);
    }

    /// Normalizes Legacy Prompt-list invalidation.
    async fn on_prompt_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.changes.send(McpClientEvent::PromptsChanged);
    }

    /// Normalizes Legacy Resource-list invalidation.
    async fn on_resource_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.changes.send(McpClientEvent::ResourcesChanged);
    }

    /// Normalizes one Legacy subscribed Resource update without rewriting its URI.
    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self
            .changes
            .send(McpClientEvent::ResourceUpdated(params.uri));
    }

    /// Routes progress by its generated request token for Tool observers.
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.handle_notification(params).await;
    }

    /// Records safe Server logging metadata without serializing arbitrary payload data.
    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        tracing::debug!(
            "MCP Server log received at level {:?} from logger '{}'",
            params.level,
            params.logger.as_deref().unwrap_or("unknown")
        );
    }
}

use std::collections::HashMap;
use std::sync::Arc;

use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpCompletionTarget,
    McpPromptMessage, McpPromptRequest, McpPromptResult, McpProtocolVersion,
    McpResourceRequest, McpResourceResult, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpToolRequest, McpToolResult,
};
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    ArgumentInfo, CallToolRequestParams, CompleteRequestParams,
    CompletionContext, GetPromptRequestParams, ProtocolVersion,
    ReadResourceRequestParams, Reference, ServerCapabilities,
    ServerNotification, SubscriptionFilter,
};
use rmcp::service::{Peer, RoleClient, RunningService};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::catalog::CatalogMapper;
use super::content::{IntoProtocolContent, IntoProtocolRole};
use super::handler::{McpClientChannels, McpLifecycleHandler};
use super::invocation::InvocationRegistry;
use super::mrtr::ToolCallDriver;
use crate::{McpClientEvent, McpError, McpRequestControl};

/// Generation-tagged Modern subscription task owned by one connected client.
struct SubscriptionTaskState {
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Shared post-lifecycle client state; lifecycle selection remains in distinct wrappers.
pub(super) struct ClientCore {
    server_id: McpServerId,
    protocol: McpProtocolVersion,
    server_info: Option<McpServerImplementation>,
    capabilities: McpServerCapabilities,
    peer: Peer<RoleClient>,
    running: tokio::sync::Mutex<
        Option<RunningService<RoleClient, McpLifecycleHandler>>,
    >,
    changes: broadcast::Sender<McpClientEvent>,
    progress: ProgressDispatcher,
    invocations: InvocationRegistry,
    handler: McpLifecycleHandler,
    subscription: Arc<tokio::sync::Mutex<SubscriptionTaskState>>,
    shutdown: CancellationToken,
}

impl ClientCore {
    /// Validates exact negotiation and captures immutable peer metadata after startup.
    pub(super) async fn from_running(
        server_id: McpServerId,
        expected: McpProtocolVersion,
        wire_expected: ProtocolVersion,
        mut running: RunningService<RoleClient, McpLifecycleHandler>,
        channels: McpClientChannels,
    ) -> Result<Self, McpError> {
        let peer_info =
            running.peer_info().ok_or_else(|| McpError::Service {
                server_id: server_id.clone(),
                message: "lifecycle completed without Server metadata"
                    .to_string(),
            })?;
        if peer_info.protocol_version != wire_expected {
            let actual = peer_info.protocol_version.to_string();
            running.close().await.map_err(|error| McpError::Shutdown {
                server_id: server_id.clone(),
                message: error.to_string(),
            })?;
            return Err(McpError::ProtocolMismatch {
                server_id,
                expected,
                actual,
            });
        }

        let server_info =
            peer_info.server_info.as_ref().map(|implementation| {
                McpServerImplementation {
                    name: implementation.name.clone(),
                    version: implementation.version.clone(),
                }
            });
        let capabilities = Self::capabilities(&peer_info.capabilities);
        let peer = running.peer().clone();
        Ok(Self {
            server_id,
            protocol: expected,
            server_info,
            capabilities,
            peer,
            running: tokio::sync::Mutex::new(Some(running)),
            changes: channels.changes,
            progress: channels.progress,
            invocations: channels.invocations,
            handler: channels.handler,
            subscription: Arc::new(tokio::sync::Mutex::new(
                SubscriptionTaskState {
                    generation: 0,
                    task: None,
                },
            )),
            shutdown: CancellationToken::new(),
        })
    }

    /// Returns the connected Server identifier.
    pub(super) fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Returns the exact selected protocol revision.
    pub(super) fn protocol(&self) -> McpProtocolVersion {
        self.protocol
    }

    /// Returns optional implementation metadata declared by the Server.
    pub(super) fn server_info(&self) -> Option<&McpServerImplementation> {
        self.server_info.as_ref()
    }

    /// Returns immutable capability gates declared by the Server.
    pub(super) fn declared_capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }

    /// Subscribes one Server runtime to normalized Legacy or Modern changes.
    pub(super) fn subscribe_changes(
        &self,
    ) -> broadcast::Receiver<McpClientEvent> {
        self.changes.subscribe()
    }

    /// Opens the Modern unified subscription stream exactly once after discovery.
    pub(super) async fn start_modern_subscription(
        &self,
        catalog: &McpCatalog,
    ) -> Result<(), McpError> {
        if !self.capabilities.tools_list_changed
            && !self.capabilities.prompts_list_changed
            && !self.capabilities.resources_list_changed
            && !self.capabilities.resource_subscribe
        {
            return Ok(());
        }
        let mut slot = self.subscription.lock().await;
        if slot.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        // A completed JoinHandle no longer represents a live subscription and
        // must not suppress the replacement stream established below.
        slot.task.take();
        let filter = SubscriptionFilter::builder()
            .tools_list_changed()
            .prompts_list_changed()
            .resources_list_changed()
            .resource_subscriptions(
                catalog
                    .resources
                    .iter()
                    .map(|resource| resource.reference.remote_uri.clone()),
            )
            .build();
        let mut subscription = self
            .peer
            .listen(filter)
            .await
            .map_err(|error| self.service_error(error))?;
        let changes = self.changes.clone();
        let shutdown = self.shutdown.clone();
        slot.generation = slot.generation.saturating_add(1);
        let generation = slot.generation;
        let subscription_state = Arc::clone(&self.subscription);
        slot.task = Some(tokio::spawn(async move {
            let ended = loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        if let Err(error) = subscription.cancel().await {
                            tracing::debug!(
                                "MCP Modern subscription cancellation failed: {}",
                                error
                            );
                        }
                        break false;
                    }
                    notification = subscription.next() => match notification {
                        Ok(Some(ServerNotification::ToolListChangedNotification(_))) => {
                            let _ = changes.send(McpClientEvent::ToolsChanged);
                        }
                        Ok(Some(ServerNotification::PromptListChangedNotification(_))) => {
                            let _ = changes.send(McpClientEvent::PromptsChanged);
                        }
                        Ok(Some(ServerNotification::ResourceListChangedNotification(_))) => {
                            let _ = changes.send(McpClientEvent::ResourcesChanged);
                        }
                        Ok(Some(ServerNotification::ResourceUpdatedNotification(notification))) => {
                            let _ = changes.send(McpClientEvent::ResourceUpdated(
                                notification.params.uri,
                            ));
                        }
                        Ok(Some(_notification)) => {}
                        Err(error) => {
                            tracing::warn!("MCP Modern subscription failed: {}", error);
                            break true;
                        }
                        Ok(None) => {
                            break true;
                        }
                    }
                }
            };
            // Clear the exact generation before publishing SubscriptionEnded so
            // the resulting catalog refresh can establish a replacement stream.
            let mut state = subscription_state.lock().await;
            if state.generation == generation {
                state.task.take();
            }
            drop(state);
            if ended {
                let _ = changes.send(McpClientEvent::SubscriptionEnded);
            }
        }));
        Ok(())
    }

    /// Fetches complete paginated catalogs only for capability families the Server declares.
    pub(super) async fn discover(&self) -> Result<McpCatalog, McpError> {
        let tools = if self.capabilities.tools {
            self.peer
                .list_all_tools()
                .await
                .map_err(|error| self.service_error(error))?
        } else {
            Vec::new()
        };
        let prompts = if self.capabilities.prompts {
            self.peer
                .list_all_prompts()
                .await
                .map_err(|error| self.service_error(error))?
        } else {
            Vec::new()
        };
        let resources = if self.capabilities.resources {
            self.peer
                .list_all_resources()
                .await
                .map_err(|error| self.service_error(error))?
        } else {
            Vec::new()
        };
        let templates = if self.capabilities.resources {
            self.peer
                .list_all_resource_templates()
                .await
                .map_err(|error| self.service_error(error))?
        } else {
            Vec::new()
        };

        Ok(CatalogMapper::new(self.server_id.clone())
            .map(tools, prompts, resources, templates))
    }

    /// Invokes one capability-gated Tool and preserves every returned content form.
    pub(super) async fn call_tool(
        &self,
        request: McpToolRequest,
        control: McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        self.ensure_route(&request.reference.server_id)?;
        self.ensure_capability(self.capabilities.tools, "tools")?;
        let result = ToolCallDriver::builder()
            .server_id(&self.server_id)
            .peer(&self.peer)
            .handler(&self.handler)
            .progress(&self.progress)
            .invocations(&self.invocations)
            .supports_tasks(self.capabilities.tasks)
            .control(control)
            .build()
            .call(
                CallToolRequestParams::new(request.reference.remote_name)
                    .with_arguments(request.arguments),
            )
            .await?;
        let content = result
            .content
            .into_iter()
            .map(|block| block.into_protocol(&self.server_id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(McpToolResult {
            content,
            structured_content: result.structured_content,
            is_error: result.is_error.unwrap_or(false),
        })
    }

    /// Retrieves one capability-gated Prompt and preserves role-bearing content.
    pub(super) async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        self.ensure_route(&request.reference.server_id)?;
        self.ensure_capability(self.capabilities.prompts, "prompts")?;
        let mut params =
            GetPromptRequestParams::new(request.reference.remote_name);
        if !request.arguments.is_empty() {
            params = params.with_arguments(
                request
                    .arguments
                    .into_iter()
                    .map(|(name, value)| {
                        (name, serde_json::Value::String(value))
                    })
                    .collect(),
            );
        }
        let result = self
            .peer
            .get_prompt(params)
            .await
            .map_err(|error| self.service_error(error))?;
        let messages = result
            .messages
            .into_iter()
            .map(|message| {
                Ok(McpPromptMessage {
                    role: message.role.into_protocol(),
                    content: message.content.into_protocol(&self.server_id)?,
                })
            })
            .collect::<Result<Vec<_>, McpError>>()?;
        Ok(McpPromptResult {
            description: result.description,
            messages,
        })
    }

    /// Reads one capability-gated Resource as ordered embedded content blocks.
    pub(super) async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        self.ensure_route(&request.reference.server_id)?;
        self.ensure_capability(self.capabilities.resources, "resources")?;
        let result = self
            .peer
            .read_resource(ReadResourceRequestParams::new(
                request.reference.remote_uri,
            ))
            .await
            .map_err(|error| self.service_error(error))?;
        let contents = result
            .contents
            .into_iter()
            .map(|content| content.into_protocol(&self.server_id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(McpResourceResult { contents })
    }

    /// Completes one capability-gated Prompt or Resource Template argument.
    pub(super) async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        self.ensure_capability(self.capabilities.completions, "completions")?;
        let reference = match request.target {
            McpCompletionTarget::Prompt(prompt) => {
                self.ensure_route(&prompt.server_id)?;
                Reference::for_prompt(prompt.remote_name)
            }
            McpCompletionTarget::ResourceTemplate {
                server_id,
                uri_template,
            } => {
                self.ensure_route(&server_id)?;
                Reference::for_resource(uri_template)
            }
        };
        let mut params = CompleteRequestParams::new(
            reference,
            ArgumentInfo::new(request.argument_name, request.argument_value),
        );
        if !request.context.is_empty() {
            params = params.with_context(CompletionContext::with_arguments(
                request.context.into_iter().collect::<HashMap<_, _>>(),
            ));
        }
        let completion = self
            .peer
            .complete(params)
            .await
            .map_err(|error| self.service_error(error))?
            .completion;
        Ok(McpCompletionResult {
            values: completion.values,
            total: completion.total.map(u64::from),
            has_more: completion.has_more.unwrap_or(false),
        })
    }

    /// Closes the background rmcp service exactly once.
    pub(super) async fn shutdown(&self) -> Result<(), McpError> {
        self.shutdown.cancel();
        let subscription = self.subscription.lock().await.task.take();
        if let Some(subscription) = subscription
            && let Err(error) = subscription.await
            && !error.is_cancelled()
        {
            tracing::warn!("MCP subscription task failed: {}", error);
        }
        let mut running = self.running.lock().await;
        if let Some(service) = running.as_mut() {
            service.close().await.map_err(|error| McpError::Shutdown {
                server_id: self.server_id.clone(),
                message: error.to_string(),
            })?;
        }
        running.take();
        Ok(())
    }

    /// Converts rmcp capability details into transport-independent boolean gates.
    fn capabilities(
        capabilities: &ServerCapabilities,
    ) -> McpServerCapabilities {
        McpServerCapabilities::builder()
            .tools(capabilities.tools.is_some())
            .prompts(capabilities.prompts.is_some())
            .resources(capabilities.resources.is_some())
            .completions(capabilities.completions.is_some())
            .resource_subscribe(
                capabilities
                    .resources
                    .as_ref()
                    .and_then(|resources| resources.subscribe)
                    .unwrap_or(false),
            )
            .tools_list_changed(
                capabilities
                    .tools
                    .as_ref()
                    .and_then(|tools| tools.list_changed)
                    .unwrap_or(false),
            )
            .prompts_list_changed(
                capabilities
                    .prompts
                    .as_ref()
                    .and_then(|prompts| prompts.list_changed)
                    .unwrap_or(false),
            )
            .resources_list_changed(
                capabilities
                    .resources
                    .as_ref()
                    .and_then(|resources| resources.list_changed)
                    .unwrap_or(false),
            )
            .tasks(capabilities.supports_tasks())
            .build()
    }

    /// Rejects a request whose structured reference belongs to another Server.
    fn ensure_route(&self, actual: &McpServerId) -> Result<(), McpError> {
        if actual == &self.server_id {
            return Ok(());
        }
        Err(McpError::Routing {
            expected: self.server_id.clone(),
            actual: actual.clone(),
        })
    }

    /// Rejects operations absent from the lifecycle capability declaration.
    fn ensure_capability(
        &self,
        available: bool,
        capability: &'static str,
    ) -> Result<(), McpError> {
        if available {
            return Ok(());
        }
        Err(McpError::CapabilityUnavailable {
            server_id: self.server_id.clone(),
            capability,
        })
    }

    /// Attaches the owning Server identity to one rmcp request failure.
    fn service_error(&self, error: rmcp::service::ServiceError) -> McpError {
        McpError::Service {
            server_id: self.server_id.clone(),
            message: error.to_string(),
        }
    }
}

impl std::fmt::Debug for ClientCore {
    /// Reports only safe immutable lifecycle metadata.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientCore")
            .field("server_id", &self.server_id)
            .field("protocol", &self.protocol)
            .field("server_info", &self.server_info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

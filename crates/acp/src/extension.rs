use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{
    Client, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, UntypedMessage, V2ConnectionTo,
};
use kernel::Kernel;
use protocol::{
    AcpCommandParameters, AcpCompactParameters, AcpExtensionMethod,
    AcpForkParameters, AcpNavigateParameters,
    AcpPendingMessageRemoveParameters, AcpSessionParameters,
    AcpSessionRenameParameters, AcpSkillParameters, AcpUserBashParameters,
    McpSessionRevisionNotification, ProductIdentity, SessionTitle,
};
use tokio_util::sync::CancellationToken;

use crate::projection::ProjectionRegistry;
use crate::server::{AcpServer, ProjectionWatch};

/// Converts any parameter decoding failure into the ACP invalid-parameters category.
fn invalid_parameters(
    error: impl std::fmt::Display,
) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(error.to_string())
}

/// Product-scoped realtime notification carrying one MCP snapshot revision.
#[derive(Debug, Clone)]
pub(crate) struct AcpMcpUpdateNotification(
    pub(crate) McpSessionRevisionNotification,
);

impl JsonRpcMessage for AcpMcpUpdateNotification {
    /// Matches only the centralized product MCP update notification.
    fn matches_method(method: &str) -> bool {
        method == ProductIdentity::ACP_MCP_UPDATE_NOTIFICATION
    }

    /// Returns the centralized product notification method.
    fn method(&self) -> &'static str {
        ProductIdentity::ACP_MCP_UPDATE_NOTIFICATION
    }

    /// Serializes the typed revision payload through one untyped JSON-RPC envelope.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method(), &self.0)
    }

    /// Parses one typed revision payload without accepting unknown protocol fields.
    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<Self, agent_client_protocol::Error> {
        if !Self::matches_method(method) {
            return Err(agent_client_protocol::Error::method_not_found());
        }
        let value = serde_json::to_value(params)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        serde_json::from_value(value)
            .map(Self)
            .map_err(invalid_parameters)
    }
}

impl JsonRpcNotification for AcpMcpUpdateNotification {}

/// One product-scoped ACP request that preserves method-specific JSON parameters.
#[derive(Debug, Clone)]
pub(crate) struct AcpExtensionRequest {
    method: AcpExtensionMethod,
    parameters: serde_json::Value,
}

impl AcpExtensionRequest {
    /// Returns the original extension parameters without adding method metadata.
    pub(crate) fn parameters(&self) -> &serde_json::Value {
        &self.parameters
    }
}

impl JsonRpcMessage for AcpExtensionRequest {
    /// Matches every extension method centralized by the protocol crate.
    fn matches_method(method: &str) -> bool {
        AcpExtensionMethod::parse(method).is_some()
    }

    /// Returns the exact extension method represented by this request.
    fn method(&self) -> &'static str {
        self.method.as_str()
    }

    /// Converts the typed request into an untyped JSON-RPC message.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method(), &self.parameters)
    }

    /// Parses one recognized extension method while preserving its complete parameter object.
    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<Self, agent_client_protocol::Error> {
        let method = AcpExtensionMethod::parse(method)
            .ok_or_else(agent_client_protocol::Error::method_not_found)?;
        let parameters = serde_json::to_value(params)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        Ok(Self { method, parameters })
    }
}

impl JsonRpcRequest for AcpExtensionRequest {
    type Response = AcpExtensionResponse;
}

/// Opaque successful extension response returned to ACP clients without schema loss.
#[derive(Debug, Clone)]
pub(crate) struct AcpExtensionResponse(pub serde_json::Value);

impl JsonRpcResponse for AcpExtensionResponse {
    /// Returns the already validated extension response JSON.
    fn into_json(
        self,
        _method: &str,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        Ok(self.0)
    }

    /// Preserves extension responses received by SDK-side clients.
    fn from_value(
        _method: &str,
        value: serde_json::Value,
    ) -> Result<Self, agent_client_protocol::Error> {
        Ok(Self(value))
    }
}

/// Type-driven dispatcher for every advertised non-standard ACP method.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct AcpExtensionDispatcher {
    pub kernel: Arc<Kernel>,
    pub connection: V2ConnectionTo<Client>,
    pub mcp_watchers: Arc<Mutex<BTreeSet<protocol::SessionId>>>,
    pub projections: Arc<ProjectionRegistry>,
    pub session_watchers: crate::server::SessionWatchers,
    pub elicitations: Arc<crate::elicitation::ElicitationBridge>,
}

impl AcpExtensionDispatcher {
    /// Validates method-specific parameters and invokes the corresponding kernel capability.
    pub async fn execute(
        &self,
        request: AcpExtensionRequest,
    ) -> Result<AcpExtensionResponse, agent_client_protocol::Error> {
        // Tree projections consume detached Kernel snapshots, never the stored history itself.
        let result = match request.method {
            AcpExtensionMethod::Tree => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                crate::mapping::SessionTreeOf(
                    self.kernel.session_tree(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .into_value()
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::Navigate => {
                let input: AcpNavigateParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let target = input.entry_id;
                crate::mapping::SessionTreeOf(
                    self.kernel
                        .navigate_session(&session_id, target)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
                .into_value()
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::Branch => {
                let input: AcpNavigateParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let target = input.entry_id;
                crate::mapping::SessionTreeOf(
                    self.kernel
                        .navigate_session(&session_id, target)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
                .into_value()
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::Fork => {
                let input: AcpForkParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let leaf = input.entry_id;
                let cwd = input.cwd.into_inner();
                let fork_id = match input.new_session_id {
                    Some(new_session_id) => {
                        self.kernel
                            .fork_session(
                                &session_id,
                                leaf,
                                new_session_id.clone(),
                                cwd,
                            )
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        new_session_id
                    }
                    None => self
                        .kernel
                        .fork_generated_session(&session_id, leaf, cwd)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                };
                let live = self.projections.subscribe_live(&fork_id).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                let server = AcpServer::new(
                    Arc::clone(&self.kernel),
                    Arc::clone(&self.projections),
                    Arc::clone(&self.elicitations),
                );
                // A fork answers with its id; commands arrive as a notification.
                server.send_available_commands(&fork_id, &self.connection)?;
                server.watch_mcp(
                    &fork_id,
                    &self.connection,
                    Arc::clone(&self.mcp_watchers),
                )?;
                server.watch_projection(
                    &fork_id,
                    &self.connection,
                    Arc::clone(&self.session_watchers),
                    ProjectionWatch::from(live),
                )?;
                serde_json::json!({ "sessionId": fork_id.to_string() })
            }
            AcpExtensionMethod::Compact => {
                let input: AcpCompactParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let result = self
                    .kernel
                    .compact_session(
                        &session_id,
                        self.projections.sink(session_id.clone()),
                    )
                    .await
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                let mut projected = serde_json::to_value(&result).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                // Direct method replies use the same precision-safe accounting as notifications.
                if let Some(usage) = &result.usage {
                    let slot = projected.get_mut("usage").ok_or_else(|| {
                        agent_client_protocol::Error::into_internal_error(
                            crate::mapping::AcpMappingError::UsageProjection,
                        )
                    })?;
                    *slot = crate::mapping::UsageOf(usage).metadata();
                }
                projected
            }
            AcpExtensionMethod::PendingMessages => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.pending_messages(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::PendingMessageRemove => {
                let input: AcpPendingMessageRemoveParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                self.kernel
                    .remove_pending_message(&input.session_id, &input.queue_id)
                    .map_err(crate::server::KernelFailure)?;
                serde_json::json!({ "removed": true })
            }
            AcpExtensionMethod::PendingMessagePromote => {
                let input: AcpPendingMessageRemoveParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let promoted = self
                    .kernel
                    .promote_pending_message(&input.session_id, &input.queue_id)
                    .map_err(crate::server::KernelFailure)?;
                serde_json::json!({ "promoted": promoted })
            }
            AcpExtensionMethod::ClearQueue => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                self.kernel
                    .clear_pending_messages(&input.session_id)
                    .map_err(crate::server::KernelFailure)?;
                serde_json::json!({ "cleared": true })
            }
            AcpExtensionMethod::SessionRename => {
                let input: AcpSessionRenameParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let title = SessionTitle::try_from(input.title)
                    .map_err(invalid_parameters)?;
                self.kernel
                    .rename_session(&input.session_id, title.clone())
                    .await
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                serde_json::json!({ "title": title.as_str() })
            }
            AcpExtensionMethod::SessionRuntime => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.session_runtime(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::InvokeSkill => {
                let input: AcpSkillParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let content = self
                    .kernel
                    .invoke_skill(&input.session_id, &input.name)
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                serde_json::json!({ "name": input.name, "content": content })
            }
            AcpExtensionMethod::SkillList => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.skills(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpStatus => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.mcp_status(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpReconnect => {
                let input: protocol::McpReconnectRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.mcp_reconnect(input).await.map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpPromptGet => {
                let input: protocol::McpSessionPromptRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.mcp_get_prompt(input).await.map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpResourceRead => {
                let input: protocol::McpSessionResourceRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let cancellation = self
                    .session_watchers
                    .lock()
                    .map_err(|_poison_error| {
                        agent_client_protocol::Error::internal_error()
                    })?
                    .get(&input.session_id)
                    .map_or_else(CancellationToken::new, |watcher| {
                        watcher.cancellation.child_token()
                    });
                let _cancel_on_drop = cancellation.clone().drop_guard();
                serde_json::to_value(
                    self.kernel
                        .mcp_read_resource(input, &cancellation)
                        .await
                        .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpComplete => {
                let input: protocol::McpSessionCompletionRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.mcp_complete(input).await.map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpOAuthContinue => {
                let input: protocol::McpAuthorizationContinueRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel
                        .mcp_continue_authorization(input)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::ExtensionCommand => {
                let input: AcpCommandParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                self.kernel
                    .invoke_extension_command(
                        &input.session_id,
                        input.name,
                        input.arguments,
                    )
                    .await
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?
            }
            AcpExtensionMethod::UserBash => {
                let input: AcpUserBashParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id.clone();
                serde_json::to_value(
                    self.kernel
                        .execute_user_bash(
                            input,
                            self.projections.sink(session_id),
                        )
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
        };
        Ok(AcpExtensionResponse(result))
    }
}

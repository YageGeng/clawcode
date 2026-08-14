use std::sync::Arc;

use agent_client_protocol::{
    Client, ConnectionTo, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse,
    UntypedMessage,
};
use kernel::Kernel;
use protocol::{
    AcpCommandParameters, AcpCompactParameters, AcpExtensionMethod,
    AcpForkParameters, AcpNavigateParameters,
    AcpPendingMessageRemoveParameters, AcpQueueMessageParameters,
    AcpSessionParameters, AcpSessionRenameParameters, AcpSkillParameters,
    QueueKind, SessionTitle,
};

use crate::server::AcpEventSink;

/// Converts any parameter decoding failure into the ACP invalid-parameters category.
fn invalid_parameters(
    error: impl std::fmt::Display,
) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(error.to_string())
}

/// One product-scoped ACP request that preserves method-specific JSON parameters.
#[derive(Debug, Clone)]
pub(crate) struct AcpExtensionRequest {
    method: AcpExtensionMethod,
    parameters: serde_json::Value,
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
pub(crate) struct AcpExtensionDispatcher {
    pub kernel: Arc<Kernel>,
    pub connection: ConnectionTo<Client>,
}

impl AcpExtensionDispatcher {
    /// Validates method-specific parameters and invokes the corresponding kernel capability.
    pub async fn execute(
        &self,
        request: AcpExtensionRequest,
    ) -> Result<AcpExtensionResponse, agent_client_protocol::Error> {
        let result = match request.method {
            AcpExtensionMethod::FollowUp => {
                let input: AcpQueueMessageParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let queued = self
                    .kernel
                    .queue_message(
                        &session_id,
                        QueueKind::FollowUp,
                        input.input,
                    )
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                serde_json::to_value(queued).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?
            }
            AcpExtensionMethod::Tree => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.session_tree(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::Navigate => {
                let input: AcpNavigateParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let target = input.entry_id;
                serde_json::to_value(
                    self.kernel
                        .navigate_session(&session_id, target)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::Branch => {
                let input: AcpNavigateParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let session_id = input.session_id;
                let target = input.entry_id;
                serde_json::to_value(
                    self.kernel
                        .navigate_session(&session_id, target)
                        .await
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                )
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
                        Arc::new(AcpEventSink::new(
                            session_id.clone(),
                            self.connection.clone(),
                        )),
                    )
                    .await
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                serde_json::to_value(result).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?
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
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                serde_json::json!({ "removed": true })
            }
            AcpExtensionMethod::ClearQueue => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                self.kernel
                    .clear_pending_messages(&input.session_id)
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
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
            AcpExtensionMethod::InvokeSkill => {
                let input: AcpSkillParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                let content = self.kernel.invoke_skill(&input.name).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                serde_json::json!({ "name": input.name, "content": content })
            }
            AcpExtensionMethod::SkillList => serde_json::to_value(
                self.kernel.skills(),
            )
            .map_err(agent_client_protocol::Error::into_internal_error)?,
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
        };
        Ok(AcpExtensionResponse(result))
    }
}

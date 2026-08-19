use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v2 as wire;
use agent_client_protocol::{
    Client, ConnectionTo, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, UntypedMessage,
};
use kernel::{Kernel, KernelError};
use protocol::{
    AcpCommandParameters, AcpCompactParameters, AcpExtensionMethod,
    AcpForkParameters, AcpNavigateParameters,
    AcpPendingMessageRemoveParameters, AcpSessionParameters,
    AcpSessionRenameParameters, AcpSkillParameters, AcpUserBashParameters,
    McpSessionRevisionNotification, ProductIdentity, QueueKind, SessionTitle,
};
use serde::Deserialize;

use crate::input::PromptInput;
use crate::server::AcpEventSink;
use crate::server::AcpServer;

/// Parameters for adding one multimodal follow-up to the active run.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcpQueueMessageParameters {
    /// Session that owns the queue.
    session_id: protocol::SessionId,
    /// Ordered ACP prompt blocks supplied by the client.
    prompt: Vec<wire::ContentBlock>,
}

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
pub(crate) struct AcpExtensionDispatcher {
    pub kernel: Arc<Kernel>,
    pub connection: ConnectionTo<Client>,
    pub mcp_watchers: Arc<Mutex<BTreeSet<protocol::SessionId>>>,
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
                let prompt = PromptInput::try_from(input.prompt)
                    .map_err(invalid_parameters)?
                    .into_inner();
                let queued = self
                    .kernel
                    .queue_message(&session_id, QueueKind::FollowUp, prompt)
                    .await
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
                AcpServer::new(Arc::clone(&self.kernel))
                    .send_available_commands(&fork_id, &self.connection)?;
                AcpServer::new(Arc::clone(&self.kernel)).watch_mcp(
                    &fork_id,
                    &self.connection,
                    Arc::clone(&self.mcp_watchers),
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
                let removed = match self
                    .kernel
                    .remove_pending_message(&input.session_id, &input.queue_id)
                {
                    Ok(()) => true,
                    Err(KernelError::PendingMessageNotFound(_)) => {
                        // The queue consumer may win the race after the WebUI
                        // rendered its last snapshot. DELETE remains idempotent.
                        tracing::debug!(
                            "pending message {} for session {} was already absent during removal",
                            input.queue_id,
                            input.session_id
                        );
                        false
                    }
                    Err(error) => {
                        tracing::error!(
                            "failed to remove pending message {} from session {}: {}",
                            input.queue_id,
                            input.session_id,
                            error
                        );
                        return Err(
                            agent_client_protocol::Error::into_internal_error(
                                error,
                            ),
                        );
                    }
                };
                serde_json::json!({ "removed": removed })
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
                serde_json::to_value(
                    self.kernel.mcp_read_resource(input).await.map_err(
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
            AcpExtensionMethod::McpElicitationList => {
                let input: AcpSessionParameters =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                serde_json::to_value(
                    self.kernel.mcp_elicitations(&input.session_id).map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?,
                )
                .map_err(agent_client_protocol::Error::into_internal_error)?
            }
            AcpExtensionMethod::McpElicitationRespond => {
                let input: protocol::McpElicitationResponseRequest =
                    serde_json::from_value(request.parameters)
                        .map_err(invalid_parameters)?;
                self.kernel.mcp_respond_elicitation(input).await.map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                serde_json::json!({ "resolved": true })
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
                            Arc::new(AcpEventSink::new(
                                session_id,
                                self.connection.clone(),
                            )),
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

#[cfg(test)]
mod tests {
    use super::AcpQueueMessageParameters;

    /// Follow-up parameters preserve ordered ACP text and image blocks.
    #[test]
    fn follow_up_parameters_accept_acp_prompt_blocks() {
        let parameters: AcpQueueMessageParameters =
            serde_json::from_value(serde_json::json!({
                "sessionId": "session-follow-up",
                "prompt": [
                    { "type": "text", "text": "inspect" },
                    {
                        "type": "image",
                        "data": "Q0xBVy03MzE5",
                        "mimeType": "image/png"
                    }
                ]
            }))
            .expect("multimodal follow-up parameters");

        assert_eq!(parameters.prompt.len(), 2);
    }
}

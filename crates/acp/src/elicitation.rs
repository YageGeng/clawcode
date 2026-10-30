use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use agent_client_protocol::schema::v2 as wire;
use agent_client_protocol::{
    Client, RequestCancellationHandle, V2ConnectionTo,
};
use kernel::{Kernel, KernelError};
use protocol::{
    McpElicitationAction, McpElicitationMode, McpElicitationRequest,
    McpElicitationResponseRequest, McpElicitationResult, ProductIdentity,
    SessionId,
};

/// A Kernel elicitation change carried beside one live projection group.
#[derive(Debug, Clone)]
pub(crate) enum ElicitationChange {
    /// An MCP server asked the user for input.
    Requested(Box<McpElicitationRequest>),
    /// The request was answered or cancelled anywhere.
    Resolved { request_id: String },
}

/// Elicitation modes one client advertised at initialize.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ElicitationModes {
    pub(crate) form: bool,
    pub(crate) url: bool,
}

impl From<Option<&wire::ElicitationCapabilities>> for ElicitationModes {
    /// Absent or empty capabilities support neither mode.
    fn from(capabilities: Option<&wire::ElicitationCapabilities>) -> Self {
        capabilities.map_or_else(Self::default, |capabilities| Self {
            form: capabilities.supports_form(),
            url: capabilities.supports_url(),
        })
    }
}

/// Borrows the Session and MCP request for an ACP-owned conversion.
pub(crate) struct ElicitationOf<'a> {
    pub(crate) session_id: &'a SessionId,
    pub(crate) request: &'a McpElicitationRequest,
}

impl TryFrom<ElicitationOf<'_>> for wire::CreateElicitationRequest {
    type Error = serde_json::Error;

    /// Scopes the request to its Session and, when known, its tool call.
    ///
    /// Fails when the requested schema is outside ACP's restricted form schema.
    fn try_from(elicitation: ElicitationOf<'_>) -> Result<Self, Self::Error> {
        let scope = wire::ElicitationSessionScope::new(
            elicitation.session_id.to_string(),
        )
        .tool_call_id(
            elicitation
                .request
                .context
                .tool_call_id
                .as_ref()
                .map(|id| wire::ToolCallId::new(id.to_string())),
        );
        Ok(match &elicitation.request.mode {
            McpElicitationMode::Form {
                message,
                requested_schema,
            } => Self::new(
                wire::ElicitationFormMode::new(
                    scope,
                    serde_json::from_value(requested_schema.clone())?,
                ),
                message.clone(),
            ),
            McpElicitationMode::Url {
                message,
                url,
                elicitation_id,
            } => Self::new(
                wire::ElicitationUrlMode::new(
                    scope,
                    elicitation_id.clone(),
                    url.clone(),
                ),
                message.clone(),
            ),
        }
        // The standard request has no server field; clients show it from product metadata.
        .meta(wire::Meta::from_iter([(
            ProductIdentity::ACP_NAMESPACE.to_string(),
            serde_json::json!({
                "mcpServerId": elicitation.request.context.server_id
            }),
        )])))
    }
}

/// One client answer checked against the schema it was asked with.
struct ClientAnswer {
    action: wire::ElicitationAction,
    validator: Option<Arc<jsonschema::Validator>>,
}

impl From<ClientAnswer> for McpElicitationResult {
    /// Unknown actions and content that breaks the schema become `cancel`.
    fn from(answer: ClientAnswer) -> Self {
        let cancel = Self {
            action: McpElicitationAction::Cancel,
            content: None,
        };
        match answer.action {
            wire::ElicitationAction::Accept(accept) => {
                let content =
                    accept.content.map(serde_json::to_value).transpose();
                let Ok(content) = content else {
                    tracing::warn!(
                        "treated accepted elicitation content that cannot serialize as cancel"
                    );
                    return cancel;
                };
                let checked =
                    content.clone().unwrap_or_else(|| serde_json::json!({}));
                if answer
                    .validator
                    .is_some_and(|validator| !validator.is_valid(&checked))
                {
                    tracing::warn!(
                        "treated accepted elicitation content that violates its schema as cancel"
                    );
                    return cancel;
                }
                Self {
                    action: McpElicitationAction::Accept,
                    content,
                }
            }
            wire::ElicitationAction::Decline => Self {
                action: McpElicitationAction::Decline,
                content: None,
            },
            wire::ElicitationAction::Cancel => cancel,
            other => {
                tracing::warn!(
                    "treated unknown elicitation action {:?} as cancel",
                    other
                );
                cancel
            }
        }
    }
}

/// Per-connection bridge between Kernel MCP elicitations and ACP `elicitation/create`.
///
/// shortcut: never sends `elicitation/complete`, because the MCP client does not yet
/// handle URL-mode completion notifications; forward them once it does.
#[derive(Default)]
pub(crate) struct ElicitationBridge {
    modes: OnceLock<ElicitationModes>,
    outstanding:
        Arc<Mutex<HashMap<(SessionId, String), RequestCancellationHandle>>>,
}

impl ElicitationBridge {
    /// Records the modes the client advertised; later initializes are ignored.
    pub(crate) fn negotiated(&self, modes: ElicitationModes) {
        tracing::info!(
            "client supports elicitation form {} and url {}",
            modes.form,
            modes.url
        );
        let _first = self.modes.set(modes);
    }

    /// Asks the client for one pending elicitation without waiting for its answer.
    pub(crate) fn request(
        &self,
        connection: &V2ConnectionTo<Client>,
        kernel: &Arc<Kernel>,
        session_id: &SessionId,
        request: &McpElicitationRequest,
    ) {
        let modes = self.modes.get().copied().unwrap_or_default();
        let (supported, schema) = match &request.mode {
            McpElicitationMode::Form {
                requested_schema, ..
            } => (modes.form, Some(requested_schema)),
            McpElicitationMode::Url { .. } => (modes.url, None),
        };
        if !supported {
            // Stays pending until a capable client attaches or the Turn cancels.
            tracing::info!(
                "kept MCP elicitation {} for Session {} pending: client lacks its mode",
                request.request_id,
                session_id
            );
            return;
        }
        let key = (session_id.clone(), request.request_id.clone());
        let Ok(mut outstanding) = self.outstanding.lock() else {
            tracing::warn!("ACP elicitation table lock poisoned");
            return;
        };
        if outstanding.contains_key(&key) {
            return;
        }
        // The client could not render a schema that does not compile either,
        // and a failed request would leave the Run waiting forever.
        let prepared = schema
            .map(jsonschema::validator_for)
            .transpose()
            .map_err(|error| error.to_string())
            .and_then(|validator| {
                wire::CreateElicitationRequest::try_from(ElicitationOf {
                    session_id,
                    request,
                })
                .map(|create| (create, validator.map(Arc::new)))
                .map_err(|error| error.to_string())
            });
        let (create, validator) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(
                    "cancelled MCP elicitation {} for Session {} with an unusable schema: {}",
                    request.request_id,
                    session_id,
                    error
                );
                drop(outstanding);
                let kernel = Arc::clone(kernel);
                let cancel = McpElicitationResponseRequest {
                    session_id: session_id.clone(),
                    request_id: request.request_id.clone(),
                    result: McpElicitationResult {
                        action: McpElicitationAction::Cancel,
                        content: None,
                    },
                };
                // Answering the Kernel waits on the MCP server; keep it off dispatch.
                if let Err(error) = connection.spawn(async move {
                    Self::deliver(&kernel, cancel).await;
                    Ok(())
                }) {
                    tracing::warn!(
                        "failed to start elicitation cancel task: {}",
                        error
                    );
                }
                return;
            }
        };
        tracing::info!(
            "sending elicitation/create for MCP elicitation {} in Session {}",
            request.request_id,
            session_id
        );
        let sent = connection.send_request(create);
        outstanding.insert(key.clone(), sent.cancellation_handle());
        drop(outstanding);
        let table = Arc::clone(&self.outstanding);
        let kernel = Arc::clone(kernel);
        if let Err(error) = connection.spawn(async move {
            let response = sent.block_task().await;
            if let Ok(mut outstanding) = table.lock() {
                outstanding.remove(&key);
            }
            let (session_id, request_id) = key;
            match response {
                Ok(response) => {
                    tracing::info!(
                        "received elicitation answer for MCP elicitation {} in Session {}",
                        request_id,
                        session_id
                    );
                    let result = McpElicitationResult::from(ClientAnswer {
                        action: response.action,
                        validator,
                    });
                    Self::deliver(
                        &kernel,
                        McpElicitationResponseRequest {
                            session_id,
                            request_id,
                            result,
                        },
                    )
                    .await;
                }
                // The Kernel request stays pending for another client or a resume.
                Err(error) => tracing::warn!(
                    "elicitation/create for MCP elicitation {} in Session {} failed: {}",
                    request_id,
                    session_id,
                    error
                ),
            }
            Ok(())
        }) {
            tracing::warn!(
                "failed to start elicitation answer task: {}",
                error
            );
        }
    }

    /// Withdraws this connection's copy of a request resolved anywhere.
    pub(crate) fn resolved(&self, session_id: &SessionId, request_id: &str) {
        let handle =
            self.outstanding.lock().ok().and_then(|mut outstanding| {
                outstanding
                    .remove(&(session_id.clone(), request_id.to_string()))
            });
        if let Some(handle) = handle {
            tracing::info!(
                "cancelling elicitation/create for resolved MCP elicitation {} in Session {}",
                request_id,
                session_id
            );
            if let Err(error) = handle.cancel() {
                tracing::warn!(
                    "failed to cancel elicitation/create for MCP elicitation {}: {}",
                    request_id,
                    error
                );
            }
        }
    }

    /// Asks again for every request a resumed Session still holds.
    pub(crate) fn reestablish(
        &self,
        connection: &V2ConnectionTo<Client>,
        kernel: &Arc<Kernel>,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        for request in kernel.mcp_elicitations(session_id)?.requests {
            self.request(connection, kernel, session_id, &request);
        }
        Ok(())
    }

    /// Hands one result to the Kernel; a request settled elsewhere is ignored.
    async fn deliver(kernel: &Kernel, response: McpElicitationResponseRequest) {
        let request_id = response.request_id.clone();
        match kernel.mcp_respond_elicitation(response).await {
            Ok(()) => tracing::info!(
                "delivered elicitation answer for MCP elicitation {}",
                request_id
            ),
            // Another client answered first, or the Turn cancelled it.
            Err(KernelError::ElicitationResolved(_)) => {}
            Err(error) => tracing::warn!(
                "failed to deliver answer for MCP elicitation {}: {}",
                request_id,
                error
            ),
        }
    }
}

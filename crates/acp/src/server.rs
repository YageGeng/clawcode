use std::sync::Arc;

use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{
    Agent, Client, ConnectTo, ConnectionTo, JsonRpcMessage, Responder,
};
use async_trait::async_trait;
use kernel::{EventSink, Kernel, SinkError};
use protocol::{
    AcpExtensionMethod, AcpWorkingDirectory, AgentEvent, AgentEventPayload,
    EventMetadata, IdGenerator, ProductIdentity, RunRequest, Sequence,
    SessionId,
};

use crate::extension::{AcpExtensionDispatcher, AcpExtensionRequest};
use crate::trace::AcpTraceFactory;
use crate::{AcpEventMapper, AcpMappingError};

/// Cohesive ACP notification operations backed by one shared Kernel.
pub(crate) struct AcpServer {
    kernel: Arc<Kernel>,
}

impl AcpServer {
    /// Binds ACP notification operations to one Kernel instance.
    pub(crate) fn new(kernel: Arc<Kernel>) -> Self {
        Self { kernel }
    }

    /// Sends the complete Session command snapshot through native ACP v2.
    pub(crate) fn send_available_commands(
        &self,
        session_id: &SessionId,
        connection: &ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let event = self
            .kernel
            .available_commands_event(session_id)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let metadata = AcpEventMapper::metadata(&event)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        for update in AcpEventMapper::map(event)
            .map_err(agent_client_protocol::Error::into_internal_error)?
        {
            connection.send_notification(
                wire::UpdateSessionNotification::new(
                    session_id.to_string(),
                    update,
                )
                .meta(metadata.clone()),
            )?;
        }
        Ok(())
    }
}

/// Factory that creates an isolated ACP v2 connection over a shared kernel.
pub struct AcpServerFactory {
    kernel: Arc<Kernel>,
    id_generator: Arc<dyn IdGenerator>,
}

impl AcpServerFactory {
    /// Creates an ACP server factory without client filesystem or terminal callbacks.
    #[must_use]
    pub fn new(
        kernel: Arc<Kernel>,
        id_generator: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            kernel,
            id_generator,
        }
    }

    /// Builds one ACP v2 agent component for stdio, HTTP/SSE, or WebSocket transport.
    pub fn component(
        &self,
        transport: crate::AcpTransportKind,
    ) -> impl ConnectTo<Client> + 'static + use<> {
        let session_kernel = Arc::clone(&self.kernel);
        let list_kernel = Arc::clone(&self.kernel);
        let resume_kernel = Arc::clone(&self.kernel);
        let close_kernel = Arc::clone(&self.kernel);
        let delete_kernel = Arc::clone(&self.kernel);
        let prompt_kernel = Arc::clone(&self.kernel);
        let cancel_kernel = Arc::clone(&self.kernel);
        let extension_kernel = Arc::clone(&self.kernel);
        let traces =
            AcpTraceFactory::new(Arc::clone(&self.id_generator), transport);
        let initialize_traces = traces.clone();
        let session_traces = traces.clone();
        let list_traces = traces.clone();
        let resume_traces = traces.clone();
        let close_traces = traces.clone();
        let delete_traces = traces.clone();
        let prompt_traces = traces.clone();
        let cancel_traces = traces.clone();
        let extension_traces = traces;

        Agent
            .v2()
            .on_receive_request(
                async move |request: wire::InitializeRequest,
                            responder: Responder<wire::InitializeResponse>,
                            _connection: ConnectionTo<Client>| {
                    let trace_factory = initialize_traces.clone();
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        let methods = [
                        AcpExtensionMethod::FollowUp,
                        AcpExtensionMethod::Tree,
                        AcpExtensionMethod::Navigate,
                        AcpExtensionMethod::Branch,
                        AcpExtensionMethod::Fork,
                        AcpExtensionMethod::Compact,
                        AcpExtensionMethod::PendingMessages,
                        AcpExtensionMethod::PendingMessageRemove,
                        AcpExtensionMethod::ClearQueue,
                        AcpExtensionMethod::SessionRename,
                        AcpExtensionMethod::InvokeSkill,
                        AcpExtensionMethod::SkillList,
                        AcpExtensionMethod::McpStatus,
                        AcpExtensionMethod::ExtensionCommand,
                        AcpExtensionMethod::UserBash,
                    ]
                    .into_iter()
                    .map(|method| serde_json::Value::String(method.to_string()))
                    .collect::<Vec<_>>();
                    let capabilities_meta = wire::Meta::from_iter([(
                        ProductIdentity::ACP_NAMESPACE.to_string(),
                        serde_json::json!({ "methods": methods }),
                    )]);
                        responder.respond(
                            wire::InitializeResponse::new(
                            ProtocolVersion::V2,
                            wire::Implementation::new(
                                ProductIdentity::SLUG,
                                env!("CARGO_PKG_VERSION"),
                            )
                            .title(ProductIdentity::NAME),
                        )
                        .capabilities(
                            wire::AgentCapabilities::new().session(
                                wire::SessionCapabilities::new().delete(
                                    wire::SessionDeleteCapabilities::new(),
                                ),
                            ),
                        )
                            .meta(capabilities_meta),
                        )
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::NewSessionRequest,
                            responder: Responder<wire::NewSessionResponse>,
                            connection: ConnectionTo<Client>| {
                    let trace_factory = session_traces.clone();
                    let kernel = Arc::clone(&session_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        let cwd = AcpWorkingDirectory::try_from(
                            request.cwd.into_inner(),
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        let session_id = kernel
                            .create_generated_session(cwd.into_inner())
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        responder.respond(wire::NewSessionResponse::new(
                            session_id.to_string(),
                        ))?;
                        AcpServer::new(kernel)
                            .send_available_commands(&session_id, &connection)
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::ListSessionsRequest,
                            responder: Responder<wire::ListSessionsResponse>,
                            _connection: ConnectionTo<Client>| {
                    let trace_factory = list_traces.clone();
                    let kernel = Arc::clone(&list_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        if request.cursor.is_some() {
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data("session/list cursor is not recognized"));
                        }
                        let cwd = request
                            .cwd
                            .as_ref()
                            .map(|path| path.as_ref());
                        let sessions = kernel
                            .list_sessions(cwd)
                            .map_err(agent_client_protocol::Error::into_internal_error)?
                            .into_iter()
                            .map(|session| {
                                wire::SessionInfo::new(
                                    session.session_id.to_string(),
                                    session.cwd,
                                )
                                .title(session.name)
                                .meta(wire::Meta::from_iter([(
                                    ProductIdentity::ACP_NAMESPACE.to_string(),
                                    serde_json::json!({
                                        "createdAtMs": session.created_at_ms.to_string(),
                                        "modifiedAtMs": session.modified_at_ms.to_string(),
                                        "parentSessionId": session.parent_session_id
                                            .map(|id| id.to_string()),
                                    }),
                                )]))
                            })
                            .collect();
                        responder.respond(wire::ListSessionsResponse::new(sessions))
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::ResumeSessionRequest,
                            responder: Responder<wire::ResumeSessionResponse>,
                            connection: ConnectionTo<Client>| {
                    let trace_factory = resume_traces.clone();
                    let kernel = Arc::clone(&resume_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        let replay_from_start = match request.replay_from {
                            Some(wire::ReplayFrom::Start(_)) => true,
                            Some(wire::ReplayFrom::Other(_)) => {
                                return Err(agent_client_protocol::Error::invalid_params()
                                    .data("unsupported session replay cursor"));
                            }
                            None => false,
                            _ => false,
                        };
                        kernel
                            .resume_session(
                                session_id.clone(),
                                AcpWorkingDirectory::try_from(
                                    request.cwd.into_inner(),
                                )
                                .map_err(|error| {
                                    agent_client_protocol::Error::invalid_params()
                                        .data(error.to_string())
                                })?
                                .into_inner(),
                            )
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        if replay_from_start {
                            for (index, message) in kernel
                                .session_transcript(&session_id)
                                .map_err(agent_client_protocol::Error::into_internal_error)?
                                .into_iter()
                                .enumerate()
                            {
                                let sequence = Sequence::try_from(
                                    u64::try_from(index)
                                        .unwrap_or(u64::MAX)
                                        .saturating_add(1),
                                )
                                .map_err(agent_client_protocol::Error::into_internal_error)?;
                                let event = AgentEvent {
                                    metadata: EventMetadata {
                                        turn_id: message.identity.turn_id.clone(),
                                        timestamp_ms: message.timing.ended_at_ms,
                                        sequence,
                                    },
                                    payload: AgentEventPayload::MessageEnd { message },
                                };
                                let metadata = AcpEventMapper::metadata(&event)
                                    .map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?;
                                for update in AcpEventMapper::map(event)
                                    .map_err(agent_client_protocol::Error::into_internal_error)?
                                {
                                    connection.send_notification(
                                        wire::UpdateSessionNotification::new(
                                            session_id.to_string(),
                                            update,
                                        )
                                        .meta(metadata.clone()),
                                    )?;
                                }
                            }
                        }
                        AcpServer::new(Arc::clone(&kernel))
                            .send_available_commands(&session_id, &connection)?;
                        responder.respond(wire::ResumeSessionResponse::new())
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::CloseSessionRequest,
                            responder: Responder<wire::CloseSessionResponse>,
                            _connection: ConnectionTo<Client>| {
                    let trace_factory = close_traces.clone();
                    let kernel = Arc::clone(&close_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        kernel
                            .close_session(&session_id)
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        responder.respond(wire::CloseSessionResponse::new())
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::DeleteSessionRequest,
                            responder: Responder<wire::DeleteSessionResponse>,
                            _connection: ConnectionTo<Client>| {
                    let trace_factory = delete_traces.clone();
                    let kernel = Arc::clone(&delete_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id =
                            SessionId::try_from(request.session_id.to_string())
                                .map_err(|error| {
                                    agent_client_protocol::Error::invalid_params()
                                        .data(error.to_string())
                                })?;
                        kernel
                            .delete_session(&session_id)
                            .await
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        responder.respond(wire::DeleteSessionResponse::new())
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: wire::PromptRequest,
                            responder: Responder<wire::PromptResponse>,
                            connection: ConnectionTo<Client>| {
                    let trace_factory = prompt_traces.clone();
                    let kernel = Arc::clone(&prompt_kernel);
                    let operation =
                        trace_factory.request(&responder, &request)?;
                    operation.start();
                    let task_operation = operation.clone();
                    let result = operation.instrument(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        let input = PromptInput::try_from(request.prompt)
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?
                            .0;
                        responder.respond(wire::PromptResponse::new())?;

                        let task_connection = connection.clone();
                        let task_session_id = session_id.clone();
                        connection.spawn(task_operation.settle(async move {
                            let sink: Arc<dyn EventSink> = Arc::new(AcpEventSink {
                                session_id: task_session_id.clone(),
                                connection: task_connection.clone(),
                            });
                            if let Err(error) = kernel
                                .run(
                                    RunRequest {
                                        session_id: task_session_id.clone(),
                                        input,
                                    },
                                    sink,
                                )
                                .await
                            {
                                tracing::error!(
                                    "Kernel Run for session {} failed: {}",
                                    task_session_id,
                                    error
                                );
                                task_connection.send_notification(
                                    wire::UpdateSessionNotification::new(
                                        task_session_id.to_string(),
                                        wire::SessionUpdate::StateUpdate(
                                            wire::StateUpdate::Idle(
                                                wire::IdleStateUpdate::new().stop_reason(
                                                    wire::StopReason::Other(format!(
                                                        "_{}/error",
                                                        ProductIdentity::ACP_NAMESPACE
                                                    )),
                                                ),
                                            ),
                                        ),
                                    )
                                    .meta(wire::Meta::from_iter([(
                                        ProductIdentity::ACP_NAMESPACE.to_string(),
                                        serde_json::json!({ "error": error.to_string() }),
                                    )])),
                                )?;
                            }
                            Ok(())
                        }))
                    }).await;
                    if let Err(error) = &result {
                        operation.fail(error);
                    }
                    result
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |notification: wire::CancelSessionNotification,
                            _connection: ConnectionTo<Client>| {
                    let trace_factory = cancel_traces.clone();
                    let kernel = Arc::clone(&cancel_kernel);
                    let operation = trace_factory.notification(
                        notification.method(),
                        &notification,
                    )?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(
                            notification.session_id.to_string(),
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        kernel
                            .cancel_session(&session_id)
                            .map_err(agent_client_protocol::Error::into_internal_error)
                    }).await
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: AcpExtensionRequest,
                            responder,
                            connection: ConnectionTo<Client>| {
                    let trace_factory = extension_traces.clone();
                    let kernel = Arc::clone(&extension_kernel);
                    let operation = trace_factory.request(
                        &responder,
                        request.parameters(),
                    )?;
                    operation.run(async move {
                        let response = AcpExtensionDispatcher {
                            kernel,
                            connection,
                        }
                        .execute(request)
                        .await?;
                        responder.respond(response)
                    }).await
                },
                agent_client_protocol::on_receive_request!(),
            )
    }
}

struct PromptInput(String);

impl TryFrom<Vec<wire::ContentBlock>> for PromptInput {
    type Error = PromptInputError;

    /// Converts ACP baseline text and resource links without client-side file reads.
    fn try_from(blocks: Vec<wire::ContentBlock>) -> Result<Self, Self::Error> {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                wire::ContentBlock::Text(text) => parts.push(text.text),
                wire::ContentBlock::ResourceLink(resource) => {
                    parts
                        .push(format!("[{}]({})", resource.name, resource.uri));
                }
                wire::ContentBlock::Other(other) => {
                    parts.push(serde_json::to_string(&other)?);
                }
                wire::ContentBlock::Image(_)
                | wire::ContentBlock::Audio(_)
                | wire::ContentBlock::Resource(_) => {
                    return Err(PromptInputError::UnsupportedContent);
                }
                _ => return Err(PromptInputError::UnsupportedContent),
            }
        }
        if parts.is_empty() {
            return Err(PromptInputError::Empty);
        }
        Ok(Self(parts.join("\n\n")))
    }
}

#[derive(Debug, thiserror::Error)]
enum PromptInputError {
    #[error("prompt must contain at least one supported content block")]
    Empty,
    #[error(
        "prompt content requires a capability this agent did not advertise"
    )]
    UnsupportedContent,
    #[error("custom prompt content could not be preserved: {0}")]
    Json(#[from] serde_json::Error),
}

pub(crate) struct AcpEventSink {
    session_id: SessionId,
    connection: ConnectionTo<Client>,
}

impl AcpEventSink {
    /// Binds one ACP connection sink to the session receiving notifications.
    pub(crate) fn new(
        session_id: SessionId,
        connection: ConnectionTo<Client>,
    ) -> Self {
        Self {
            session_id,
            connection,
        }
    }
}

#[async_trait]
impl EventSink for AcpEventSink {
    /// Converts and forwards every kernel event as ordered ACP v2 session updates.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError> {
        let metadata = AcpEventMapper::metadata(&event)
            .map_err(|error| SinkError::Consumer(error.to_string()))?;
        let updates = AcpEventMapper::map(event)
            .map_err(|error| SinkError::Consumer(error.to_string()))?;
        for update in updates {
            self.connection
                .send_notification(
                    wire::UpdateSessionNotification::new(
                        self.session_id.to_string(),
                        update,
                    )
                    .meta(metadata.clone()),
                )
                .map_err(|error| SinkError::Consumer(error.to_string()))?;
        }
        Ok(())
    }
}

impl From<AcpMappingError> for SinkError {
    /// Preserves mapping diagnostics at the kernel event-sink boundary.
    fn from(error: AcpMappingError) -> Self {
        Self::Consumer(error.to_string())
    }
}

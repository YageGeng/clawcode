//! ACP Agent bridging the clawcode kernel to the ACP protocol.

use std::sync::{Arc, Mutex};

use acp::schema::{
    ModelInfo as AcpModelInfo, SessionId as AcpSessionId, SessionMode as AcpSessionMode,
    StopReason as AcpStopReason, ToolCallStatus as AcpToolCallStatus, *,
};
use acp::{Agent, Client, ConnectTo, ConnectionTo, Error};
use agent_client_protocol as acp;
use futures::StreamExt;

use protocol::message::{AssistantContent, Message, ToolResultContent, UserContent};
use protocol::{AgentKernel, Event, SessionId};

/// ACP Agent bridging the clawcode kernel to the ACP protocol.
pub struct ClawcodeAgent {
    /// Reference to the kernel for session operations.
    kernel: Arc<dyn AgentKernel>,
    /// Capabilities reported by the connected ACP client.
    #[allow(dead_code)]
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
}

impl ClawcodeAgent {
    /// Create a new ACP agent with the given kernel.
    #[must_use]
    pub fn new(kernel: Arc<dyn AgentKernel>) -> Self {
        Self {
            kernel,
            client_capabilities: Arc::default(),
        }
    }

    /// Convert an internal SessionId to an ACP SessionId.
    fn to_acp_session_id(id: &SessionId) -> AcpSessionId {
        AcpSessionId::new(id.0.clone())
    }

    /// Convert kernel session modes into ACP mode state.
    fn to_acp_mode_state(modes: Vec<protocol::SessionMode>) -> SessionModeState {
        let acp_modes: Vec<AcpSessionMode> = modes
            .into_iter()
            .map(|mode| {
                let mut acp_mode =
                    AcpSessionMode::new(acp::schema::SessionModeId::new(mode.id), mode.name);
                if let Some(description) = mode.description {
                    acp_mode = acp_mode.description(description);
                }
                acp_mode
            })
            .collect();

        let first_mode_id = acp_modes
            .first()
            .map(|mode| mode.id.clone())
            .unwrap_or_else(|| acp::schema::SessionModeId::new("auto".to_string()));

        SessionModeState::new(first_mode_id, acp_modes)
    }

    /// Convert kernel model metadata into ACP model state.
    fn to_acp_model_state(models: Vec<protocol::ModelInfo>) -> SessionModelState {
        let acp_models: Vec<AcpModelInfo> = models
            .into_iter()
            .map(|model| {
                let mut info =
                    AcpModelInfo::new(acp::schema::ModelId::new(model.id), model.display_name);
                if let Some(description) = model.description {
                    info = info.description(description);
                }
                info
            })
            .collect();

        let first_model_id = acp_models
            .first()
            .map(|model| model.model_id.clone())
            .unwrap_or_else(|| acp::schema::ModelId::new("".to_string()));

        SessionModelState::new(first_model_id, acp_models)
    }

    /// Convert a persisted message into terminal-friendly transcript text.
    fn history_message_text(message: &Message) -> Option<String> {
        match message {
            Message::System { .. } => None,
            Message::User { content } => {
                let parts = content
                    .iter()
                    .filter_map(Self::user_content_text)
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    None
                } else {
                    Some(format!("\n> {}\n", parts.join("\n")))
                }
            }
            Message::Assistant { content, .. } => {
                let mut text = String::new();
                for content in content.iter() {
                    match content {
                        AssistantContent::Text(part) => text.push_str(&part.text),
                        _ => {
                            if let Some(part) = Self::assistant_content_text(content) {
                                if !text.is_empty() && !text.ends_with('\n') {
                                    text.push('\n');
                                }
                                text.push_str(&part);
                                text.push('\n');
                            }
                        }
                    }
                }
                if text.trim().is_empty() {
                    None
                } else {
                    Some(format!("{}\n", text.trim_end()))
                }
            }
        }
    }

    /// Convert persisted user content into transcript text.
    fn user_content_text(content: &UserContent) -> Option<String> {
        match content {
            UserContent::Text(text) => Some(text.text.clone()),
            UserContent::ToolResult(result) => {
                let parts = result
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        ToolResultContent::Text(text) => Some(text.text.clone()),
                        ToolResultContent::Image(_) => None,
                    })
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    None
                } else {
                    Some(format!("[tool result {}] {}", result.id, parts.join("\n")))
                }
            }
            UserContent::Image(_) => Some("[image]".to_string()),
            UserContent::Document(_) => Some("[document]".to_string()),
        }
    }

    /// Convert persisted assistant content into transcript text.
    fn assistant_content_text(content: &AssistantContent) -> Option<String> {
        match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            AssistantContent::ToolCall(tool_call) => Some(format!(
                "  [tool call] {} {}",
                tool_call.function.name, tool_call.function.arguments
            )),
            AssistantContent::Reasoning(reasoning) => {
                let parts = reasoning
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        protocol::message::ReasoningContent::Text { text, .. } => {
                            Some(text.clone())
                        }
                        protocol::message::ReasoningContent::Summary(text) => Some(text.clone()),
                        protocol::message::ReasoningContent::Encrypted(_)
                        | protocol::message::ReasoningContent::Redacted { .. } => None,
                    })
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    None
                } else {
                    Some(format!("[thought] {}", parts.join("\n")))
                }
            }
            AssistantContent::Image(_) => Some("[assistant image]".to_string()),
        }
    }

    /// Replay restored history to the ACP client as message chunks.
    async fn replay_history(
        session_id: &AcpSessionId,
        history: &[Message],
        cx: &ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if history.is_empty() {
            return Ok(());
        }

        let header = ContentChunk::new(ContentBlock::Text(TextContent::new(
            "\n--- restored history ---\n",
        )));
        cx.send_notification(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(header),
        ))?;

        for message in history {
            if let Some(text) = Self::history_message_text(message) {
                let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk(chunk),
                ))?;
            }
        }

        let footer = ContentChunk::new(ContentBlock::Text(TextContent::new(
            "--- end restored history ---\n",
        )));
        cx.send_notification(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(footer),
        ))?;
        Ok(())
    }

    /// Build and serve the ACP agent over the given transport.
    pub async fn serve(
        self: Arc<Self>,
        transport: impl ConnectTo<Agent> + 'static,
    ) -> acp::Result<()> {
        let agent = self;
        Agent
            .builder()
            .name("claw-acp")
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: InitializeRequest, responder, _cx| {
                        responder.respond_with_result(agent.handle_initialize(request).await)
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: AuthenticateRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_authenticate(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_new_session(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            responder
                                .respond_with_result(agent.handle_load_session(request, cx2).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: ListSessionsRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_list_sessions(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_prompt(request, cx2).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                {
                    let agent = agent.clone();
                    async move |notification: CancelNotification, cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            if let Err(e) = agent.handle_cancel(notification).await {
                                tracing::error!("Error handling cancel: {:?}", e);
                            }
                            Ok(())
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: SetSessionModeRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_set_mode(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: SetSessionModelRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_set_model(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = agent.clone();
                    async move |request: CloseSessionRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(agent.handle_close_session(request).await)
                        })?;
                        Ok(())
                    }
                },
                acp::on_receive_request!(),
            )
            .connect_to(transport)
            .await
    }

    // ── Handler implementations ──

    async fn handle_initialize(
        &self,
        _request: InitializeRequest,
    ) -> Result<InitializeResponse, Error> {
        let protocol_version = acp::schema::ProtocolVersion::V1;

        let mut caps = AgentCapabilities::new()
            .prompt_capabilities(PromptCapabilities::new().embedded_context(true).image(true))
            .mcp_capabilities(McpCapabilities::new().http(true))
            .load_session(true)
            .auth(AgentAuthCapabilities::new().logout(LogoutCapabilities::new()));

        caps.session_capabilities = SessionCapabilities::new()
            .close(SessionCloseCapabilities::new())
            .list(SessionListCapabilities::new());

        Ok(InitializeResponse::new(protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                Implementation::new("claw-acp", env!("CARGO_PKG_VERSION")).title("Clawcode"),
            ))
    }

    async fn handle_authenticate(
        &self,
        _request: AuthenticateRequest,
    ) -> Result<AuthenticateResponse, Error> {
        Ok(AuthenticateResponse::new())
    }

    async fn handle_new_session(
        &self,
        request: NewSessionRequest,
    ) -> Result<NewSessionResponse, Error> {
        let NewSessionRequest { cwd, .. } = request;
        // ACP clients may send a relative cwd; resolve it in the agent process
        // before passing it to tool execution.
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            std::env::current_dir()
                .map_err(|e| Error::internal_error().data(e.to_string()))?
                .join(cwd)
        };

        let created = self
            .kernel
            .new_session(cwd)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        let acp_session_id = Self::to_acp_session_id(&created.session_id);
        let mode_state = Self::to_acp_mode_state(created.modes);
        let model_state = Self::to_acp_model_state(created.models);

        Ok(NewSessionResponse::new(acp_session_id)
            .modes(mode_state)
            .models(model_state))
    }

    /// Load a persisted session through the kernel and return initial ACP state.
    async fn handle_load_session(
        &self,
        request: LoadSessionRequest,
        cx: ConnectionTo<Client>,
    ) -> Result<LoadSessionResponse, Error> {
        let session_id = SessionId(request.session_id.0.to_string());
        let created = self
            .kernel
            .load_session(&session_id)
            .await
            .map_err(|error| Error::internal_error().data(error.to_string()))?;

        let acp_session_id = Self::to_acp_session_id(&created.session_id);
        Self::replay_history(&acp_session_id, &created.history, &cx).await?;

        let mode_state = Self::to_acp_mode_state(created.modes);
        let model_state = Self::to_acp_model_state(created.models);

        Ok(LoadSessionResponse::new()
            .modes(mode_state)
            .models(model_state))
    }

    /// List persisted sessions through the kernel and convert them into ACP session summaries.
    async fn handle_list_sessions(
        &self,
        request: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, Error> {
        let page = self
            .kernel
            .list_sessions(request.cwd.as_deref(), request.cursor.as_deref())
            .await
            .map_err(|error| Error::internal_error().data(error.to_string()))?;

        let sessions = page
            .sessions
            .into_iter()
            .map(|session| {
                SessionInfo::new(Self::to_acp_session_id(&session.session_id), session.cwd)
                    .title(session.title)
                    .updated_at(session.updated_at)
            })
            .collect();

        Ok(ListSessionsResponse::new(sessions).next_cursor(page.next_cursor))
    }

    async fn handle_prompt(
        &self,
        request: PromptRequest,
        cx: ConnectionTo<Client>,
    ) -> Result<PromptResponse, Error> {
        let session_id = SessionId(request.session_id.0.to_string());

        // Extract text from prompt blocks
        let text = request
            .prompt
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let acp_sid = AcpSessionId::new(request.session_id.0.to_string());

        // Call kernel and get event stream
        let mut events = self
            .kernel
            .prompt(&session_id, text)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        // Translate events to ACP notifications
        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::internal_error().data(e.to_string()))?;
            match event {
                Event::AgentMessageChunk { text, .. } => {
                    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                    let update = SessionUpdate::AgentMessageChunk(chunk);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::AgentThoughtChunk { text, .. } => {
                    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                    let update = SessionUpdate::AgentThoughtChunk(chunk);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::ToolCallDelta {
                    call_id, content, ..
                } => {
                    // Stream incremental tool call building to the client.
                    let mut fields = ToolCallUpdateFields::default();
                    match content {
                        protocol::event::ToolCallDeltaContent::Name(n) => {
                            fields.title = Some(n);
                        }
                        protocol::event::ToolCallDeltaContent::Delta(d) => {
                            fields.content = Some(vec![ToolCallContent::Content(Content::new(
                                ContentBlock::Text(TextContent::new(d)),
                            ))]);
                        }
                    }
                    let update_val = ToolCallUpdate::new(ToolCallId::new(call_id), fields);
                    let update = SessionUpdate::ToolCallUpdate(update_val);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::ToolCall {
                    call_id,
                    name,
                    arguments,
                    status,
                    ..
                } => {
                    let acp_status: AcpToolCallStatus = status.into();
                    let tool_call = ToolCall::new(ToolCallId::new(call_id), name)
                        .status(acp_status)
                        .raw_input(arguments);
                    let update = SessionUpdate::ToolCall(tool_call);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::ToolCallUpdate {
                    call_id,
                    output_delta,
                    status,
                    ..
                } => {
                    let mut fields = ToolCallUpdateFields::default();
                    if let Some(delta) = output_delta {
                        fields.content = Some(vec![ToolCallContent::Content(Content::new(
                            ContentBlock::Text(TextContent::new(delta)),
                        ))]);
                    }
                    if let Some(s) = status {
                        let acp_status: AcpToolCallStatus = s.into();
                        fields.status = Some(acp_status);
                    }
                    let update_val = ToolCallUpdate::new(ToolCallId::new(call_id), fields);
                    let update = SessionUpdate::ToolCallUpdate(update_val);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::PlanUpdate { entries, .. } => {
                    let plan_entries: Vec<PlanEntry> = entries
                        .into_iter()
                        .map(|e| PlanEntry::new(e.name, e.priority.into(), e.status.into()))
                        .collect();
                    let update = SessionUpdate::Plan(Plan::new(plan_entries));
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::UsageUpdate {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    let usage = UsageUpdate::new(input_tokens + output_tokens, 0);
                    let update = SessionUpdate::UsageUpdate(usage);
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::ExecApprovalRequested {
                    call_id,
                    tool_name,
                    arguments,
                    ..
                } => {
                    // Build a minimal ToolCallUpdate to describe the permission request
                    let mut tc_fields = ToolCallUpdateFields::default();
                    tc_fields.status = Some(ToolCallStatus::Pending);
                    tc_fields.title = Some(tool_name.clone());
                    tc_fields.content = Some(vec![ToolCallContent::Content(Content::new(
                        ContentBlock::Text(TextContent::new(format!("{tool_name}: {arguments}"))),
                    ))]);

                    let tc_update =
                        ToolCallUpdate::new(ToolCallId::new(call_id.clone()), tc_fields);

                    let perm_req = RequestPermissionRequest::new(
                        acp_sid.clone(),
                        tc_update,
                        vec![
                            PermissionOption::new(
                                "allow_once",
                                "Allow Once",
                                PermissionOptionKind::AllowOnce,
                            ),
                            PermissionOption::new(
                                "reject_once",
                                "Reject",
                                PermissionOptionKind::RejectOnce,
                            ),
                        ],
                    );

                    let resp: RequestPermissionResponse =
                        cx.send_request(perm_req).block_task().await?;

                    let decision = match &resp.outcome {
                        RequestPermissionOutcome::Selected(sel) => match sel.option_id.0.as_ref() {
                            "allow_once" | "allow_always" => protocol::ReviewDecision::AllowOnce,
                            _ => protocol::ReviewDecision::RejectOnce,
                        },
                        _ => protocol::ReviewDecision::RejectOnce,
                    };

                    let _ = self
                        .kernel
                        .resolve_approval(&session_id, &call_id, decision)
                        .await;
                }
                Event::TurnComplete { stop_reason, .. } => {
                    let reason: AcpStopReason = stop_reason.into();
                    return Ok(PromptResponse::new(reason));
                }
                _ => {}
            }
        }

        Ok(PromptResponse::new(AcpStopReason::EndTurn))
    }

    async fn handle_cancel(&self, notification: CancelNotification) -> Result<(), Error> {
        let session_id = SessionId(notification.session_id.0.to_string());
        self.kernel
            .cancel(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))
    }

    async fn handle_set_mode(
        &self,
        request: SetSessionModeRequest,
    ) -> Result<SetSessionModeResponse, Error> {
        let session_id = SessionId(request.session_id.0.to_string());
        self.kernel
            .set_mode(&session_id, &request.mode_id.0)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(SetSessionModeResponse::default())
    }

    async fn handle_set_model(
        &self,
        request: SetSessionModelRequest,
    ) -> Result<SetSessionModelResponse, Error> {
        let session_id = SessionId(request.session_id.0.to_string());
        let parts: Vec<&str> = request.model_id.0.splitn(2, '/').collect();
        let (provider_id, model_id) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            ("", parts[0])
        };
        self.kernel
            .set_model(&session_id, provider_id, model_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(SetSessionModelResponse::default())
    }

    async fn handle_close_session(
        &self,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, Error> {
        let session_id = SessionId(request.session_id.0.to_string());
        self.kernel
            .close_session(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        Ok(CloseSessionResponse::new())
    }
}

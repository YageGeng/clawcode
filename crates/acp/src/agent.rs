//! ACP Agent bridging the clawcode kernel to the ACP protocol.

use std::sync::{Arc, Mutex};

use acp::schema::{
    ModelInfo as AcpModelInfo, SessionId as AcpSessionId, SessionMode as AcpSessionMode,
    StopReason as AcpStopReason, ToolCallStatus as AcpToolCallStatus, *,
};
use acp::{Agent, Client, ConnectTo, ConnectionTo, Error};
use agent_client_protocol as acp;
use futures::StreamExt;

use protocol::acp_conv::TurnItemAcpExt;
use protocol::mcp::{McpServerConfig, McpTransportConfig};
use protocol::message::{AssistantContent, Message, ToolResult, ToolResultContent, UserContent};
use protocol::{AgentKernel, Event, SessionId, SessionLaunchOptions};

use crate::backend::fs::AcpClientFsRouter;
use crate::backend::terminal::AcpClientTerminalRouter;

/// ACP Agent bridging the clawcode kernel to the ACP protocol.
pub struct ClawcodeAgent {
    /// Reference to the kernel for session operations.
    kernel: Arc<dyn AgentKernel>,
    /// Capabilities reported by the connected ACP client.
    #[allow(dead_code)]
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
    /// Routes ACP filesystem backend calls to client sessions.
    fs_router: Arc<AcpClientFsRouter>,
    /// Routes ACP terminal backend calls to client sessions.
    terminal_router: Arc<AcpClientTerminalRouter>,
}

impl ClawcodeAgent {
    /// Create a new ACP agent with the given kernel (default routers).
    #[must_use]
    pub fn new(kernel: Arc<dyn AgentKernel>) -> Self {
        Self::with_routers(
            kernel,
            Arc::new(AcpClientFsRouter::default()),
            Arc::new(AcpClientTerminalRouter::default()),
        )
    }

    /// Create a new ACP agent with a shared filesystem router.
    #[must_use]
    pub fn with_fs_router(kernel: Arc<dyn AgentKernel>, fs_router: Arc<AcpClientFsRouter>) -> Self {
        Self::with_routers(
            kernel,
            fs_router,
            Arc::new(AcpClientTerminalRouter::default()),
        )
    }

    /// Create a new ACP agent with shared filesystem and terminal routers.
    #[must_use]
    pub fn with_routers(
        kernel: Arc<dyn AgentKernel>,
        fs_router: Arc<AcpClientFsRouter>,
        terminal_router: Arc<AcpClientTerminalRouter>,
    ) -> Self {
        Self {
            kernel,
            client_capabilities: Arc::default(),
            fs_router,
            terminal_router,
        }
    }

    /// Return the latest client capabilities snapshot.
    fn client_capabilities_snapshot(&self) -> ClientCapabilities {
        self.client_capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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

    /// Resolve an ACP request cwd against the current process directory when needed.
    fn resolve_request_cwd(cwd: std::path::PathBuf) -> Result<std::path::PathBuf, Error> {
        if cwd.is_absolute() {
            return Ok(cwd);
        }

        std::env::current_dir()
            .map_err(|e| Error::internal_error().data(e.to_string()))
            .map(|current_dir| current_dir.join(cwd))
    }

    /// Convert ACP MCP server declarations into kernel session launch options.
    fn launch_options_from_mcp_servers(
        mcp_servers: Vec<McpServer>,
        cwd: &std::path::Path,
    ) -> Result<SessionLaunchOptions, Error> {
        let external_mcp_servers = mcp_servers
            .into_iter()
            .map(McpServerConfig::try_from)
            .map(|config| config.map(|config| Self::with_session_cwd(config, cwd)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| Error::internal_error().data(error.to_string()))?;

        Ok(SessionLaunchOptions {
            external_mcp_servers,
        })
    }

    /// Attach the session cwd to stdio MCP configs supplied through ACP.
    fn with_session_cwd(mut config: McpServerConfig, cwd: &std::path::Path) -> McpServerConfig {
        if let McpTransportConfig::Stdio {
            cwd: transport_cwd, ..
        } = &mut config.transport
        {
            *transport_cwd = Some(cwd.to_path_buf());
        }
        config
    }

    /// Convert one persisted message into ACP replay updates while preserving content order.
    fn history_replay_updates(message: &Message) -> Vec<SessionUpdate> {
        match message {
            Message::System { .. } => Vec::new(),
            Message::User { content } => content
                .iter()
                .filter_map(Self::user_content_update)
                .collect(),
            Message::Assistant { content, .. } => Self::assistant_replay_updates(content.iter()),
        }
    }

    /// Convert assistant content into ordered replay updates with contiguous text merged.
    fn assistant_replay_updates<'a>(
        content: impl IntoIterator<Item = &'a AssistantContent>,
    ) -> Vec<SessionUpdate> {
        let mut updates = Vec::new();
        let mut pending_text = String::new();

        for content in content {
            match content {
                AssistantContent::Text(text) => pending_text.push_str(&text.text),
                _ => {
                    Self::flush_pending_agent_text(&mut updates, &mut pending_text);
                    if let Some(update) = Self::assistant_content_update(content) {
                        updates.push(update);
                    }
                }
            }
        }

        Self::flush_pending_agent_text(&mut updates, &mut pending_text);
        updates
    }

    /// Push a pending assistant text update before replaying a non-text content item.
    fn flush_pending_agent_text(updates: &mut Vec<SessionUpdate>, pending_text: &mut String) {
        if pending_text.is_empty() {
            return;
        }

        updates.push(Self::agent_message_update(std::mem::take(pending_text)));
    }

    /// Convert one persisted user content item into an ACP replay update.
    fn user_content_update(content: &UserContent) -> Option<SessionUpdate> {
        match content {
            UserContent::Text(text) => {
                Some(Self::agent_message_update(format!("\n> {}\n", text.text)))
            }
            UserContent::ToolResult(result) => Some(Self::tool_result_update(result)),
            UserContent::Image(_) => Some(Self::agent_message_update("\n> [image]\n")),
            UserContent::Document(_) => Some(Self::agent_message_update("\n> [document]\n")),
        }
    }

    /// Convert one persisted assistant content item into an ACP replay update.
    fn assistant_content_update(content: &AssistantContent) -> Option<SessionUpdate> {
        match content {
            AssistantContent::Text(text) => Some(Self::agent_message_update(text.text.clone())),
            AssistantContent::ToolCall(tool_call) => Some(SessionUpdate::ToolCall(
                ToolCall::new(
                    ToolCallId::new(tool_call.id.clone()),
                    tool_call.function.name.clone(),
                )
                .status(AcpToolCallStatus::Completed)
                .raw_input(tool_call.function.arguments.clone()),
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
                    Some(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(parts.join("\n"))),
                    )))
                }
            }
            AssistantContent::Image(_) => Some(Self::agent_message_update("[assistant image]")),
        }
    }

    /// Build an ACP assistant message chunk for replay text.
    fn agent_message_update(text: impl Into<String>) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text.into(),
        ))))
    }

    /// Convert a persisted tool result into the ACP update shape used by live tool output.
    fn tool_result_update(result: &ToolResult) -> SessionUpdate {
        let mut fields = ToolCallUpdateFields::default();
        fields.status = Some(AcpToolCallStatus::Completed);

        let parts = result
            .content
            .iter()
            .filter_map(|content| match content {
                ToolResultContent::Text(text) => Some(text.text.clone()),
                ToolResultContent::Image(_) => None,
            })
            .collect::<Vec<_>>();

        if !parts.is_empty() {
            // Keep the complete persisted output in the protocol event; the TUI
            // applies the preview limit at render time just like it does live.
            fields.content = Some(vec![ToolCallContent::Content(Content::new(
                ContentBlock::Text(TextContent::new(parts.join("\n"))),
            ))]);
        }

        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            ToolCallId::new(result.id.clone()),
            fields,
        ))
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
            for update in Self::history_replay_updates(message) {
                cx.send_notification(SessionNotification::new(session_id.clone(), update))?;
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
                        let cx2 = cx.clone();
                        cx.spawn(async move {
                            responder
                                .respond_with_result(agent.handle_new_session(request, cx2).await)
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
        request: InitializeRequest,
    ) -> Result<InitializeResponse, Error> {
        let protocol_version = acp::schema::ProtocolVersion::V1;
        *self
            .client_capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = request.client_capabilities;

        let mut caps = AgentCapabilities::new()
            .prompt_capabilities(PromptCapabilities::new().embedded_context(true))
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
        cx: ConnectionTo<Client>,
    ) -> Result<NewSessionResponse, Error> {
        let NewSessionRequest {
            cwd, mcp_servers, ..
        } = request;
        // ACP clients may send a relative cwd; resolve it in the agent process
        // before passing it to tool execution.
        let cwd = Self::resolve_request_cwd(cwd)?;
        let options = Self::launch_options_from_mcp_servers(mcp_servers, &cwd)?;

        let created = self
            .kernel
            .new_session(cwd, options)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        let acp_session_id = Self::to_acp_session_id(&created.session_id);
        let mode_state = Self::to_acp_mode_state(created.modes);
        let model_state = Self::to_acp_model_state(created.models);
        self.fs_router.register_session(
            created.session_id.clone(),
            cx.clone(),
            self.client_capabilities_snapshot(),
        );
        self.terminal_router.register_session(
            created.session_id,
            cx,
            self.client_capabilities_snapshot(),
        );

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
        let LoadSessionRequest {
            session_id,
            cwd,
            mcp_servers,
            ..
        } = request;
        let session_id = SessionId(session_id.0.to_string());
        let cwd = Self::resolve_request_cwd(cwd)?;
        let options = Self::launch_options_from_mcp_servers(mcp_servers, &cwd)?;
        let created = self
            .kernel
            .load_session(&session_id, cwd, options)
            .await
            .map_err(|error| Error::internal_error().data(error.to_string()))?;

        let acp_session_id = Self::to_acp_session_id(&created.session_id);
        Self::replay_history(&acp_session_id, &created.history, &cx).await?;

        let mode_state = Self::to_acp_mode_state(created.modes);
        let model_state = Self::to_acp_model_state(created.models);
        self.fs_router.register_session(
            created.session_id.clone(),
            cx.clone(),
            self.client_capabilities_snapshot(),
        );
        self.terminal_router.register_session(
            created.session_id,
            cx,
            self.client_capabilities_snapshot(),
        );

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
                Event::ItemStarted { item, .. } => {
                    if let Some(update) = item.start() {
                        let _ =
                            cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                    }
                }
                Event::ItemCompleted { item, .. } => {
                    if let Some(update) = item.end() {
                        let _ =
                            cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                    }
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
        // SAFETY: splitn(2, '/') guarantees the Vec has at least 1 element.
        // The len == 2 check ensures both parts[0] and parts[1] are valid.
        #[allow(clippy::indexing_slicing)]
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
        self.fs_router.unregister_session(&session_id);
        self.terminal_router.unregister_session(&session_id);
        Ok(CloseSessionResponse::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream;
    use protocol::mcp::McpTransportConfig;
    use protocol::{
        AgentPath, EventStream, KernelError, ModelInfo, OneOrMany, ReviewDecision, SessionCreated,
        SessionInfo, SessionLaunchOptions, SessionListPage, SessionMode, Text, ToolFunction,
    };
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecordingKernel {
        new_session_options: std::sync::Mutex<Option<SessionLaunchOptions>>,
        load_session_options: std::sync::Mutex<Option<SessionLaunchOptions>>,
    }

    #[async_trait]
    impl AgentKernel for RecordingKernel {
        /// Record new-session launch options for ACP handler tests.
        async fn new_session(
            &self,
            _cwd: PathBuf,
            options: SessionLaunchOptions,
        ) -> Result<SessionCreated, KernelError> {
            *self
                .new_session_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(options);
            Ok(session_created())
        }

        /// Record load-session launch options for ACP handler tests.
        async fn load_session(
            &self,
            _session_id: &SessionId,
            _cwd: PathBuf,
            options: SessionLaunchOptions,
        ) -> Result<SessionCreated, KernelError> {
            *self
                .load_session_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(options);
            Ok(session_created())
        }

        /// Return an empty session list; list behavior is outside these tests.
        async fn list_sessions(
            &self,
            _cwd: Option<&Path>,
            _cursor: Option<&str>,
        ) -> Result<SessionListPage, KernelError> {
            Ok(SessionListPage {
                sessions: Vec::<SessionInfo>::new(),
                next_cursor: None,
            })
        }

        /// Return an empty event stream; prompting is outside these tests.
        async fn prompt(
            &self,
            _session_id: &SessionId,
            _text: String,
        ) -> Result<EventStream, KernelError> {
            Ok(Box::pin(stream::empty()))
        }

        /// Accept cancellation in the fake kernel.
        async fn cancel(&self, _session_id: &SessionId) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept mode changes in the fake kernel.
        async fn set_mode(&self, _session_id: &SessionId, _mode: &str) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept model changes in the fake kernel.
        async fn set_model(
            &self,
            _session_id: &SessionId,
            _provider_id: &str,
            _model_id: &str,
        ) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept session close in the fake kernel.
        async fn close_session(&self, _session_id: &SessionId) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept sub-agent spawns in the fake kernel.
        async fn spawn_agent(
            &self,
            _parent_session: &SessionId,
            _agent_path: AgentPath,
            _role: &str,
            _prompt: &str,
        ) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept approval decisions in the fake kernel.
        async fn resolve_approval(
            &self,
            _session_id: &SessionId,
            _call_id: &str,
            _decision: ReviewDecision,
        ) -> Result<(), KernelError> {
            Ok(())
        }

        /// Return no modes; ACP conversion has fallbacks for empty state.
        fn available_modes(&self) -> Vec<SessionMode> {
            Vec::new()
        }

        /// Return no models; ACP conversion has fallbacks for empty state.
        fn available_models(&self) -> Vec<ModelInfo> {
            Vec::new()
        }
    }

    /// Build a minimal session-created response for ACP handler tests.
    fn session_created() -> SessionCreated {
        SessionCreated::builder()
            .session_id(protocol::SessionId("session-1".to_string()))
            .modes(Vec::new())
            .models(Vec::new())
            .build()
    }

    /// Extract exactly one external MCP config from recorded launch options.
    fn recorded_mcp_config(
        options: &std::sync::Mutex<Option<SessionLaunchOptions>>,
    ) -> protocol::mcp::McpServerConfig {
        options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("launch options should be recorded")
            .external_mcp_servers
            .first()
            .expect("external MCP config should be forwarded")
            .clone()
    }

    /// Create an ACP connection handle for load-session replay tests.
    async fn test_connection_to_client() -> ConnectionTo<Client> {
        let (agent_channel, _client_channel) = acp::Channel::duplex();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = Agent
                .builder()
                .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                    let _ = tx.send(cx);
                    std::future::pending::<Result<(), Error>>().await
                })
                .await;
        });
        rx.await.expect("test ACP connection should start")
    }

    /// Build a persisted assistant tool call message for replay tests.
    fn assistant_tool_call_message() -> Message {
        protocol::message::ToolCall::new(
            "call-1".to_string(),
            ToolFunction::new(
                "shell".to_string(),
                serde_json::json!({"cmd": "printf 'one\\ntwo\\n'"}),
            ),
        )
        .into()
    }

    #[tokio::test]
    async fn initialize_records_client_fs_capabilities_and_omits_image() {
        let kernel = Arc::new(RecordingKernel::default());
        let agent = ClawcodeAgent::new(kernel);
        let request = InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
            ClientCapabilities::new().fs(FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true)),
        );

        let response = agent
            .handle_initialize(request)
            .await
            .expect("initialize should succeed");
        let stored = agent
            .client_capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        assert!(stored.fs.read_text_file);
        assert!(stored.fs.write_text_file);
        assert!(!response.agent_capabilities.prompt_capabilities.image);
    }

    #[tokio::test]
    async fn new_session_forwards_acp_mcp_servers_to_kernel_options() {
        let kernel = Arc::new(RecordingKernel::default());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request =
            NewSessionRequest::new(PathBuf::from("/tmp")).mcp_servers(vec![McpServer::Stdio(
                McpServerStdio::new("filesystem", "/usr/bin/mcp"),
            )]);
        let client = test_connection_to_client().await;

        agent
            .handle_new_session(request, client)
            .await
            .expect("new session should succeed");

        let config = recorded_mcp_config(&kernel.new_session_options);
        assert_eq!(config.name, "filesystem");
        assert!(config.external);
        let McpTransportConfig::Stdio { cwd, .. } = config.transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(cwd, Some(PathBuf::from("/tmp")));
    }

    #[tokio::test]
    async fn load_session_forwards_acp_mcp_servers_to_kernel_options() {
        let kernel = Arc::new(RecordingKernel::default());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request =
            LoadSessionRequest::new(AcpSessionId::new("session-1"), PathBuf::from("/tmp"))
                .mcp_servers(vec![McpServer::Http(McpServerHttp::new(
                    "remote",
                    "https://example.com/mcp",
                ))]);
        let client = test_connection_to_client().await;

        agent
            .handle_load_session(request, client)
            .await
            .expect("load session should succeed");

        let config = recorded_mcp_config(&kernel.load_session_options);
        assert_eq!(config.name, "remote");
        assert!(config.external);
        assert!(matches!(
            config.transport,
            McpTransportConfig::StreamableHttp { .. }
        ));
    }

    #[test]
    fn replay_history_converts_tool_calls_to_structured_updates() {
        let message = assistant_tool_call_message();

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 1);

        let SessionUpdate::ToolCall(tool_call) = &updates[0] else {
            panic!("expected a structured tool call update");
        };

        assert_eq!(tool_call.tool_call_id.to_string(), "call-1");
        assert_eq!(tool_call.title, "shell");
        assert_eq!(tool_call.status, AcpToolCallStatus::Completed);
        assert_eq!(
            tool_call.raw_input,
            Some(serde_json::json!({"cmd": "printf 'one\\ntwo\\n'"}))
        );
    }

    #[test]
    fn replay_history_converts_tool_results_to_structured_updates() {
        let message = Message::tool_result("call-1", "one\ntwo\nthree\nfour\nfive\nsix");

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 1);

        let SessionUpdate::ToolCallUpdate(update) = &updates[0] else {
            panic!("expected a structured tool result update");
        };

        assert_eq!(update.tool_call_id.to_string(), "call-1");
        assert_eq!(update.fields.status, Some(AcpToolCallStatus::Completed));

        let content = update
            .fields
            .content
            .as_ref()
            .and_then(|content| content.first())
            .expect("tool output should be replayed as content");
        let ToolCallContent::Content(content) = content else {
            panic!("expected content-backed tool output");
        };
        let ContentBlock::Text(text) = &content.content else {
            panic!("expected text tool output");
        };

        assert_eq!(text.text, "one\ntwo\nthree\nfour\nfive\nsix");
    }

    #[test]
    fn replay_history_marks_image_only_tool_results_completed() {
        let message = Message::from(ToolResult {
            id: "call-image".to_string(),
            call_id: None,
            content: OneOrMany::one(ToolResultContent::Image(protocol::message::Image {
                data: protocol::message::DocumentSourceKind::unknown(),
                media_type: None,
                detail: None,
                additional_params: None,
            })),
        });

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 1);

        let SessionUpdate::ToolCallUpdate(update) = &updates[0] else {
            panic!("expected a structured tool result update");
        };

        assert_eq!(update.tool_call_id.to_string(), "call-image");
        assert_eq!(update.fields.status, Some(AcpToolCallStatus::Completed));
        assert!(update.fields.content.is_none());
    }

    #[test]
    fn replay_history_keeps_regular_text_as_transcript_text() {
        let message = Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: "hello".to_string(),
            })),
        };

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 1);

        let SessionUpdate::AgentMessageChunk(chunk) = &updates[0] else {
            panic!("expected regular user text to replay as transcript text");
        };
        let ContentBlock::Text(text) = &chunk.content else {
            panic!("expected text user content");
        };

        assert_eq!(text.text, "\n> hello\n");
    }

    #[test]
    fn replay_history_keeps_reasoning_out_of_assistant_text() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::reasoning("hidden reasoning")),
        };

        let updates = ClawcodeAgent::history_replay_updates(&message);

        assert!(
            updates
                .iter()
                .all(|update| !matches!(update, SessionUpdate::AgentMessageChunk(_)))
        );
    }

    #[test]
    fn replay_history_converts_reasoning_to_thought_chunks() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::reasoning("hidden reasoning")),
        };

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 1);

        let SessionUpdate::AgentThoughtChunk(chunk) = &updates[0] else {
            panic!("expected a structured thought chunk");
        };
        let ContentBlock::Text(text) = &chunk.content else {
            panic!("expected text thought content");
        };

        assert_eq!(text.text, "hidden reasoning");
    }

    #[test]
    fn replay_history_preserves_assistant_content_order() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::reasoning("thinking first"),
                AssistantContent::text("answer second"),
            ])
            .expect("non-empty assistant content"),
        };

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 2);

        let SessionUpdate::AgentThoughtChunk(thought) = &updates[0] else {
            panic!("expected reasoning to be replayed before answer text");
        };
        let ContentBlock::Text(thought_text) = &thought.content else {
            panic!("expected text thought content");
        };
        assert_eq!(thought_text.text, "thinking first");

        let SessionUpdate::AgentMessageChunk(answer) = &updates[1] else {
            panic!("expected answer text after reasoning");
        };
        let ContentBlock::Text(answer_text) = &answer.content else {
            panic!("expected text answer content");
        };
        assert_eq!(answer_text.text, "answer second");
    }

    #[test]
    fn replay_history_combines_contiguous_assistant_text_chunks() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::reasoning("thinking first"),
                AssistantContent::text("answer "),
                AssistantContent::text("second"),
            ])
            .expect("non-empty assistant content"),
        };

        let updates = ClawcodeAgent::history_replay_updates(&message);
        assert_eq!(updates.len(), 2);

        let SessionUpdate::AgentMessageChunk(answer) = &updates[1] else {
            panic!("expected contiguous answer text to replay as one chunk");
        };
        let ContentBlock::Text(answer_text) = &answer.content else {
            panic!("expected text answer content");
        };

        assert_eq!(answer_text.text, "answer second");
    }
}

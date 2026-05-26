//! ACP Agent bridging the clawcode kernel to the ACP protocol.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acp::schema::{
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigSelectGroup,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionConfigValueId,
    SessionId as AcpSessionId, StopReason as AcpStopReason, ToolCallStatus as AcpToolCallStatus, *,
};
use acp::{Agent, Client, ConnectTo, ConnectionTo, Error};
use agent_client_protocol as acp;
use futures::StreamExt;

use protocol::acp_conv::TurnItemAcpExt;
use protocol::mcp::{McpServerConfig, McpTransportConfig};
use protocol::message::{AssistantContent, Message, ToolResult, ToolResultContent, UserContent};
use protocol::{AgentKernel, Event, SessionId, SessionLaunchOptions, Usage};

use crate::backend::fs::AcpClientFsRouter;
use crate::backend::terminal::AcpClientTerminalRouter;

const SUBAGENT_METADATA_TOOL_CALL_ID: &str = "clawcode-subagents";

/// ACP Agent bridging the clawcode kernel to the ACP protocol.
pub struct ClawcodeAgent {
    /// Reference to the kernel for session operations.
    kernel: Arc<dyn AgentKernel>,
    /// Capabilities reported by the connected ACP client.
    client_capabilities: Arc<Mutex<ClientCapabilities>>,
    /// Routes ACP filesystem backend calls to client sessions.
    fs_router: Arc<AcpClientFsRouter>,
    /// Routes ACP terminal backend calls to client sessions.
    terminal_router: Arc<AcpClientTerminalRouter>,
    /// Session-scoped configuration options tracked for supported `set_session_config_option` calls.
    session_configs: Arc<Mutex<HashMap<protocol::SessionId, Vec<SessionConfigOption>>>>,
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
            session_configs: Arc::default(),
        }
    }

    /// Return the latest client capabilities snapshot.
    fn client_capabilities_snapshot(&self) -> ClientCapabilities {
        self.client_capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Build a metadata-only ACP update for the TUI agent picker.
    fn subagent_metadata_update(
        event: protocol::AgentUiEventKind,
        agents: Vec<protocol::AgentUiMetadata>,
    ) -> SessionUpdate {
        let patch = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(event)
            .agents(agents)
            .build();

        let meta = serde_json::json!({
            "clawcode": {
                "subagents": patch,
            }
        })
        .as_object()
        .cloned()
        .expect("metadata root must be an object");

        SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(
                ToolCallId::new(SUBAGENT_METADATA_TOOL_CALL_ID),
                ToolCallUpdateFields::default(),
            )
            .meta(meta),
        )
    }

    /// Build and store the default session configuration options for a session.
    fn set_session_config_defaults(
        &self,
        session_id: protocol::SessionId,
        modes: &[protocol::config::SessionMode],
        models: &[protocol::ModelInfo],
    ) -> Vec<SessionConfigOption> {
        let config_options = Self::build_session_config_options(modes, models);

        self.session_configs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id, config_options.clone());

        config_options
    }

    /// Build the default set of config options from available session modes and models.
    fn build_session_config_options(
        modes: &[protocol::config::SessionMode],
        models: &[protocol::ModelInfo],
    ) -> Vec<SessionConfigOption> {
        let mut options = Vec::new();

        if let Some(default_mode) = modes.first() {
            let select_options = modes
                .iter()
                .map(|mode| {
                    SessionConfigSelectOption::new(mode.id.clone(), mode.name.clone())
                        .description(mode.description.clone())
                })
                .collect::<Vec<_>>();

            let default_value = SessionConfigValueId::new(default_mode.id.clone());
            options.push(
                SessionConfigOption::select(
                    SessionConfigId::new("mode"),
                    "Mode",
                    default_value,
                    select_options,
                )
                .category(SessionConfigOptionCategory::Mode),
            );
        }

        if let Some(default_model) = models.first() {
            let select_options = models
                .iter()
                .map(|model| {
                    SessionConfigSelectOption::new(model.id.clone(), model.display_name.clone())
                        .description(model.description.clone())
                })
                .collect::<Vec<_>>();

            options.push(
                SessionConfigOption::select(
                    SessionConfigId::new("model"),
                    "Model",
                    SessionConfigValueId::new(default_model.id.clone()),
                    select_options,
                )
                .category(SessionConfigOptionCategory::Model),
            );
        }

        options
    }

    /// Return a clone of the current session config option snapshot.
    fn session_config_snapshot(
        &self,
        session_id: &protocol::SessionId,
    ) -> Option<Vec<SessionConfigOption>> {
        self.session_configs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    /// Update the current value for a single session configuration option in-memory.
    fn set_session_config_current_value(
        &self,
        session_id: &protocol::SessionId,
        config_id: &str,
        value: &str,
    ) -> bool {
        let mut sessions = self
            .session_configs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let options = match sessions.get_mut(session_id) {
            Some(configs) => configs,
            None => return false,
        };

        for option in options.iter_mut() {
            if option.id.0.as_ref() != config_id {
                continue;
            }

            if let SessionConfigKind::Select(select) = &mut option.kind {
                let requested = SessionConfigValueId::new(value);
                if !self.session_config_select_contains_value(&select.options, &requested) {
                    return false;
                }

                select.current_value = requested;
                return true;
            }
        }

        false
    }

    /// Check if a session config option exists and exposes a specific selectable value.
    fn has_session_config_value(
        &self,
        session_id: &protocol::SessionId,
        config_id: &str,
        value: &str,
    ) -> bool {
        let sessions = self
            .session_configs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let Some(options) = sessions.get(session_id) else {
            return false;
        };

        let requested = SessionConfigValueId::new(value);
        options.iter().any(|option| {
            option.id.0.as_ref() == config_id
                && match &option.kind {
                    SessionConfigKind::Select(select) => {
                        self.session_config_select_contains_value(&select.options, &requested)
                    }
                    _ => false,
                }
        })
    }

    fn session_config_select_contains_value(
        &self,
        options: &SessionConfigSelectOptions,
        value: &SessionConfigValueId,
    ) -> bool {
        match options {
            SessionConfigSelectOptions::Ungrouped(values) => {
                values.iter().any(|candidate| candidate.value == *value)
            }
            SessionConfigSelectOptions::Grouped(groups) => {
                groups.iter().any(|group: &SessionConfigSelectGroup| {
                    group
                        .options
                        .iter()
                        .any(|candidate| candidate.value == *value)
                })
            }
            _ => false,
        }
    }

    /// Clear all cached configuration snapshots for a closed session.
    fn clear_session_configs(&self, session_id: &protocol::SessionId) {
        self.session_configs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
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

    /// Convert an apply_patch argument preview into an ACP edit tool update.
    fn patch_apply_updated_to_acp(
        call_id: String,
        changes: Vec<protocol::PatchPreviewChange>,
    ) -> SessionUpdate {
        let content = changes
            .into_iter()
            .map(Self::patch_preview_change_to_content)
            .collect::<Vec<_>>();
        let fields = ToolCallUpdateFields::new()
            .kind(ToolKind::Edit)
            .status(AcpToolCallStatus::InProgress)
            .title("Apply patch")
            .content(content);
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new(call_id), fields))
    }

    /// Convert one patch preview change into ACP tool-call content.
    fn patch_preview_change_to_content(change: protocol::PatchPreviewChange) -> ToolCallContent {
        match change {
            protocol::PatchPreviewChange::Add { path, content } => {
                ToolCallContent::Diff(Diff::new(path, content))
            }
            protocol::PatchPreviewChange::Delete { path } => {
                ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new(
                    format!("Delete file: {}", path.display()),
                ))))
            }
            protocol::PatchPreviewChange::Update {
                path,
                move_path,
                old_text,
                new_text,
            } => ToolCallContent::Diff(
                Diff::new(move_path.unwrap_or(path), new_text).old_text(old_text),
            ),
        }
    }

    /// Replay restored history to the ACP client as message chunks.
    async fn replay_history(
        session_id: &AcpSessionId,
        history: &[Message],
        usage: Option<Usage>,
        cx: &ConnectionTo<Client>,
    ) -> Result<(), Error> {
        if history.is_empty() && usage.is_none() {
            return Ok(());
        }

        for message in history {
            for update in Self::history_replay_updates(message) {
                cx.send_notification(SessionNotification::new(session_id.clone(), update))?;
            }
        }

        if let Some(usage) = usage {
            // ACP status follows the live event path and displays input + output tokens.
            cx.send_notification(SessionNotification::new(
                session_id.clone(),
                SessionUpdate::UsageUpdate(UsageUpdate::new(usage.display_tokens(), 0)),
            ))?;
        }

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
                    async move |request: SetSessionConfigOptionRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        let agent = agent.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent.handle_set_session_config_option(request).await,
                            )
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
            .prompt_capabilities(
                PromptCapabilities::new()
                    .embedded_context(true)
                    .image(false),
            )
            .mcp_capabilities(McpCapabilities::new().http(true))
            .load_session(true);

        caps.session_capabilities = SessionCapabilities::new()
            .close(SessionCloseCapabilities::new())
            .list(SessionListCapabilities::new());

        Ok(InitializeResponse::new(protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                Implementation::new("claw-acp", env!("CARGO_PKG_VERSION")).title("Clawcode"),
            ))
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

        let acp_session_id = created.acp_session_id();
        let mode_state = created.acp_mode_state();
        let model_state = created.acp_model_state();
        let root_session_id = created.session_id.clone();

        let config_options = self.set_session_config_defaults(
            created.session_id.clone(),
            &created.modes,
            &created.models,
        );
        self.fs_router.register_session(
            created.session_id.clone(),
            cx.clone(),
            self.client_capabilities_snapshot(),
        );
        self.terminal_router.register_session(
            created.session_id,
            cx.clone(),
            self.client_capabilities_snapshot(),
        );
        // Send available slash commands after the response is delivered so the
        // client has had time to register the session (Zed discard notifications
        // for unknown sessions).
        let cx_for_cmds = cx.clone();
        let sid = acp_session_id.clone();
        let acp_snapshot_session_id = acp_session_id.clone();
        let kernel = Arc::clone(&self.kernel);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = Self::send_available_commands(&sid, &cx_for_cmds);
            if let Ok(snapshot) = kernel.agent_ui_snapshot(&root_session_id).await {
                let update =
                    Self::subagent_metadata_update(protocol::AgentUiEventKind::Snapshot, snapshot);
                let _ = cx_for_cmds
                    .send_notification(SessionNotification::new(acp_snapshot_session_id, update));
            }
        });
        Ok(NewSessionResponse::new(acp_session_id)
            .modes(mode_state)
            .models(model_state)
            .config_options(config_options))
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
        let session_id = SessionId::from(session_id);
        let cwd = Self::resolve_request_cwd(cwd)?;
        let options = Self::launch_options_from_mcp_servers(mcp_servers, &cwd)?;
        let created = self
            .kernel
            .load_session(&session_id, cwd, options)
            .await
            .map_err(|error| Error::internal_error().data(error.to_string()))?;

        let acp_session_id = created.acp_session_id();
        let root_session_id = created.session_id.clone();
        let config_options = self.set_session_config_defaults(
            created.session_id.clone(),
            &created.modes,
            &created.models,
        );
        Self::replay_history(
            &acp_session_id,
            &created.history,
            created.history_usage,
            &cx,
        )
        .await?;

        let model_state = created.acp_model_state();
        let mode_state = created.acp_mode_state();

        self.fs_router.register_session(
            created.session_id.clone(),
            cx.clone(),
            self.client_capabilities_snapshot(),
        );
        self.terminal_router.register_session(
            created.session_id,
            cx.clone(),
            self.client_capabilities_snapshot(),
        );

        // Send available slash commands after the response is delivered so the
        // client has had time to register the session (Zed discard notifications
        // for unknown sessions).
        let cx_for_cmds = cx.clone();
        let sid = acp_session_id.clone();
        let acp_snapshot_session_id = acp_session_id.clone();
        let kernel = Arc::clone(&self.kernel);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = Self::send_available_commands(&sid, &cx_for_cmds);
            if let Ok(snapshot) = kernel.agent_ui_snapshot(&root_session_id).await {
                let update =
                    Self::subagent_metadata_update(protocol::AgentUiEventKind::Snapshot, snapshot);
                let _ = cx_for_cmds
                    .send_notification(SessionNotification::new(acp_snapshot_session_id, update));
            }
        });

        Ok(LoadSessionResponse::new()
            .modes(mode_state)
            .models(model_state)
            .config_options(config_options))
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
                SessionInfo::new(session.session_id, session.cwd)
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
        let prompt_session_id = SessionId::from(request.session_id);

        // Keep supported prompt blocks from ACP and forward them as plain kernel text.
        let text = request
            .prompt
            .into_iter()
            .filter_map(Self::prompt_block_to_text)
            .collect::<Vec<_>>()
            .join("\n");

        if text.is_empty() {
            return Err(Error::invalid_params().data("prompt must include a text block"));
        }

        let acp_sid: AcpSessionId = (&prompt_session_id).into();

        // Call kernel and get event stream
        let mut events = self
            .kernel
            .prompt(&prompt_session_id, text)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        let mut tool_names = HashMap::<String, String>::new();

        // Translate events to ACP notifications
        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::internal_error().data(e.to_string()))?;
            match event {
                Event::AgentMessageChunk { session_id, text } => {
                    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                    let update = SessionUpdate::AgentMessageChunk(chunk);
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::AgentThoughtChunk { session_id, text } => {
                    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                    let update = SessionUpdate::AgentThoughtChunk(chunk);
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::ToolCall {
                    session_id,
                    call_id,
                    name,
                    arguments,
                    status,
                    ..
                } => {
                    tool_names.insert(call_id.clone(), name.clone());
                    let acp_status: AcpToolCallStatus = status.into();
                    let tool_call = ToolCall::new(ToolCallId::new(call_id), name)
                        .status(acp_status)
                        .raw_input(arguments);
                    let update = SessionUpdate::ToolCall(tool_call);
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::ToolCallUpdate {
                    session_id,
                    call_id,
                    output_delta,
                    status,
                } => {
                    let completed = status == Some(protocol::ToolCallStatus::Completed);
                    let completed_tool_name = if completed {
                        tool_names.get(&call_id).cloned()
                    } else {
                        None
                    };
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
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                    if matches!(
                        completed_tool_name.as_deref(),
                        Some("spawn_agent" | "close_agent")
                    ) {
                        // Agent tools mutate the registry, so refresh the UI list from the
                        // authoritative kernel state instead of parsing model-visible tool output.
                        if let Ok(snapshot) =
                            self.kernel.agent_ui_snapshot(&prompt_session_id).await
                        {
                            let update = Self::subagent_metadata_update(
                                protocol::AgentUiEventKind::Snapshot,
                                snapshot,
                            );
                            let _ = cx.send_notification(SessionNotification::new(
                                acp_sid.clone(),
                                update,
                            ));
                        }
                    }
                }
                Event::PatchApplyUpdated {
                    session_id,
                    call_id,
                    changes,
                } => {
                    let update = Self::patch_apply_updated_to_acp(call_id, changes);
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::ExecCommandOutputDelta {
                    session_id,
                    call_id,
                    chunk,
                    ..
                } => {
                    // Forward shell output chunks verbatim; the kernel event
                    // already carries stdout/stderr metadata for non-text clients.
                    let decoded = String::from_utf8_lossy(&chunk);
                    if !decoded.is_empty() {
                        let mut fields = ToolCallUpdateFields::default();
                        fields.content = Some(vec![ToolCallContent::Content(Content::new(
                            ContentBlock::Text(TextContent::new(decoded.into_owned())),
                        ))]);
                        fields.status = Some(AcpToolCallStatus::InProgress);
                        let update_val = ToolCallUpdate::new(ToolCallId::new(call_id), fields);
                        let update = SessionUpdate::ToolCallUpdate(update_val);
                        let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                    }
                }
                Event::ItemStarted {
                    session_id, item, ..
                } => {
                    if let Some(update) = item.start() {
                        let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                    }
                }
                Event::ItemCompleted {
                    session_id, item, ..
                } => {
                    if let Some(update) = item.end() {
                        let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                    }
                }
                Event::PlanUpdate {
                    session_id,
                    entries,
                } => {
                    let plan_entries: Vec<PlanEntry> = entries
                        .into_iter()
                        .map(|e| PlanEntry::new(e.name, e.priority.into(), e.status.into()))
                        .collect();
                    let update = SessionUpdate::Plan(Plan::new(plan_entries));
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::UsageUpdate {
                    session_id,
                    input_tokens,
                    output_tokens,
                } => {
                    let usage = UsageUpdate::new(input_tokens + output_tokens, 0);
                    let update = SessionUpdate::UsageUpdate(usage);
                    let _ = cx.send_notification(SessionNotification::new(&session_id, update));
                }
                Event::ExecApprovalRequested {
                    session_id,
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
                        &session_id,
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
                Event::AgentSpawned {
                    session_id,
                    agent_path,
                    agent_nickname,
                    agent_role,
                } => {
                    // Spawned agents run inside kernel-created sessions, so they never pass through
                    // ACP new/load session handlers before their tools execute. Register the current
                    // client route immediately so child-session fs/terminal tools can delegate to it.
                    self.fs_router.register_session(
                        session_id.clone(),
                        cx.clone(),
                        self.client_capabilities_snapshot(),
                    );
                    self.terminal_router.register_session(
                        session_id.clone(),
                        cx.clone(),
                        self.client_capabilities_snapshot(),
                    );
                    let metadata = protocol::AgentUiMetadata::builder()
                        .session_id(session_id)
                        .parent_session_id(prompt_session_id.clone())
                        .agent_path(agent_path)
                        .nickname(agent_nickname)
                        .role(agent_role)
                        .status(protocol::AgentStatus::Running)
                        .is_root(false)
                        .build();
                    let update = Self::subagent_metadata_update(
                        protocol::AgentUiEventKind::Upsert,
                        vec![metadata],
                    );
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
                }
                Event::AgentStatusChange {
                    session_id,
                    agent_path,
                    status,
                } => {
                    let metadata = protocol::AgentUiMetadata::builder()
                        .session_id(session_id)
                        .agent_path(agent_path)
                        .status(status)
                        .is_root(false)
                        .build();
                    let update = Self::subagent_metadata_update(
                        protocol::AgentUiEventKind::Status,
                        vec![metadata],
                    );
                    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
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

    /// Convert supported ACP prompt blocks into the kernel text format.
    fn prompt_block_to_text(block: ContentBlock) -> Option<String> {
        match block {
            ContentBlock::Text(t) => Some(t.text),
            ContentBlock::ResourceLink(link) => {
                Some(Self::format_uri_as_link(Some(link.name), link.uri))
            }
            ContentBlock::Resource(EmbeddedResource {
                resource:
                    EmbeddedResourceResource::TextResourceContents(TextResourceContents {
                        text,
                        uri,
                        ..
                    }),
                ..
            }) => Some(format!(
                "{}\n<context ref=\"{uri}\">\n{text}\n</context>",
                Self::format_uri_as_link(None, uri.clone())
            )),
            // Audio and image blocks are intentionally unsupported in this iteration.
            // Skip them so callers can still send mixed prompts with plain text/link.
            ContentBlock::Audio(_) | ContentBlock::Image(_) => None,
            // Skip unsupported embedded content formats to keep the behavior stable.
            ContentBlock::Resource(..) | _ => None,
        }
    }

    /// Render a resource link as the mention-style syntax expected by the kernel.
    fn format_uri_as_link(name: Option<String>, uri: String) -> String {
        if let Some(name) = name
            && !name.is_empty()
        {
            format!("[@{name}]({uri})")
        } else if let Some(path) = uri.strip_prefix("file://") {
            let name = path.split('/').next_back().unwrap_or(path);
            format!("[@{name}]({uri})")
        } else if uri.starts_with("zed://") {
            let name = uri.split('/').next_back().unwrap_or(&uri);
            format!("[@{name}]({uri})")
        } else {
            uri
        }
    }

    async fn handle_cancel(&self, notification: CancelNotification) -> Result<(), Error> {
        let session_id = SessionId::from(notification.session_id);
        self.kernel
            .cancel(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))
    }

    /// Send the `AvailableCommandsUpdate` notification for the given ACP session.
    fn send_available_commands(
        session_id: &AcpSessionId,
        cx: &ConnectionTo<Client>,
    ) -> Result<(), Error> {
        let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
            AvailableCommand::new("sessions", "list recent sessions"),
        ]));
        cx.send_notification(SessionNotification::new(session_id.clone(), update))
    }

    async fn handle_set_mode(
        &self,
        request: SetSessionModeRequest,
    ) -> Result<SetSessionModeResponse, Error> {
        let session_id = SessionId::from(request.session_id);
        self.kernel
            .set_mode(&session_id, &request.mode_id.0)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;

        let _ = self.set_session_config_current_value(&session_id, "mode", &request.mode_id.0);
        Ok(SetSessionModeResponse::default())
    }

    async fn handle_set_model(
        &self,
        request: SetSessionModelRequest,
    ) -> Result<SetSessionModelResponse, Error> {
        let session_id = SessionId::from(request.session_id);
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

        let _ = self.set_session_config_current_value(&session_id, "model", &request.model_id.0);
        Ok(SetSessionModelResponse::default())
    }

    async fn handle_set_session_config_option(
        &self,
        request: SetSessionConfigOptionRequest,
    ) -> Result<SetSessionConfigOptionResponse, Error> {
        let session_id = SessionId::from(request.session_id.clone());
        let config_id = request.config_id.0.as_ref();
        let requested = match request.value {
            SessionConfigOptionValue::ValueId { value } => value.to_string(),
            SessionConfigOptionValue::Boolean { .. } => {
                return Err(Error::invalid_params()
                    .data("session config option value type is not supported"));
            }
            _ => {
                return Err(Error::invalid_params()
                    .data("session config option value type is not supported"));
            }
        };

        if self.session_config_snapshot(&session_id).is_none() {
            return Err(Error::resource_not_found(Some(format!(
                "session not found: {}",
                request.session_id.0.as_ref()
            ))));
        }

        if !self.has_session_config_value(&session_id, config_id, requested.as_str()) {
            return Err(
                Error::invalid_params().data("session config option or value is not supported")
            );
        }

        match config_id {
            "mode" => {
                self.kernel
                    .set_mode(&session_id, &requested)
                    .await
                    .map_err(|e| Error::internal_error().data(e.to_string()))?;
            }
            "model" => {
                let mut parts = requested.splitn(2, '/');
                let provider_id = parts.next().unwrap_or("");
                let model_id = parts.next().unwrap_or(requested.as_str());

                self.kernel
                    .set_model(&session_id, provider_id, model_id)
                    .await
                    .map_err(|e| Error::internal_error().data(e.to_string()))?;
            }
            _ => {
                return Err(Error::invalid_params().data("unsupported session config option"));
            }
        }

        let _ = self.set_session_config_current_value(&session_id, config_id, requested.as_str());

        let updated = self.session_config_snapshot(&session_id).ok_or_else(|| {
            Error::resource_not_found(Some(format!(
                "session not found: {}",
                request.session_id.0.as_ref()
            )))
        })?;

        Ok(SetSessionConfigOptionResponse::new(updated))
    }

    async fn handle_close_session(
        &self,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, Error> {
        let session_id = SessionId::from(request.session_id);
        self.kernel
            .close_session(&session_id)
            .await
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        self.clear_session_configs(&session_id);
        self.fs_router.unregister_session(&session_id);
        self.terminal_router.unregister_session(&session_id);
        Ok(CloseSessionResponse::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::Responder;
    use async_trait::async_trait;
    use futures::stream;
    use protocol::mcp::McpTransportConfig;
    use protocol::{
        AgentPath, EventStream, KernelError, ModelInfo, OneOrMany, ReviewDecision, SessionCreated,
        SessionId, SessionInfo, SessionLaunchOptions, SessionListPage, SessionMode, Text,
        ToolFunction, Usage,
    };
    use std::path::{Path, PathBuf};
    use tools::{FsBackend, FsReadRequest};

    #[derive(Default, typed_builder::TypedBuilder)]
    struct RecordingKernel {
        /// New-session options captured by ACP handler tests.
        #[builder(default)]
        new_session_options: std::sync::Mutex<Option<SessionLaunchOptions>>,
        /// Load-session options captured by ACP handler tests.
        #[builder(default)]
        load_session_options: std::sync::Mutex<Option<SessionLaunchOptions>>,
        /// Events returned by the fake prompt stream in ACP routing tests.
        #[builder(default)]
        prompt_events: std::sync::Mutex<Vec<Event>>,
        /// Mode changes captured by ACP config tests.
        #[builder(default)]
        set_mode_calls: std::sync::Mutex<Vec<String>>,
        /// Model changes captured by ACP config tests.
        #[builder(default)]
        set_model_calls: std::sync::Mutex<Vec<(String, String)>>,
        /// Available modes returned by the fake kernel.
        #[builder(default)]
        available_modes: Vec<SessionMode>,
        /// Available models returned by the fake kernel.
        #[builder(default)]
        available_models: Vec<ModelInfo>,
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
            Ok(session_created(
                self.available_modes.clone(),
                self.available_models.clone(),
            ))
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
            Ok(session_created(
                self.available_modes.clone(),
                self.available_models.clone(),
            ))
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

        /// Return the configured event stream for ACP prompt routing tests.
        async fn prompt(
            &self,
            _session_id: &SessionId,
            _text: String,
        ) -> Result<EventStream, KernelError> {
            let events = self
                .prompt_events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }

        /// Accept cancellation in the fake kernel.
        async fn cancel(&self, _session_id: &SessionId) -> Result<(), KernelError> {
            Ok(())
        }

        /// Accept mode changes in the fake kernel.
        async fn set_mode(&self, _session_id: &SessionId, _mode: &str) -> Result<(), KernelError> {
            self.set_mode_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(_mode.to_string());
            Ok(())
        }

        /// Accept model changes in the fake kernel.
        async fn set_model(
            &self,
            _session_id: &SessionId,
            _provider_id: &str,
            _model_id: &str,
        ) -> Result<(), KernelError> {
            self.set_model_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((_provider_id.to_string(), _model_id.to_string()));
            Ok(())
        }

        /// Accept session close in the fake kernel.
        async fn close_session(&self, _session_id: &SessionId) -> Result<(), KernelError> {
            Ok(())
        }

        /// Return a root-only agent UI snapshot for ACP handler tests.
        async fn agent_ui_snapshot(
            &self,
            root_session_id: &SessionId,
        ) -> Result<Vec<protocol::AgentUiMetadata>, KernelError> {
            Ok(vec![
                protocol::AgentUiMetadata::builder()
                    .session_id(root_session_id.clone())
                    .agent_path(protocol::AgentPath::root())
                    .status(protocol::AgentStatus::Running)
                    .is_root(true)
                    .build(),
            ])
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
            self.available_modes.clone()
        }

        /// Return no models; ACP conversion has fallbacks for empty state.
        fn available_models(&self) -> Vec<ModelInfo> {
            self.available_models.clone()
        }
    }

    fn kernel_with_configs(
        modes: Vec<SessionMode>,
        models: Vec<ModelInfo>,
    ) -> Arc<RecordingKernel> {
        Arc::new(
            RecordingKernel::builder()
                .available_modes(modes)
                .available_models(models)
                .build(),
        )
    }

    /// Create a fake kernel that returns the provided prompt events.
    fn kernel_with_prompt_events(events: Vec<Event>) -> Arc<RecordingKernel> {
        Arc::new(
            RecordingKernel::builder()
                .prompt_events(std::sync::Mutex::new(events))
                .build(),
        )
    }

    fn default_config_modes() -> Vec<SessionMode> {
        vec![SessionMode {
            id: "auto".to_string(),
            name: "Auto".to_string(),
            description: Some("Default mode".to_string()),
        }]
    }

    fn default_config_models() -> Vec<ModelInfo> {
        vec![
            ModelInfo::builder()
                .id("deepseek/deepseek-chat".to_string())
                .display_name("DeepSeek Chat".to_string())
                .build(),
        ]
    }

    /// Build a session-created response for ACP handler tests.
    fn session_created(modes: Vec<SessionMode>, models: Vec<ModelInfo>) -> SessionCreated {
        SessionCreated::builder()
            .session_id(protocol::SessionId::from("session-1"))
            .current_model(
                models
                    .first()
                    .map(|model| model.id.clone())
                    .unwrap_or_default(),
            )
            .modes(modes)
            .models(models)
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

    /// Create an ACP connection handle and collect client notifications.
    async fn test_connection_to_client_recording_notifications() -> (
        ConnectionTo<Client>,
        tokio::sync::mpsc::UnboundedReceiver<SessionNotification>,
    ) {
        let (agent_channel, client_channel) = acp::Channel::duplex();
        let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();
        let (notification_tx, notification_rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn(async move {
            let _ = Agent
                .builder()
                .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                    let _ = connection_tx.send(cx);
                    std::future::pending::<Result<(), Error>>().await
                })
                .await;
        });

        tokio::spawn(async move {
            let _ = Client
                .builder()
                .on_receive_notification(
                    move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                        let tx = notification_tx.clone();
                        async move {
                            let _ = tx.send(notification);
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(client_channel, async move |_cx| {
                    std::future::pending::<Result<(), Error>>().await
                })
                .await;
        });

        (
            connection_rx
                .await
                .expect("agent connection should be created"),
            notification_rx,
        )
    }

    /// Create an ACP connection handle whose client side supports fs/read_text_file.
    async fn test_connection_to_client_with_read_handler() -> ConnectionTo<Client> {
        let (agent_channel, client_channel) = acp::Channel::duplex();
        let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let _ = Agent
                .builder()
                .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                    let _ = connection_tx.send(cx);
                    std::future::pending::<Result<(), Error>>().await
                })
                .await;
        });

        tokio::spawn(async move {
            let _ = Client
                .builder()
                .on_receive_request(
                    move |_request: ReadTextFileRequest,
                          responder: Responder<ReadTextFileResponse>,
                          _cx| async move {
                        responder.respond(ReadTextFileResponse::new("from subagent route"))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(client_channel, async move |_cx| {
                    std::future::pending::<Result<(), Error>>().await
                })
                .await;
        });

        connection_rx
            .await
            .expect("agent connection should be created")
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
        let kernel = Arc::new(RecordingKernel::builder().build());
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
    async fn new_session_registers_initial_config_options() {
        let kernel = kernel_with_configs(default_config_modes(), default_config_models());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = NewSessionRequest::new(PathBuf::from("/tmp"));
        let client = test_connection_to_client().await;

        let response = agent
            .handle_new_session(request, client)
            .await
            .expect("new session should include config options");

        let config_options = response
            .config_options
            .expect("new session should return session config options");
        assert_eq!(config_options.len(), 2);

        let mode = config_options
            .iter()
            .find(|option| option.id.0.as_ref() == "mode")
            .expect("mode config should exist");
        let SessionConfigKind::Select(mode_select) = &mode.kind else {
            panic!("expected mode to use select config kind");
        };
        assert_eq!(mode_select.current_value.0.as_ref(), "auto");

        let model = config_options
            .iter()
            .find(|option| option.id.0.as_ref() == "model")
            .expect("model config should exist");
        let SessionConfigKind::Select(model_select) = &model.kind else {
            panic!("expected model to use select config kind");
        };
        assert_eq!(
            model_select.current_value.0.as_ref(),
            "deepseek/deepseek-chat"
        );
    }

    #[tokio::test]
    async fn set_session_config_option_updates_mode_and_kernel() {
        let kernel = kernel_with_configs(default_config_modes(), default_config_models());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = NewSessionRequest::new(PathBuf::from("/tmp"));
        let client = test_connection_to_client().await;
        let response = agent
            .handle_new_session(request, client)
            .await
            .expect("new session should succeed");

        let set = SetSessionConfigOptionRequest::new(response.session_id.clone(), "mode", "auto");
        let updated = agent
            .handle_set_session_config_option(set)
            .await
            .expect("set_session_config_option should succeed");

        let mode = updated
            .config_options
            .into_iter()
            .find(|option| option.id.0.as_ref() == "mode")
            .expect("mode should still be present");

        let SessionConfigKind::Select(mode_select) = mode.kind else {
            panic!("expected mode to use select config kind");
        };
        assert_eq!(mode_select.current_value.0.as_ref(), "auto");

        assert_eq!(
            kernel
                .set_mode_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            ["auto"].as_ref()
        );
    }

    #[tokio::test]
    async fn set_session_config_option_rejects_unsupported_config_id() {
        let kernel = kernel_with_configs(default_config_modes(), default_config_models());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = NewSessionRequest::new(PathBuf::from("/tmp"));
        let client = test_connection_to_client().await;
        let response = agent
            .handle_new_session(request, client)
            .await
            .expect("new session should succeed");

        let set =
            SetSessionConfigOptionRequest::new(response.session_id.clone(), "unsupported", "x");

        agent
            .handle_set_session_config_option(set)
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn set_session_config_option_rejects_unsupported_value() {
        let kernel = kernel_with_configs(default_config_modes(), default_config_models());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = NewSessionRequest::new(PathBuf::from("/tmp"));
        let client = test_connection_to_client().await;
        let response = agent
            .handle_new_session(request, client)
            .await
            .expect("new session should succeed");

        let set =
            SetSessionConfigOptionRequest::new(response.session_id.clone(), "mode", "unknown");

        agent
            .handle_set_session_config_option(set)
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn handle_prompt_accepts_text_with_other_blocks() {
        let kernel = Arc::new(RecordingKernel::builder().build());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = PromptRequest::new(
            AcpSessionId::new("session-1"),
            vec![
                ContentBlock::Text(TextContent::new("hello".to_string())),
                ContentBlock::Image(ImageContent::new("image-data", "image/png")),
            ],
        );

        let client = test_connection_to_client().await;
        let result = agent.handle_prompt(request, client).await;

        result.unwrap();
    }

    #[tokio::test]
    async fn handle_prompt_routes_stream_updates_to_event_session() {
        let child_session = SessionId::from("child-session");
        let kernel = kernel_with_prompt_events(vec![
            Event::message_chunk(child_session.clone(), "child context"),
            Event::TurnComplete {
                session_id: child_session.clone(),
                stop_reason: protocol::StopReason::EndTurn,
            },
        ]);
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = PromptRequest::new(
            AcpSessionId::new("root-session"),
            vec![ContentBlock::Text(TextContent::new("hello".to_string()))],
        );
        let (client, mut notifications) = test_connection_to_client_recording_notifications().await;

        agent
            .handle_prompt(request, client)
            .await
            .expect("prompt should finish");

        let notification =
            tokio::time::timeout(std::time::Duration::from_secs(1), notifications.recv())
                .await
                .expect("notification should be delivered")
                .expect("notification channel should stay open");

        assert_eq!(notification.session_id.0.as_ref(), child_session.0.as_ref());
        let SessionUpdate::AgentMessageChunk(chunk) = notification.update else {
            panic!("expected child message chunk");
        };
        let ContentBlock::Text(text) = chunk.content else {
            panic!("expected text chunk");
        };
        assert_eq!(text.text, "child context");
    }

    #[tokio::test]
    async fn handle_prompt_registers_spawned_agent_for_client_fs_routes() {
        let child_session = SessionId::from("child-session");
        let fs_router = Arc::new(AcpClientFsRouter::default());
        let kernel = kernel_with_prompt_events(vec![
            Event::agent_spawned(
                child_session.clone(),
                AgentPath::root().join("analyze_cargo"),
                "Bacon",
                "default",
            ),
            Event::TurnComplete {
                session_id: SessionId::from("root-session"),
                stop_reason: protocol::StopReason::EndTurn,
            },
        ]);
        let agent = ClawcodeAgent::with_fs_router(
            Arc::clone(&kernel) as Arc<dyn AgentKernel>,
            Arc::clone(&fs_router),
        );
        agent
            .handle_initialize(
                InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                    ClientCapabilities::new()
                        .fs(FileSystemCapabilities::new().read_text_file(true)),
                ),
            )
            .await
            .expect("initialize should record fs capabilities");
        let client = test_connection_to_client_with_read_handler().await;

        agent
            .handle_prompt(
                PromptRequest::new(
                    AcpSessionId::new("root-session"),
                    vec![ContentBlock::Text(TextContent::new("spawn".to_string()))],
                ),
                client,
            )
            .await
            .expect("prompt should finish");

        let backend = crate::backend::fs::AcpFsBackend::new(fs_router);
        let response = backend
            .read_text_file(
                FsReadRequest::builder()
                    .session_id(child_session)
                    .cwd(PathBuf::from("/workspace"))
                    .path(PathBuf::from("Cargo.toml"))
                    .offset(0)
                    .limit(1)
                    .build(),
            )
            .await
            .expect("spawned agent session should reuse the ACP client fs route");

        assert_eq!(response.content, "from subagent route");
    }

    #[tokio::test]
    async fn handle_prompt_accepts_resource_link() {
        let kernel = Arc::new(RecordingKernel::builder().build());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = PromptRequest::new(
            AcpSessionId::new("session-1"),
            vec![ContentBlock::ResourceLink(ResourceLink::new(
                "README".to_string(),
                "file:///tmp/README.md".to_string(),
            ))],
        );

        let client = test_connection_to_client().await;
        let result = agent.handle_prompt(request, client).await;

        result.unwrap();
    }

    #[tokio::test]
    async fn handle_prompt_rejects_non_text_only_content() {
        let kernel = Arc::new(RecordingKernel::builder().build());
        let agent = ClawcodeAgent::new(Arc::clone(&kernel) as Arc<dyn AgentKernel>);
        let request = PromptRequest::new(
            AcpSessionId::new("session-1"),
            vec![ContentBlock::Image(ImageContent::new(
                "image-data",
                "image/png",
            ))],
        );

        let client = test_connection_to_client().await;
        let result = agent.handle_prompt(request, client).await;

        result.unwrap_err();
    }

    #[tokio::test]
    async fn new_session_forwards_acp_mcp_servers_to_kernel_options() {
        let kernel = Arc::new(RecordingKernel::builder().build());
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
        let kernel = Arc::new(RecordingKernel::builder().build());
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

    #[tokio::test]
    async fn replay_history_sends_accumulated_usage_update_after_messages() {
        let session_id = AcpSessionId::new("session-usage");
        let history = vec![Message::user("hello")];
        let usage = Usage {
            input_tokens: 12,
            output_tokens: 8,
            total_tokens: 99,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let (client, mut notifications) = test_connection_to_client_recording_notifications().await;

        ClawcodeAgent::replay_history(&session_id, &history, Some(usage), &client)
            .await
            .expect("history replay should send notifications");

        let first = notifications.recv().await.expect("message notification");
        assert!(matches!(first.update, SessionUpdate::AgentMessageChunk(_)));
        let second = notifications.recv().await.expect("usage notification");
        let SessionUpdate::UsageUpdate(update) = second.update else {
            panic!("expected usage update after replayed messages");
        };
        assert_eq!(update.used, 20);
        match notifications.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            other => panic!("expected no extra replay notifications, got {other:?}"),
        }
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

    /// Verifies subagent metadata is carried on ToolCallUpdate._meta and has no visible content.
    #[test]
    fn subagent_snapshot_update_uses_tool_call_meta_without_visible_content() {
        let root = protocol::AgentUiMetadata::builder()
            .session_id(protocol::SessionId::from("root-session"))
            .agent_path(protocol::AgentPath::root())
            .status(protocol::AgentStatus::Running)
            .is_root(true)
            .build();

        let update = ClawcodeAgent::subagent_metadata_update(
            protocol::AgentUiEventKind::Snapshot,
            vec![root],
        );

        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("subagent metadata should be a ToolCallUpdate");
        };
        assert_eq!(update.tool_call_id.to_string(), "clawcode-subagents");
        assert!(update.fields.content.is_none());
        assert_eq!(
            update.meta.as_ref().unwrap()["clawcode"]["subagents"]["event"],
            "snapshot"
        );
    }

    #[test]
    fn patch_apply_updated_converts_to_acp_diff_update() {
        let update = ClawcodeAgent::patch_apply_updated_to_acp(
            "call-1".to_string(),
            vec![protocol::PatchPreviewChange::Update {
                path: PathBuf::from("src/lib.rs"),
                move_path: None,
                old_text: "fn old() {}\n".to_string(),
                new_text: "fn new() {}\n".to_string(),
            }],
        );

        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected patch preview to become tool call update");
        };

        assert_eq!(update.tool_call_id.to_string(), "call-1");
        assert_eq!(update.fields.status, Some(AcpToolCallStatus::InProgress));
        assert_eq!(update.fields.kind, Some(ToolKind::Edit));

        let content = update
            .fields
            .content
            .as_ref()
            .and_then(|content| content.first())
            .expect("patch preview should include diff content");
        let ToolCallContent::Diff(diff) = content else {
            panic!("expected diff content");
        };

        assert_eq!(diff.path, PathBuf::from("src/lib.rs"));
        assert_eq!(diff.old_text.as_deref(), Some("fn old() {}\n"));
        assert_eq!(diff.new_text, "fn new() {}\n");
    }

    #[test]
    fn patch_apply_updated_converts_delete_preview_to_text_update() {
        let update = ClawcodeAgent::patch_apply_updated_to_acp(
            "call-1".to_string(),
            vec![protocol::PatchPreviewChange::Delete {
                path: PathBuf::from("obsolete.txt"),
            }],
        );

        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected patch preview to become tool call update");
        };

        let content = update
            .fields
            .content
            .as_ref()
            .and_then(|content| content.first())
            .expect("delete preview should include visible content");
        let ToolCallContent::Content(content) = content else {
            panic!("expected text content for delete preview");
        };
        let ContentBlock::Text(text) = &content.content else {
            panic!("expected delete preview text");
        };

        assert_eq!(text.text, "Delete file: obsolete.txt");
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
